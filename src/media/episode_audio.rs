//! Bounded audio preparation while an episode is playing. Cache decoded PCM
//! so arbitrary clips (including clips spanning segment boundaries) need only
//! a local encode, with no second read from the media server.
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tracing::{info, warn};

pub const SEGMENT_MS: i64 = 60_000;
const BYTES_PER_MS: usize = 48 * 2; // 48 kHz, mono, signed 16-bit PCM
const MAX_SEGMENTS: usize = 16; // At most 92 MB of decoded audio
const TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SourceKey {
    pub source: String,
    pub index: Option<u32>,
    pub ordinal: Option<usize>,
}

impl SourceKey {
    pub fn new(source: &str, index: Option<u32>, ordinal: Option<usize>) -> Self {
        Self {
            source: source.into(),
            index: if ordinal.is_some() { None } else { index },
            ordinal,
        }
    }
}

struct Segment {
    pcm: Arc<Vec<u8>>,
    touched: Instant,
}

#[derive(Default)]
pub struct EpisodeAudio {
    segments: Mutex<HashMap<(SourceKey, i64), Segment>>,
}

impl EpisodeAudio {
    pub async fn prepare_window(
        &self,
        source: SourceKey,
        position_ms: i64,
        duration_ms: Option<i64>,
    ) {
        let current = position_ms.max(0) / SEGMENT_MS;
        // First prepare the current minute, then the previous context and two
        // minutes ahead. Seeking/source changes cancel and reprioritize this.
        for segment in [current, current - 1, current + 1, current + 2] {
            if segment < 0 || duration_ms.is_some_and(|end| segment * SEGMENT_MS >= end) {
                continue;
            }
            let key = (source.clone(), segment);
            {
                let mut segments = self.segments.lock().await;
                segments.retain(|_, s| s.touched.elapsed() < TTL);
                if let Some(segment) = segments.get_mut(&key) {
                    segment.touched = Instant::now();
                    continue;
                }
            }
            let started = Instant::now();
            let start_ms = segment * SEGMENT_MS;
            info!(stage = "episode_audio_prepare", start_ms, end_ms = start_ms + SEGMENT_MS,
                audio_index = ?source.index, audio_ordinal = ?source.ordinal, "Preparing episode audio in the background");
            match super::extract_pcm(
                &source.source,
                start_ms,
                start_ms + SEGMENT_MS,
                source.index,
                source.ordinal,
            )
            .await
            {
                Ok(pcm) => {
                    info!(
                        stage = "episode_audio_ready",
                        start_ms,
                        bytes = pcm.len(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "Episode audio segment ready"
                    );
                    let mut segments = self.segments.lock().await;
                    if segments.len() >= MAX_SEGMENTS {
                        if let Some(oldest) = segments
                            .iter()
                            .min_by_key(|(_, s)| s.touched)
                            .map(|(k, _)| k.clone())
                        {
                            segments.remove(&oldest);
                        }
                    }
                    segments.insert(
                        key,
                        Segment {
                            pcm: Arc::new(pcm),
                            touched: Instant::now(),
                        },
                    );
                }
                Err(error) => {
                    warn!(stage = "episode_audio_failed", start_ms, elapsed_ms = started.elapsed().as_millis(), %error,
                        "Background audio preparation failed; card extraction remains available");
                    break;
                }
            }
        }
    }

    pub async fn clip(&self, source: &SourceKey, start_ms: i64, end_ms: i64) -> Option<Vec<u8>> {
        if start_ms < 0 || end_ms <= start_ms || end_ms - start_ms > 5 * SEGMENT_MS {
            return None;
        }
        let mut segments = self.segments.lock().await;
        let first = start_ms / SEGMENT_MS;
        let last = (end_ms - 1) / SEGMENT_MS;
        let mut slices = Vec::new();
        for index in first..=last {
            let segment = segments.get_mut(&(source.clone(), index))?;
            if segment.touched.elapsed() >= TTL {
                return None;
            }
            let from = (start_ms - index * SEGMENT_MS).max(0) as usize * BYTES_PER_MS;
            let to = (end_ms - index * SEGMENT_MS).min(SEGMENT_MS) as usize * BYTES_PER_MS;
            if to > segment.pcm.len() {
                return None;
            }
            segment.touched = Instant::now();
            slices.push((segment.pcm.clone(), from, to));
        }
        drop(segments);
        let mut pcm = Vec::with_capacity((end_ms - start_ms) as usize * BYTES_PER_MS);
        for (segment, from, to) in slices {
            pcm.extend_from_slice(&segment[from..to]);
        }
        Some(pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires ffmpeg; validates real decoding, segment joins, and encoding"]
    async fn prepared_audio_ffmpeg_smoke() {
        use super::super::{AudioCodec, TemporaryMedia, extract_pcm, run_ffmpeg, temp_dir};
        use tokio::process::Command;
        let source =
            TemporaryMedia(temp_dir().join(format!("audio-test-{}.wav", uuid::Uuid::new_v4())));
        let mut command = Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=65",
            "-c:a",
            "pcm_s16le",
        ]);
        command.arg(&source.0);
        run_ffmpeg(command).await.unwrap();
        let source_key = SourceKey::new(source.0.to_str().unwrap(), None, Some(0));
        let cache = crate::media::audio_cache::AudioCache::default();
        cache
            .episode
            .prepare_window(source_key.clone(), 59_000, Some(65_000))
            .await;
        let pcm = cache
            .episode
            .clip(&source_key, 59_950, 60_150)
            .await
            .unwrap();
        let direct = extract_pcm(&source_key.source, 59_950, 60_150, None, Some(0))
            .await
            .unwrap();
        assert_eq!(
            pcm, direct,
            "a clip crossing a prepared segment boundary must contain the same samples"
        );
        // Remove the source: successful encoding now proves the prepared audio
        // path is used, with no hidden re-read from the episode.
        std::fs::remove_file(&source.0).unwrap();
        for codec in [AudioCodec::Mp3, AudioCodec::Aac, AudioCodec::Opus] {
            let audio = cache
                .extract(&source_key.source, 59_950, 60_150, None, Some(0), codec)
                .await
                .unwrap();
            assert!(!audio.is_empty());
        }
    }

    #[tokio::test]
    async fn clips_cross_segment_boundaries_without_gaps_or_wrong_tracks() {
        let cache = EpisodeAudio::default();
        let source = SourceKey::new("episode-a", None, Some(1));
        for index in 0..2 {
            cache.segments.lock().await.insert(
                (source.clone(), index),
                Segment {
                    pcm: Arc::new(vec![index as u8 + 1; SEGMENT_MS as usize * BYTES_PER_MS]),
                    touched: Instant::now(),
                },
            );
        }
        let clip = cache.clip(&source, 59_999, 60_002).await.unwrap();
        assert_eq!(clip.len(), 3 * BYTES_PER_MS);
        assert!(clip[..BYTES_PER_MS].iter().all(|b| *b == 1));
        assert!(clip[BYTES_PER_MS..].iter().all(|b| *b == 2));
        assert!(
            cache
                .clip(&SourceKey::new("episode-b", None, Some(1)), 0, 1)
                .await
                .is_none()
        );
        assert!(
            cache
                .clip(&SourceKey::new("episode-a", None, Some(2)), 0, 1)
                .await
                .is_none()
        );
        assert!(cache.clip(&source, 119_999, 120_001).await.is_none());
        assert!(cache.clip(&source, 0, 60_000).await.is_some());
    }
}
