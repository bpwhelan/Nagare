use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OnceCell, Semaphore};
use tracing::{info, warn};

use crate::config::AudioCodec;

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(10 * 60);

// The source is part of the identity but is never logged: streaming URLs can
// contain credentials. Prefer the actual ffmpeg ordinal over the server index.
#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    source: String,
    start_ms: i64,
    end_ms: i64,
    index: Option<u32>,
    ordinal: Option<usize>,
    codec: &'static str,
}

impl Key {
    fn new(
        source: &str,
        start_ms: i64,
        end_ms: i64,
        index: Option<u32>,
        ordinal: Option<usize>,
        codec: AudioCodec,
    ) -> Self {
        Self {
            source: source.into(),
            start_ms,
            end_ms,
            index: if ordinal.is_some() { None } else { index },
            ordinal,
            codec: codec.as_str(),
        }
    }
}

struct Entry {
    data: Arc<OnceCell<Arc<Vec<u8>>>>,
    touched: Instant,
}

pub struct AudioCache {
    entries: Mutex<HashMap<Key, Entry>>,
    extraction_slots: Semaphore,
    pub episode: super::episode_audio::EpisodeAudio,
}

impl Default for AudioCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            extraction_slots: Semaphore::new(3),
            episode: Default::default(),
        }
    }
}

impl AudioCache {
    pub async fn extract(
        &self,
        source: &str,
        start_ms: i64,
        end_ms: i64,
        index: Option<u32>,
        ordinal: Option<usize>,
        codec: AudioCodec,
    ) -> anyhow::Result<Arc<Vec<u8>>> {
        anyhow::ensure!(
            start_ms >= 0 && end_ms > start_ms,
            "Choose an audio end time after the start time"
        );
        let key = Key::new(source, start_ms, end_ms, index, ordinal, codec);
        self.get_or_extract(key, || async {
            let _permit = self.extraction_slots.acquire().await?;
            let identity = super::episode_audio::SourceKey::new(source, index, ordinal);
            if let Some(pcm) = self.episode.clip(&identity, start_ms, end_ms).await {
                info!(
                    stage = "episode_audio_hit",
                    start_ms, end_ms, "Encoding clip from prepared episode audio"
                );
                return super::encode_pcm(&pcm, codec).await;
            }
            info!(
                stage = "episode_audio_miss",
                start_ms, end_ms, "Requested range is not prepared; extracting directly"
            );
            let (path, bytes) =
                super::extract_audio(source, start_ms, end_ms, index, ordinal, codec).await?;
            super::cleanup_temp_file(&path).await;
            Ok(bytes)
        })
        .await
    }

    async fn get_or_extract<F, Fut>(&self, key: Key, extract: F) -> anyhow::Result<Arc<Vec<u8>>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<u8>>>,
    {
        let started = Instant::now();
        let (cell, cache_status) = {
            let mut entries = self.entries.lock().await;
            // Never evict an extraction while another caller is sharing it.
            entries.retain(|_, e| e.touched.elapsed() < TTL || Arc::strong_count(&e.data) > 1);
            if let Some(entry) = entries.get_mut(&key) {
                entry.touched = Instant::now();
                (
                    entry.data.clone(),
                    if entry.data.initialized() {
                        "hit"
                    } else {
                        "in_flight"
                    },
                )
            } else {
                trim(&mut entries, MAX_ENTRIES - 1, MAX_BYTES);
                let cell = Arc::new(OnceCell::new());
                if entries.len() < MAX_ENTRIES {
                    entries.insert(
                        key.clone(),
                        Entry {
                            data: cell.clone(),
                            touched: Instant::now(),
                        },
                    );
                }
                (cell, "miss")
            }
        };
        info!(stage = "audio_cache", cache_status, start_ms = key.start_ms, end_ms = key.end_ms,
            audio_index = ?key.index, audio_ordinal = ?key.ordinal, codec = key.codec, "Audio requested");
        // Cancellation leaves the cell empty so a later request can retry.
        // Errors are not cached, and no cache lock is held during ffmpeg work.
        let data = cell
            .get_or_try_init(|| async { extract().await.map(Arc::new) })
            .await
            .cloned();
        let mut entries = self.entries.lock().await;
        if data.is_err() {
            if Arc::strong_count(&cell) == 2
                && entries
                    .get(&key)
                    .is_some_and(|e| Arc::ptr_eq(&e.data, &cell))
            {
                entries.remove(&key);
            }
        }
        drop(cell);
        trim(&mut entries, MAX_ENTRIES, MAX_BYTES);
        match &data {
            Ok(bytes) => info!(
                stage = "audio_cache",
                cache_status,
                bytes = bytes.len(),
                elapsed_ms = started.elapsed().as_millis(),
                "Audio ready"
            ),
            Err(error) => {
                warn!(stage = "audio_cache", cache_status, elapsed_ms = started.elapsed().as_millis(), %error, "Audio preparation failed; a later request can retry")
            }
        }
        data
    }
}

fn trim(entries: &mut HashMap<Key, Entry>, max_entries: usize, max_bytes: usize) {
    loop {
        let bytes: usize = entries
            .values()
            .filter_map(|e| e.data.get())
            .map(|d| d.len())
            .sum();
        if entries.len() <= max_entries && bytes <= max_bytes {
            break;
        }
        let oldest = entries
            .iter()
            .filter(|(_, e)| Arc::strong_count(&e.data) == 1)
            .min_by_key(|(_, e)| e.touched)
            .map(|(k, _)| k.clone());
        let Some(oldest) = oldest else {
            break;
        };
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key() -> Key {
        Key::new("media-a", 100, 2000, Some(7), Some(1), AudioCodec::Mp3)
    }

    #[tokio::test]
    async fn preview_and_enhancement_share_one_extraction_and_reuse_bytes() {
        let cache = AudioCache::default();
        let calls = AtomicUsize::new(0);
        let gate = tokio::sync::Notify::new();
        let first = cache.get_or_extract(key(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
            Ok(vec![1, 2, 3])
        });
        let second = async {
            tokio::task::yield_now().await;
            gate.notify_one();
            cache
                .get_or_extract(key(), || async { panic!("duplicate extraction") })
                .await
        };
        let (a, b) = tokio::join!(first, second);
        assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *cache
                .get_or_extract(key(), || async { panic!("cache miss") })
                .await
                .unwrap(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn range_source_track_and_codec_changes_do_not_reuse_audio() {
        let cache = AudioCache::default();
        let original = key();
        let mut keys = vec![original.clone()];
        let mut changed = original.clone();
        changed.source = "media-b".into();
        keys.push(changed);
        let mut changed = original.clone();
        changed.start_ms += 1;
        keys.push(changed);
        let mut changed = original.clone();
        changed.end_ms += 1;
        keys.push(changed);
        let mut changed = original.clone();
        changed.ordinal = Some(2);
        keys.push(changed);
        let mut changed = original.clone();
        changed.codec = "opus";
        keys.push(changed);
        for (i, key) in keys.into_iter().enumerate() {
            assert_eq!(
                *cache
                    .get_or_extract(key, || async { Ok(vec![i as u8]) })
                    .await
                    .unwrap(),
                vec![i as u8]
            );
        }
    }

    #[tokio::test]
    async fn errors_cancellation_expiry_and_capacity_allow_fresh_extraction() {
        let cache = AudioCache::default();
        assert!(
            cache
                .get_or_extract(key(), || async { anyhow::bail!("offline") })
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                cache.get_or_extract(key(), || std::future::pending())
            )
            .await
            .is_err()
        );
        cache
            .get_or_extract(key(), || async { Ok(vec![1]) })
            .await
            .unwrap();
        cache.entries.lock().await.get_mut(&key()).unwrap().touched = Instant::now() - TTL;
        assert_eq!(
            *cache
                .get_or_extract(key(), || async { Ok(vec![2]) })
                .await
                .unwrap(),
            vec![2]
        );
        for i in 0..MAX_ENTRIES + 5 {
            let mut key = key();
            key.start_ms = i as i64;
            cache
                .get_or_extract(key, || async { Ok(vec![3]) })
                .await
                .unwrap();
        }
        let mut entries = cache.entries.lock().await;
        assert!(entries.len() <= MAX_ENTRIES);
        trim(&mut entries, MAX_ENTRIES, 2);
        assert!(entries.len() <= 2);
    }
}
