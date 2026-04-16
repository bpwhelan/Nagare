use crate::config::{Config, MediaServerKind};
use crate::media_server::{MediaServer, ServerMap, Session, SubtitleFormat};
use crate::mining::AppDatabase;
use crate::subtitle::{SubtitleTrack, parse_subtitle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, watch};
use tracing::{debug, info, warn};

/// Represents the current state of the monitored session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub sessions: Vec<SessionSummary>,
    pub active_session_id: Option<String>,
    pub now_playing: Option<NowPlayingState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub server_kind: MediaServerKind,
    pub client: String,
    pub device_name: String,
    pub user_name: Option<String>,
    pub title: Option<String>,
    pub is_target_language: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NowPlayingState {
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub item_id: String,
    pub title: String,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub is_paused: bool,
    pub supports_remote_control: bool,
    pub subtitle_stream_index: Option<u32>,
    pub media_source_id: String,
    pub file_path: Option<String>,
}

/// A snapshot of a previously-watched item, kept so the user can mine it later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub item_id: String,
    pub title: String,
    pub media_source_id: String,
    pub file_path: Option<String>,
    pub duration_ms: Option<i64>,
    pub subtitle_count: usize,
    pub last_position_ms: i64,
    /// Timestamp when we last saw this item playing
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
struct ServerSession {
    kind: MediaServerKind,
    server: Arc<dyn MediaServer>,
    session: Session,
}

pub fn scoped_history_id(kind: MediaServerKind, item_id: &str) -> String {
    format!("{kind}|{item_id}")
}

pub fn scoped_session_id(kind: MediaServerKind, session_id: &str) -> String {
    format!("{kind}|{session_id}")
}

pub fn split_scoped_id(scoped_id: &str) -> Option<(MediaServerKind, &str)> {
    let (kind, raw_id) = scoped_id.split_once('|')?;
    Some((MediaServerKind::parse(kind)?, raw_id))
}

pub struct SessionManager {
    servers: Arc<RwLock<ServerMap>>,
    config: Arc<RwLock<Config>>,
    state: Arc<RwLock<SessionState>>,
    subtitles: Arc<RwLock<Option<SubtitleTrack>>>,
    state_tx: watch::Sender<SessionState>,
    /// User override for which session to track (None = auto-select)
    selected_session_id: Arc<RwLock<Option<String>>>,
    /// Subtitle tracks keyed by item_id, kept forever for history mining
    subtitle_history: Arc<RwLock<HashMap<String, SubtitleTrack>>>,
    /// Metadata for previously-watched items
    history: Arc<RwLock<HashMap<String, HistoryEntry>>>,
    /// Shared persistence layer for config, session state, and mined-note history.
    db: Arc<AppDatabase>,
    /// Prevent overlapping poll cycles when the API forces immediate refreshes.
    poll_lock: Mutex<()>,
    /// Throttle: only persist to SQLite at most once per 30 s during position updates.
    last_save: Arc<Mutex<Instant>>,
}

impl SessionManager {
    pub async fn new(
        servers: Arc<RwLock<ServerMap>>,
        config: Arc<RwLock<Config>>,
        state_tx: watch::Sender<SessionState>,
        data_dir: PathBuf,
        db: Arc<AppDatabase>,
    ) -> anyhow::Result<Self> {
        let initial_state = SessionState {
            sessions: Vec::new(),
            active_session_id: None,
            now_playing: None,
        };

        let (history, subtitle_history) = db
            .load_session_history(
                data_dir.join("history.json"),
                data_dir.join("subtitle_history.json"),
            )
            .await?;

        if !history.is_empty() {
            info!("Loaded {} history entries from SQLite", history.len());
        }

        // Start last_save far in the past so first throttled save fires immediately
        let last_save = Arc::new(Mutex::new(
            Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or_else(Instant::now),
        ));

        Ok(Self {
            servers,
            config,
            state: Arc::new(RwLock::new(initial_state)),
            subtitles: Arc::new(RwLock::new(None)),
            state_tx,
            selected_session_id: Arc::new(RwLock::new(None)),
            subtitle_history: Arc::new(RwLock::new(subtitle_history)),
            history: Arc::new(RwLock::new(history)),
            db,
            poll_lock: Mutex::new(()),
            last_save,
        })
    }

    pub fn subtitles(&self) -> Arc<RwLock<Option<SubtitleTrack>>> {
        self.subtitles.clone()
    }

    pub fn subtitle_history(&self) -> Arc<RwLock<HashMap<String, SubtitleTrack>>> {
        self.subtitle_history.clone()
    }

    pub fn history(&self) -> Arc<RwLock<HashMap<String, HistoryEntry>>> {
        self.history.clone()
    }

    /// Get a reference to the shared server map RwLock.
    pub fn servers(&self) -> Arc<RwLock<ServerMap>> {
        self.servers.clone()
    }

    /// Persist history to SQLite.
    ///
    /// `force = true`  → write immediately (used on new-item events).
    /// `force = false` → write only if more than 30 s have elapsed since the
    ///                   last save (used on high-frequency position updates).
    async fn save_history(&self, force: bool) {
        let mut last = self.last_save.lock().await;
        if !force && last.elapsed() < Duration::from_secs(30) {
            return;
        }

        let history = self.history.read().await.clone();
        let subtitle_history = if force {
            Some(self.subtitle_history.read().await.clone())
        } else {
            None
        };

        if let Err(error) = self
            .db
            .save_session_history(history, subtitle_history)
            .await
        {
            warn!("Failed to persist session history to SQLite: {}", error);
            return;
        }

        *last = Instant::now();
    }

    pub async fn select_session(&self, session_id: Option<String>) {
        let mut sel = self.selected_session_id.write().await;
        *sel = session_id;
    }

    pub async fn poll_once(&self) {
        let _poll_guard = self.poll_lock.lock().await;

        let servers = self.servers.read().await.clone();
        if servers.is_empty() {
            {
                let mut state = self.state.write().await;
                state.sessions.clear();
                state.active_session_id = None;
                state.now_playing = None;
            }
            let mut subs = self.subtitles.write().await;
            *subs = None;
            drop(subs);
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
            return;
        }

        let config = self.config.read().await;
        let target_lang = config.target_language.clone();
        drop(config);

        let mut sessions = Vec::<ServerSession>::new();
        for (kind, server) in servers {
            match server.get_sessions().await {
                Ok(fetched) => {
                    sessions.extend(fetched.into_iter().map(|session| ServerSession {
                        kind,
                        server: server.clone(),
                        session,
                    }));
                }
                Err(e) => {
                    warn!("Failed to fetch {kind} sessions: {}", e);
                }
            }
        }

        // Build session summaries
        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .filter(|s| s.session.now_playing.is_some())
            .map(|s| {
                let np = s.session.now_playing.as_ref().unwrap();
                let is_target = np.has_audio_language(&target_lang);
                SessionSummary {
                    id: scoped_session_id(s.kind, &s.session.id),
                    server_kind: s.kind,
                    client: s.session.client.clone(),
                    device_name: s.session.device_name.clone(),
                    user_name: s.session.user_name.clone(),
                    title: Some(np.display_title()),
                    is_target_language: is_target,
                }
            })
            .collect();

        // Determine active session
        let user_selected = self.selected_session_id.read().await.clone();
        let active_session = if let Some(ref sel_id) = user_selected {
            split_scoped_id(sel_id).and_then(|(kind, raw_id)| {
                sessions
                    .iter()
                    .find(|s| s.kind == kind && s.session.id == raw_id)
                    .cloned()
            })
        } else {
            // Auto-select: prefer target language sessions
            sessions
                .iter()
                .find(|s| {
                    s.session
                        .now_playing
                        .as_ref()
                        .map(|np| np.has_audio_language(&target_lang))
                        .unwrap_or(false)
                })
                .cloned()
        };

        // Collect info we need while holding the lock, then release
        let mut needs_subtitle_load: Option<(String, String, String, ServerSession)> = None; // (history_id, item_id, media_source_id, active session)

        {
            let mut state = self.state.write().await;
            state.sessions = summaries;

            if let Some(active) = active_session.clone() {
                let prev_history_id = state.now_playing.as_ref().map(|np| np.history_id.clone());

                let session = &active.session;
                let np = session.now_playing.as_ref().unwrap();
                let item_id = np.item_id.clone();
                let history_id = scoped_history_id(active.kind, &item_id);
                let media_source_id = np
                    .media_source_id
                    .clone()
                    .unwrap_or_else(|| format!("mediasource_{}", np.item_id));

                state.active_session_id = Some(scoped_session_id(active.kind, &session.id));
                state.now_playing = Some(NowPlayingState {
                    history_id: history_id.clone(),
                    server_kind: active.kind,
                    item_id: item_id.clone(),
                    title: np.display_title(),
                    position_ms: session.position_ms().unwrap_or(0),
                    duration_ms: np.run_time_ticks.map(|t| t / 10_000),
                    is_paused: session.play_state.is_paused,
                    supports_remote_control: session.supports_remote_control,
                    subtitle_stream_index: self.find_subtitle_index(session, &target_lang),
                    media_source_id: media_source_id.clone(),
                    file_path: np.path.clone(),
                });

                if prev_history_id.as_deref() != Some(&history_id) {
                    needs_subtitle_load =
                        Some((history_id, item_id, media_source_id, active.clone()));
                }
            } else {
                state.active_session_id = None;
                state.now_playing = None;
            }
        } // write lock released here

        // Load subtitles outside the lock
        if let Some((history_id, item_id, media_source_id, active)) = needs_subtitle_load {
            let display_title = active
                .session
                .now_playing
                .as_ref()
                .map(|np| np.display_title())
                .unwrap_or_else(|| item_id.clone());
            info!(
                "New item detected: {} ({}) on {}",
                display_title, item_id, active.kind
            );
            self.load_subtitles_for_item(
                &item_id,
                &media_source_id,
                &active.session,
                &target_lang,
                &active.server,
            )
            .await;

            // Update file path and save to history
            let file_path = if let Some(path) = active
                .session
                .now_playing
                .as_ref()
                .and_then(|np| np.path.clone())
            {
                path
            } else if let Ok(item_info) = active
                .server
                .get_item_info(&item_id, active.session.user_id.as_deref())
                .await
            {
                item_info.path.unwrap_or_default()
            } else {
                String::new()
            };

            let file_path = if file_path.is_empty() {
                None
            } else {
                let mut state = self.state.write().await;
                if let Some(ref mut np_state) = state.now_playing {
                    np_state.file_path = Some(file_path.clone());
                }
                Some(file_path)
            };

            // Snapshot into history
            let subs = self.subtitles.read().await;
            let sub_count = subs.as_ref().map(|t| t.lines.len()).unwrap_or(0);
            if let Some(track) = subs.as_ref() {
                self.subtitle_history
                    .write()
                    .await
                    .insert(history_id.clone(), track.clone());
            }
            let state = self.state.read().await;
            let pos = state
                .now_playing
                .as_ref()
                .map(|np| np.position_ms)
                .unwrap_or(0);
            let dur = state.now_playing.as_ref().and_then(|np| np.duration_ms);
            drop(state);

            let entry = HistoryEntry {
                history_id: history_id.clone(),
                server_kind: active.kind,
                item_id: item_id.clone(),
                title: display_title.clone(),
                media_source_id: media_source_id.clone(),
                file_path,
                duration_ms: dur,
                subtitle_count: sub_count,
                last_position_ms: pos,
                last_seen: chrono::Utc::now(),
            };
            self.history.write().await.insert(history_id, entry);

            // Force-save both history metadata and subtitle tracks for this new item
            self.save_history(true).await;
        }

        // Update position in history for the active item
        {
            let state = self.state.read().await;
            if let Some(ref np) = state.now_playing {
                let mut hist = self.history.write().await;
                if let Some(entry) = hist.get_mut(&np.history_id) {
                    entry.last_position_ms = np.position_ms;
                    entry.last_seen = chrono::Utc::now();
                }
            }
        }

        // Throttled save for position updates (at most once every 30 s)
        self.save_history(false).await;

        // Broadcast state update
        let snapshot = self.state.read().await.clone();
        let _ = self.state_tx.send(snapshot);
    }

    pub async fn force_refresh_after_remote_command(
        &self,
        session_id: String,
        target_position_ms: Option<i64>,
        target_paused: Option<bool>,
    ) {
        const ATTEMPTS: usize = 6;

        for attempt in 0..ATTEMPTS {
            self.poll_once().await;

            let snapshot = self.state.read().await.clone();
            if snapshot.active_session_id.as_deref() != Some(session_id.as_str()) {
                break;
            }

            let now_playing = match snapshot.now_playing.as_ref() {
                Some(np) => np,
                None => break,
            };

            let pause_synced = target_paused
                .map(|paused| now_playing.is_paused == paused)
                .unwrap_or(true);
            let position_synced = target_position_ms
                .map(|target| (now_playing.position_ms - target).abs() <= 1_500)
                .unwrap_or(true);

            if pause_synced && position_synced {
                break;
            }

            if attempt + 1 < ATTEMPTS {
                let delay_ms = if attempt < 2 { 75 } else { 150 };
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }

    fn find_subtitle_index(&self, session: &Session, target_lang: &str) -> Option<u32> {
        // If the session already has a subtitle selected, verify it's a text stream
        if let Some(idx) = session.play_state.subtitle_stream_index {
            if idx >= 0 {
                let idx_u32 = idx as u32;
                // Check if it's a text subtitle (image subs return empty VTT)
                let is_text = session
                    .now_playing
                    .as_ref()
                    .and_then(|np| np.media_streams.iter().find(|s| s.index == idx_u32))
                    .map(|s| s.is_text_subtitle_stream)
                    .unwrap_or(true); // assume text if we can't tell
                if is_text {
                    return Some(idx_u32);
                } else {
                    debug!(
                        "Active subtitle stream {} is image-based, searching for text sub",
                        idx_u32
                    );
                }
            }
        }

        // Find a target-language TEXT subtitle
        if let Some(np) = &session.now_playing {
            let target_subs = np.subtitle_streams_for_language(target_lang);
            if let Some(sub) = target_subs.iter().find(|s| s.is_text_subtitle_stream) {
                return Some(sub.index);
            }
            // Log if only image subs are available
            if let Some(sub) = target_subs.first() {
                warn!(
                    "Only image-based subtitle available for '{}' (index {}, codec {:?}) — skipping",
                    target_lang, sub.index, sub.codec
                );
            }
        }

        None
    }

    async fn load_subtitles_for_item(
        &self,
        item_id: &str,
        media_source_id: &str,
        session: &Session,
        target_lang: &str,
        server: &Arc<dyn MediaServer>,
    ) {
        let sub_index = match self.find_subtitle_index(session, target_lang) {
            Some(idx) => idx,
            None => {
                warn!("No subtitle stream found for target language");
                let mut subs = self.subtitles.write().await;
                *subs = None;
                return;
            }
        };

        debug!(
            "Fetching subtitles: item={} msid={} index={}",
            item_id, media_source_id, sub_index
        );

        // Try API first
        match server
            .get_subtitles(item_id, media_source_id, sub_index, SubtitleFormat::Vtt)
            .await
        {
            Ok(content) => {
                let track = parse_subtitle(&content, None);
                info!("Loaded {} subtitle lines from API", track.lines.len());
                let mut subs = self.subtitles.write().await;
                *subs = Some(track);
                return;
            }
            Err(e) => {
                warn!("Failed to fetch subtitles from API: {}", e);
            }
        }

        // Disk fallback
        let config = self.config.read().await;
        if config.media_access_mode != crate::config::MediaAccessMode::Api {
            let path = session
                .now_playing
                .as_ref()
                .and_then(|np| np.path.as_deref())
                .map(str::to_owned);
            let path = match path {
                Some(path) => Some(path),
                None => server
                    .get_item_info(item_id, session.user_id.as_deref())
                    .await
                    .ok()
                    .and_then(|item_info| item_info.path),
            };

            if let Some(path) = path {
                let local_path = config.map_path(&path);
                debug!("Trying disk fallback at: {:?}", local_path);
                // Try to find adjacent subtitle files
                if let Some(parent) = local_path.parent() {
                    let stem = local_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");

                    for ext in &["srt", "ass", "ssa", "vtt"] {
                        // Try lang-specific: video.jpn.srt
                        let lang_path = parent.join(format!("{}.{}.{}", stem, target_lang, ext));
                        if lang_path.exists() {
                            if let Ok(content) = std::fs::read_to_string(&lang_path) {
                                let track = parse_subtitle(&content, lang_path.to_str());
                                info!(
                                    "Loaded {} subtitle lines from disk: {:?}",
                                    track.lines.len(),
                                    lang_path
                                );
                                let mut subs = self.subtitles.write().await;
                                *subs = Some(track);
                                return;
                            }
                        }

                        // Try plain: video.srt
                        let plain_path = parent.join(format!("{}.{}", stem, ext));
                        if plain_path.exists() {
                            if let Ok(content) = std::fs::read_to_string(&plain_path) {
                                let track = parse_subtitle(&content, plain_path.to_str());
                                info!(
                                    "Loaded {} subtitle lines from disk: {:?}",
                                    track.lines.len(),
                                    plain_path
                                );
                                let mut subs = self.subtitles.write().await;
                                *subs = Some(track);
                                return;
                            }
                        }
                    }
                }
            }
        }

        warn!("Could not load subtitles from any source");
        let mut subs = self.subtitles.write().await;
        *subs = None;
    }
}

/// Run the session polling loop.
pub async fn run_session_poller(manager: Arc<SessionManager>) {
    loop {
        manager.poll_once().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
