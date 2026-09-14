use crate::config::{Config, MediaServerKind};
use crate::media_server::{
    ItemInfo, MediaServer, MediaStream, NowPlaying, ServerMap, Session, StreamType, SubtitleFormat,
};
use crate::mining::AppDatabase;
use crate::subtitle::{SubtitleTrack, parse_subtitle};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, watch};
use tracing::{debug, info, warn};

const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// Paused sessions remain available briefly for manual selection/mining, but
/// media servers can otherwise retain them for hours or even indefinitely.
const PAUSED_SESSION_VISIBLE_AFTER: Duration = Duration::from_secs(5 * 60);
/// AudioBookShelf keeps downloaded playback in its listening-history endpoint
/// for fifteen minutes after the last sync, matching the server-side contract.
const AUDIOBOOKSHELF_PAUSED_SESSION_VISIBLE_AFTER: Duration = Duration::from_secs(15 * 60);
/// A server that continues to claim a session is playing without checking in
/// is treated as abandoned. Servers without a reliable activity clock are
/// still trusted to return only live sessions.
const PLAYING_SESSION_STALE_AFTER: Duration = Duration::from_secs(2 * 60);
/// Browser tabs can overwrite one provider session row on each check-in.
/// Keep their individual items available across several ten-second check-ins.
const SHARED_SESSION_RETENTION: Duration = Duration::from_secs(30);

/// Represents the current state of the monitored session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub sessions: Vec<SessionSummary>,
    pub active_session_id: Option<String>,
    pub now_playing: Option<NowPlayingState>,
}

impl SessionState {
    pub fn active_remote_session_id(&self) -> Option<&str> {
        let active_id = self.active_session_id.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == active_id)
            .map(|session| session.remote_session_id.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    /// Provider ID for commands; the public ID identifies an individual item.
    #[serde(skip)]
    pub remote_session_id: String,
    pub server_kind: MediaServerKind,
    pub client: String,
    pub device_name: String,
    pub user_name: Option<String>,
    pub title: Option<String>,
    pub is_target_language: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleSelectionMode {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleCandidateSource {
    Server,
    Sidecar,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubtitleCandidate {
    pub id: String,
    pub source: SubtitleCandidateSource,
    pub stream_index: Option<u32>,
    pub language: Option<String>,
    pub label: String,
    pub codec: Option<String>,
    pub is_default: bool,
    pub is_external: bool,
    pub is_selected_in_session: bool,
    /// Filename similarity to the active media file for fuzzy sidecar fallbacks.
    /// Exact sidecars and server tracks do not need a confidence value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_confidence: Option<u8>,
    #[serde(skip_serializing, skip_deserializing, default)]
    local_path: Option<PathBuf>,
    /// Whether a sidecar's basename exactly matches the media basename.
    /// Kept internal because it is an auto-selection hint, not UI metadata.
    #[serde(skip_serializing, skip_deserializing, default)]
    is_exact_file_match: bool,
}

/// Describes an available audio track in the current media.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioTrack {
    /// Absolute stream index (for ffmpeg `-map 0:{index}`).
    pub index: u32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub display_title: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub channels: Option<String>,
}

/// How the active audio track was chosen.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioTrackResolution {
    /// Only one audio track exists — nothing to choose.
    Single,
    /// Auto-selected because it matches the target language.
    AutoLanguage,
    /// User explicitly selected this track.
    Manual,
    /// Multiple tracks exist and none match the target language — user must pick.
    NeedsSelection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NowPlayingState {
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub item_id: String,
    pub title: String,
    /// Series name as reported by the media server (None for movies / one-offs).
    #[serde(default)]
    pub series_name: Option<String>,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub is_paused: bool,
    pub supports_remote_control: bool,
    pub subtitle_stream_index: Option<u32>,
    pub subtitle_candidate_id: Option<String>,
    pub subtitle_selection_mode: SubtitleSelectionMode,
    pub media_source_id: String,
    pub file_path: Option<String>,
    pub audio_stream_index: Option<u32>,
    /// Accumulated user timing offset for the loaded subtitle track (ms).
    /// Mirrors `SubtitleTrack::offset_ms` for the active item.
    #[serde(default)]
    pub subtitle_offset_ms: i64,
    /// True while Nagare is fetching and parsing the selected subtitle track.
    #[serde(default)]
    pub subtitle_loading: bool,
}

/// A snapshot of a previously-watched item, kept so the user can mine it later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub item_id: String,
    pub title: String,
    /// Series name as reported by the media server (None for movies / one-offs).
    #[serde(default)]
    pub series_name: Option<String>,
    pub media_source_id: String,
    pub file_path: Option<String>,
    pub duration_ms: Option<i64>,
    pub subtitle_count: usize,
    /// Audio language tags reported by the media server. Automatic Tadoku
    /// exports use these to ensure the item matches the configured target
    /// language; entries without a known matching stream stay manual-only.
    #[serde(default)]
    pub audio_languages: Vec<String>,
    pub last_position_ms: i64,
    /// Timestamp when we last saw this item playing
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
struct ServerSession {
    kind: MediaServerKind,
    server: Arc<dyn MediaServer>,
    session: Session,
    reported: bool,
    newly_observed: bool,
}

impl ServerSession {
    fn id(&self) -> String {
        scoped_playback_id(
            self.kind,
            &self.session.id,
            self.session
                .now_playing
                .as_ref()
                .map(|item| item.item_id.as_str())
                .unwrap_or(""),
            self.session.playback_session_id.as_deref(),
        )
    }

    fn same_connection(&self, other: &Self) -> bool {
        self.kind == other.kind
            && Arc::ptr_eq(&self.server, &other.server)
            && self.session.id == other.session.id
            && self.session.playback_session_id == other.session.playback_session_id
            && self.session.user_id == other.session.user_id
    }
}

#[derive(Default)]
struct PlaybackSessions {
    observed: HashMap<String, (ServerSession, Instant)>,
}

impl PlaybackSessions {
    fn update(&mut self, reported: Vec<ServerSession>, now: Instant) -> Vec<ServerSession> {
        // Do not hold sessions through disconnection, unload, filtering, or an
        // explicit Plex playback-instance change. Only a shared row is retained.
        self.observed.retain(|_, (previous, last_seen)| {
            now.duration_since(*last_seen) < SHARED_SESSION_RETENTION
                && reported.iter().any(|current| {
                    current.session.now_playing.is_some()
                        && current.same_connection(previous)
                        && (current.id() == previous.id()
                            || previous.kind != MediaServerKind::Audiobookshelf)
                })
        });
        for (previous, _) in self.observed.values_mut() {
            previous.reported = false;
            previous.newly_observed = false;
        }
        for mut current in reported {
            if current.session.now_playing.is_none() {
                continue;
            }
            let id = current.id();
            current.newly_observed = !self.observed.contains_key(&id);
            current.reported = true;
            self.observed.insert(id, (current, now));
        }

        let mut sessions: Vec<_> = self
            .observed
            .values()
            .map(|(session, _)| session.clone())
            .collect();
        let mut connections = HashMap::new();
        for session in &sessions {
            *connections
                .entry((session.kind, session.session.id.clone()))
                .or_insert(0) += 1;
        }
        for session in &mut sessions {
            // A command addressed only to the shared provider ID cannot target
            // one of these tabs reliably. Restore controls when it is unique.
            if connections[&(session.kind, session.session.id.clone())] > 1 {
                session.session.supports_remote_control = false;
            }
        }
        sessions
    }
}

pub fn scoped_history_id(kind: MediaServerKind, item_id: &str) -> String {
    format!("{kind}|{item_id}")
}

pub fn scoped_session_id(kind: MediaServerKind, session_id: &str) -> String {
    format!("{kind}|{session_id}")
}

fn scoped_playback_id(
    kind: MediaServerKind,
    session_id: &str,
    item_id: &str,
    playback_session_id: Option<&str>,
) -> String {
    let id = format!("{}|{item_id}", scoped_session_id(kind, session_id));
    match playback_session_id {
        Some(playback_id) => format!("{id}|{playback_id}"),
        None => id,
    }
}

fn is_ignored_episode(now_playing: &NowPlaying) -> bool {
    let is_episode = now_playing.media_type.eq_ignore_ascii_case("episode")
        || now_playing.series_name.is_some()
        || now_playing.episode_index.is_some();

    is_episode && now_playing.name.trim().eq_ignore_ascii_case("theme")
}

fn session_is_visible(kind: MediaServerKind, session: &Session, now_ms: i64) -> bool {
    let Some(last_activity_at_ms) = session.last_activity_at_ms else {
        return true;
    };
    let age_ms = now_ms.saturating_sub(last_activity_at_ms).max(0) as u64;
    let lifetime = if session.play_state.is_paused {
        if kind == MediaServerKind::Audiobookshelf {
            AUDIOBOOKSHELF_PAUSED_SESSION_VISIBLE_AFTER
        } else {
            PAUSED_SESSION_VISIBLE_AFTER
        }
    } else {
        PLAYING_SESSION_STALE_AFTER
    };

    age_ms <= lifetime.as_millis() as u64
}

/// Prefer the media server's activity timestamp when it has one. This keeps a
/// paused AudioBookShelf row from looking newly active every time Nagare polls
/// the unchanged row.
fn session_playback_activity_at(session: &Session) -> chrono::DateTime<chrono::Utc> {
    session
        .last_activity_at_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .unwrap_or_else(chrono::Utc::now)
}

fn compare_auto_session_priority(
    left: &Session,
    right: &Session,
    target_language: &str,
) -> CmpOrdering {
    let left_is_playing = !left.play_state.is_paused;
    let right_is_playing = !right.play_state.is_paused;
    let left_is_target = left
        .now_playing
        .as_ref()
        .is_some_and(|item| item.has_audio_language(target_language));
    let right_is_target = right
        .now_playing
        .as_ref()
        .is_some_and(|item| item.has_audio_language(target_language));

    right_is_playing
        .cmp(&left_is_playing)
        .then_with(|| right_is_target.cmp(&left_is_target))
        .then_with(|| right.last_activity_at_ms.cmp(&left.last_activity_at_ms))
        .then_with(|| left.id.cmp(&right.id))
}

pub struct SessionManager {
    servers: Arc<RwLock<ServerMap>>,
    config: Arc<RwLock<Config>>,
    state: Arc<RwLock<SessionState>>,
    subtitles: Arc<RwLock<Option<SubtitleTrack>>>,
    /// Secondary native-language track shown alongside the target subtitles
    /// (live playback only). `None` when no suitable native track exists.
    native_subtitles: Arc<RwLock<Option<SubtitleTrack>>>,
    subtitle_candidates: Arc<RwLock<Vec<SubtitleCandidate>>>,
    state_tx: watch::Sender<SessionState>,
    /// User override for which session to track (None = auto-select)
    selected_session_id: Arc<RwLock<Option<String>>>,
    /// User override for which subtitle track to load on the active item (None = auto-select).
    selected_subtitle_candidate_override: Arc<RwLock<Option<String>>>,
    /// The subtitle candidate currently loaded into Nagare, if any.
    loaded_subtitle_candidate_id: Arc<RwLock<Option<String>>>,
    /// The native-language subtitle candidate currently loaded, if any.
    loaded_native_candidate_id: Arc<RwLock<Option<String>>>,
    /// Subtitle tracks keyed by item_id, kept forever for history mining
    subtitle_history: Arc<RwLock<HashMap<String, SubtitleTrack>>>,
    /// Metadata for previously-watched items
    history: Arc<RwLock<HashMap<String, HistoryEntry>>>,
    /// Audio tracks for the current media item.
    audio_tracks: Arc<RwLock<Vec<AudioTrack>>>,
    /// The currently selected audio stream index (absolute).
    selected_audio_track: Arc<RwLock<Option<u32>>>,
    /// How the audio track was resolved.
    audio_track_resolution: Arc<RwLock<AudioTrackResolution>>,
    /// Shared persistence layer for config, session state, and mined-note history.
    db: Arc<AppDatabase>,
    /// Prevent overlapping poll cycles when the API forces immediate refreshes.
    poll_lock: Mutex<()>,
    playback_sessions: Mutex<PlaybackSessions>,
    /// Throttle: only persist to SQLite at most once per position-save interval.
    last_save: Arc<Mutex<Instant>>,
    /// Whether the Plex websocket listener is connected and can provide live play-state updates.
    plex_ws_connected: AtomicBool,
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
            native_subtitles: Arc::new(RwLock::new(None)),
            subtitle_candidates: Arc::new(RwLock::new(Vec::new())),
            state_tx,
            selected_session_id: Arc::new(RwLock::new(None)),
            selected_subtitle_candidate_override: Arc::new(RwLock::new(None)),
            loaded_subtitle_candidate_id: Arc::new(RwLock::new(None)),
            loaded_native_candidate_id: Arc::new(RwLock::new(None)),
            subtitle_history: Arc::new(RwLock::new(subtitle_history)),
            history: Arc::new(RwLock::new(history)),
            audio_tracks: Arc::new(RwLock::new(Vec::new())),
            selected_audio_track: Arc::new(RwLock::new(None)),
            audio_track_resolution: Arc::new(RwLock::new(AudioTrackResolution::Single)),
            db,
            poll_lock: Mutex::new(()),
            playback_sessions: Mutex::new(PlaybackSessions::default()),
            last_save,
            plex_ws_connected: AtomicBool::new(false),
        })
    }

    pub fn subtitles(&self) -> Arc<RwLock<Option<SubtitleTrack>>> {
        self.subtitles.clone()
    }

    pub fn native_subtitles(&self) -> Arc<RwLock<Option<SubtitleTrack>>> {
        self.native_subtitles.clone()
    }

    pub fn subtitle_candidates(&self) -> Arc<RwLock<Vec<SubtitleCandidate>>> {
        self.subtitle_candidates.clone()
    }

    pub fn audio_tracks(&self) -> Arc<RwLock<Vec<AudioTrack>>> {
        self.audio_tracks.clone()
    }

    pub fn selected_audio_track(&self) -> Arc<RwLock<Option<u32>>> {
        self.selected_audio_track.clone()
    }

    pub fn audio_track_resolution(&self) -> Arc<RwLock<AudioTrackResolution>> {
        self.audio_track_resolution.clone()
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

    /// Build [`AudioTrack`] list from the media server's stream metadata.
    fn audio_tracks_from_streams(streams: &[MediaStream]) -> Vec<AudioTrack> {
        streams
            .iter()
            .filter(|s| s.stream_type == StreamType::Audio)
            .map(|s| {
                // Try to extract channel info from display_title (e.g. "Japanese - 2.0 - AAC")
                let channels = s.display_title.as_ref().and_then(|dt| {
                    // Look for patterns like "2.0", "5.1", "7.1"
                    dt.split(|c: char| c == '-' || c == '(' || c == ')')
                        .map(str::trim)
                        .find(|part| {
                            part.contains('.')
                                && part.len() <= 4
                                && part.chars().all(|c| c.is_ascii_digit() || c == '.')
                        })
                        .map(String::from)
                });
                AudioTrack {
                    index: s.index,
                    codec: s.codec.clone(),
                    language: s.language.clone(),
                    display_title: s.display_title.clone(),
                    title: s.title.clone(),
                    is_default: s.is_default,
                    channels,
                }
            })
            .collect()
    }

    /// Resolve which audio track to select and how.
    fn resolve_audio_track(
        tracks: &[AudioTrack],
        target_lang: &str,
        user_override: Option<u32>,
    ) -> (Option<u32>, AudioTrackResolution) {
        if let Some(idx) = user_override {
            if tracks.iter().any(|t| t.index == idx) {
                return (Some(idx), AudioTrackResolution::Manual);
            }
        }

        if tracks.len() <= 1 {
            return (
                tracks.first().map(|t| t.index),
                AudioTrackResolution::Single,
            );
        }

        // Multiple tracks — try target language match.
        if let Some(track) = tracks
            .iter()
            .find(|t| Self::language_matches_target(t.language.as_deref(), target_lang))
        {
            return (Some(track.index), AudioTrackResolution::AutoLanguage);
        }

        // No language match — user needs to pick.
        (None, AudioTrackResolution::NeedsSelection)
    }

    /// User-initiated audio track selection.
    pub async fn select_audio_track(&self, stream_index: u32) {
        let tracks = self.audio_tracks.read().await;
        if tracks.iter().any(|t| t.index == stream_index) {
            let mut sel = self.selected_audio_track.write().await;
            *sel = Some(stream_index);
            let mut res = self.audio_track_resolution.write().await;
            *res = AudioTrackResolution::Manual;

            // Update NowPlayingState
            let mut state = self.state.write().await;
            if let Some(np) = state.now_playing.as_mut() {
                np.audio_stream_index = Some(stream_index);
            }
            drop(state);
            drop(sel);
            drop(res);
            drop(tracks);

            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
        }
    }

    /// Apply a subtitle timing offset (absolute, in ms).
    ///
    /// If `history_id` is `None` or matches the active item, the in-memory
    /// active subtitle track is shifted and `NowPlayingState` is updated.
    /// In all cases the cached `subtitle_history` track for the matching
    /// history entry is also shifted (so re-mining from history sees the
    /// adjusted timings) and persisted to SQLite.
    ///
    /// After loading a fresh subtitle track for `history_id`, look up any
    /// previously persisted offset stored in `subtitle_history` and re-apply
    /// it to the newly loaded track so timing corrections survive across
    /// sessions, server restarts, and track switches.
    async fn restore_subtitle_offset(&self, history_id: &str) {
        let saved_offset = self
            .subtitle_history
            .read()
            .await
            .get(history_id)
            .map(|t| t.offset_ms)
            .unwrap_or(0);

        if saved_offset == 0 {
            return;
        }

        {
            let mut subs = self.subtitles.write().await;
            if let Some(track) = subs.as_mut() {
                // Fresh track starts at offset 0; apply the saved delta directly.
                track.shift_by(saved_offset);
            }
        }

        let mut state = self.state.write().await;
        if let Some(np) = state.now_playing.as_mut() {
            if np.history_id == history_id {
                np.subtitle_offset_ms = saved_offset;
            }
        }
    }

    /// Returns the new accumulated offset.
    pub async fn set_subtitle_offset(
        &self,
        history_id: Option<&str>,
        absolute_offset_ms: i64,
    ) -> anyhow::Result<i64> {
        let active_history_id = self
            .state
            .read()
            .await
            .now_playing
            .as_ref()
            .map(|np| np.history_id.clone());

        let target_history_id = match history_id {
            Some(id) => id.to_string(),
            None => active_history_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("No active item and no history_id provided"))?,
        };

        let is_active = active_history_id.as_deref() == Some(target_history_id.as_str());

        // Shift the active in-memory track if applicable.
        if is_active {
            let mut subs = self.subtitles.write().await;
            if let Some(track) = subs.as_mut() {
                let delta = absolute_offset_ms - track.offset_ms;
                track.shift_by(delta);
            }
        }

        // Shift the cached history copy too, so future re-loads/mining match.
        {
            let mut subtitle_history = self.subtitle_history.write().await;
            if let Some(track) = subtitle_history.get_mut(&target_history_id) {
                let delta = absolute_offset_ms - track.offset_ms;
                track.shift_by(delta);
            } else if is_active {
                // Snapshot the active track into history so the offset survives.
                if let Some(track) = self.subtitles.read().await.clone() {
                    subtitle_history.insert(target_history_id.clone(), track);
                }
            }
        }

        if is_active {
            let mut state = self.state.write().await;
            if let Some(np) = state.now_playing.as_mut() {
                np.subtitle_offset_ms = absolute_offset_ms;
            }
            drop(state);
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
        }

        // Persist (force=true → writes both history + subtitle_history maps).
        self.save_history(true).await;

        Ok(absolute_offset_ms)
    }

    ///
    /// `force = true`  → write immediately (used on new-item and trailing flushes).
    /// `force = false` → write only if the position-save interval has elapsed
    ///                   since the last save (used on high-frequency position updates).
    async fn save_history(&self, force: bool) {
        let mut last = self.last_save.lock().await;
        if !force && last.elapsed() < POSITION_SAVE_INTERVAL {
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
            // Back off after a failed write so a transient SQLite lock cannot
            // turn the fast playback poll into a tight retry loop. The next
            // poll after the normal interval will retry the latest position.
            *last = Instant::now();
            warn!("Failed to persist session history to SQLite: {error:#}");
            return;
        }

        *last = Instant::now();
    }

    /// Persist all in-memory history immediately, bypassing the position throttle.
    pub async fn flush_history(&self) {
        self.save_history(true).await;
    }

    /// Queue a Tadoku eligibility check after the media server unloads the
    /// current item. Long-form playback still waits for its inactivity window.
    fn queue_automatic_tadoku_export(&self) {
        let config = self.config.clone();
        let db = self.db.clone();
        tokio::spawn(async move {
            match crate::tadoku::export_if_automatic(config, db).await {
                Ok(Some(count)) => {
                    info!(
                        "Playback-ended Tadoku check completed ({} show logs)",
                        count
                    )
                }
                Ok(None) => {}
                Err(error) => warn!("Playback-ended Tadoku check failed: {}", error),
            }
        });
    }

    pub async fn select_session(&self, session_id: Option<String>) {
        let mut sel = self.selected_session_id.write().await;
        *sel = session_id;
        drop(sel);

        let mut subtitle_override = self.selected_subtitle_candidate_override.write().await;
        *subtitle_override = None;
    }

    pub fn set_plex_websocket_connected(&self, connected: bool) {
        self.plex_ws_connected.store(connected, Ordering::Relaxed);
    }

    pub async fn poll_interval(&self) -> Duration {
        let servers = self.servers.read().await;
        let has_plex = servers.contains_key(&MediaServerKind::Plex);
        let has_non_plex = servers.keys().any(|kind| *kind != MediaServerKind::Plex);
        drop(servers);

        let active_server_kind = self
            .state
            .read()
            .await
            .now_playing
            .as_ref()
            .map(|now_playing| now_playing.server_kind);

        if has_plex
            && self.plex_ws_connected.load(Ordering::Relaxed)
            && active_server_kind == Some(MediaServerKind::Plex)
        {
            Duration::from_secs(5)
        } else if has_non_plex && active_server_kind.is_some() {
            Duration::from_millis(100)
        } else if has_plex {
            if self.plex_ws_connected.load(Ordering::Relaxed) {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(1)
            }
        } else {
            Duration::from_secs(1)
        }
    }

    async fn collect_server_sessions(&self, servers: ServerMap) -> Vec<ServerSession> {
        let config = self.config.read().await.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut sessions = Vec::<ServerSession>::new();
        for (kind, server) in servers {
            match server.get_sessions().await {
                Ok(fetched) => {
                    sessions.extend(
                        fetched
                            .into_iter()
                            .filter(|session| {
                                config.is_user_allowed(
                                    kind,
                                    session.user_id.as_deref(),
                                    session.user_name.as_deref(),
                                )
                            })
                            .filter(|session| {
                                !session.now_playing.as_ref().is_some_and(is_ignored_episode)
                            })
                            .filter(|session| session_is_visible(kind, session, now_ms))
                            .map(|session| ServerSession {
                                kind,
                                server: server.clone(),
                                session,
                                reported: true,
                                newly_observed: false,
                            }),
                    );
                }
                Err(e) => {
                    warn!("Failed to fetch {kind} sessions: {}", e);
                }
            }
        }

        sessions
    }

    async fn collect_playback_sessions(&self, servers: ServerMap) -> Vec<ServerSession> {
        let reported = self.collect_server_sessions(servers).await;
        let mut sessions = self
            .playback_sessions
            .lock()
            .await
            .update(reported, Instant::now());
        let config = self.config.read().await;
        sessions.retain(|s| {
            config.is_user_allowed(
                s.kind,
                s.session.user_id.as_deref(),
                s.session.user_name.as_deref(),
            )
        });
        sessions
    }

    fn is_loadable_text_subtitle(stream: &MediaStream) -> bool {
        if stream.stream_type != StreamType::Subtitle {
            return false;
        }

        stream.is_text_subtitle_stream
            || matches!(
                stream.codec.as_deref(),
                Some("srt" | "subrip" | "ass" | "ssa" | "vtt" | "webvtt")
            )
    }

    fn normalize_language_tag(value: &str) -> String {
        value
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-")
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect()
    }

    fn language_alias_group(language: &str) -> Option<&'static [&'static str]> {
        match language {
            "ja" | "jp" | "jpn" | "japanese" => Some(&["ja", "jp", "jpn", "japanese"]),
            "en" | "eng" | "english" => Some(&["en", "eng", "english"]),
            "es" | "spa" | "spanish" => Some(&["es", "spa", "spanish"]),
            "de" | "ger" | "deu" | "german" => Some(&["de", "ger", "deu", "german"]),
            "fr" | "fre" | "fra" | "french" => Some(&["fr", "fre", "fra", "french"]),
            "it" | "ita" | "italian" => Some(&["it", "ita", "italian"]),
            "pt" | "por" | "portuguese" => Some(&["pt", "por", "portuguese"]),
            "zh" | "zho" | "chi" | "chinese" => Some(&["zh", "zho", "chi", "chinese"]),
            "ko" | "kor" | "korean" => Some(&["ko", "kor", "korean"]),
            "ru" | "rus" | "russian" => Some(&["ru", "rus", "russian"]),
            "ar" | "ara" | "arabic" => Some(&["ar", "ara", "arabic"]),
            "tr" | "tur" | "turkish" => Some(&["tr", "tur", "turkish"]),
            "pl" | "pol" | "polish" => Some(&["pl", "pol", "polish"]),
            "nl" | "dut" | "nld" | "dutch" => Some(&["nl", "dut", "nld", "dutch"]),
            "sv" | "swe" | "swedish" => Some(&["sv", "swe", "swedish"]),
            "no" | "nor" | "norwegian" => Some(&["no", "nor", "norwegian"]),
            "da" | "dan" | "danish" => Some(&["da", "dan", "danish"]),
            "fi" | "fin" | "finnish" => Some(&["fi", "fin", "finnish"]),
            "cs" | "cze" | "ces" | "czech" => Some(&["cs", "cze", "ces", "czech"]),
            "el" | "gre" | "ell" | "greek" => Some(&["el", "gre", "ell", "greek"]),
            "ro" | "rum" | "ron" | "romanian" => Some(&["ro", "rum", "ron", "romanian"]),
            "hu" | "hun" | "hungarian" => Some(&["hu", "hun", "hungarian"]),
            "vi" | "vie" | "vietnamese" => Some(&["vi", "vie", "vietnamese"]),
            "th" | "tha" | "thai" => Some(&["th", "tha", "thai"]),
            "id" | "ind" | "indonesian" => Some(&["id", "ind", "indonesian"]),
            _ => None,
        }
    }

    pub(crate) fn language_matches_target(language: Option<&str>, target_lang: &str) -> bool {
        let Some(language) = language else {
            return false;
        };

        let normalized_language = Self::normalize_language_tag(language);
        let normalized_target = Self::normalize_language_tag(target_lang);
        if normalized_language.is_empty() || normalized_target.is_empty() {
            return false;
        }

        if normalized_language == normalized_target {
            return true;
        }

        let language_base = normalized_language
            .split('-')
            .next()
            .unwrap_or(normalized_language.as_str());
        let target_base = normalized_target
            .split('-')
            .next()
            .unwrap_or(normalized_target.as_str());
        if language_base == target_base {
            return true;
        }

        Self::language_alias_group(language_base)
            .zip(Self::language_alias_group(target_base))
            .map(|(left, right)| left == right)
            .unwrap_or(false)
    }

    fn is_language_metadata_token(token: &str) -> bool {
        let normalized = Self::normalize_language_tag(token);
        if normalized.is_empty() {
            return false;
        }

        if Self::language_alias_group(normalized.as_str()).is_some() {
            return true;
        }

        let base = normalized.split('-').next().unwrap_or(normalized.as_str());
        base.chars()
            .all(|character| character.is_ascii_alphabetic())
            && (2..=3).contains(&base.len())
    }

    fn subtitle_candidate_label(stream: &MediaStream) -> String {
        let base = stream
            .display_title
            .clone()
            .or_else(|| stream.title.clone())
            .or_else(|| stream.language.clone())
            .unwrap_or_else(|| format!("Subtitle {}", stream.index));

        let mut details = Vec::new();
        if let Some(codec) = stream.codec.as_deref() {
            details.push(codec.to_uppercase());
        }
        details.push(if stream.is_external {
            "External".to_string()
        } else {
            "Internal".to_string()
        });
        if stream.is_default {
            details.push("Default".to_string());
        }

        format!("{base} ({})", details.join(" · "))
    }

    fn subtitle_candidate_from_stream(
        stream: &MediaStream,
        is_selected_in_session: bool,
    ) -> SubtitleCandidate {
        SubtitleCandidate {
            id: format!("server:{}", stream.index),
            source: SubtitleCandidateSource::Server,
            stream_index: Some(stream.index),
            language: stream.language.clone(),
            label: Self::subtitle_candidate_label(stream),
            codec: stream.codec.clone(),
            is_default: stream.is_default,
            is_external: stream.is_external,
            is_selected_in_session,
            match_confidence: None,
            local_path: None,
            is_exact_file_match: false,
        }
    }

    fn selected_session_subtitle_stream<'a>(session: &'a Session) -> Option<&'a MediaStream> {
        let selected_index = session.play_state.subtitle_stream_index?;
        session
            .now_playing
            .as_ref()?
            .media_streams
            .iter()
            .find(|stream| {
                stream.stream_type == StreamType::Subtitle && stream.index == selected_index as u32
            })
    }

    fn stream_matches_selected_session_stream(
        stream: &MediaStream,
        selected_session_stream: Option<&MediaStream>,
    ) -> bool {
        let Some(selected) = selected_session_stream else {
            return false;
        };

        stream.stream_type == StreamType::Subtitle
            && stream.codec == selected.codec
            && stream.language == selected.language
            && stream.display_title == selected.display_title
            && stream.title == selected.title
            && stream.is_external == selected.is_external
    }

    fn subtitle_streams_for_media_source<'a>(
        item_info: &'a ItemInfo,
        media_source_id: &str,
    ) -> &'a [MediaStream] {
        item_info
            .media_sources
            .iter()
            .find(|source| source.id == media_source_id)
            .or_else(|| item_info.media_sources.first())
            .map(|source| source.media_streams.as_slice())
            .unwrap_or(item_info.media_streams.as_slice())
    }

    fn server_subtitle_candidates_from_streams(
        streams: &[MediaStream],
        session: &Session,
    ) -> Vec<SubtitleCandidate> {
        let selected_index = session.play_state.subtitle_stream_index;
        let selected_session_stream = Self::selected_session_subtitle_stream(session);

        streams
            .iter()
            .filter(|stream| Self::is_loadable_text_subtitle(stream))
            .map(|stream| {
                let is_selected = selected_index == Some(stream.index as i32)
                    || Self::stream_matches_selected_session_stream(
                        stream,
                        selected_session_stream,
                    );
                Self::subtitle_candidate_from_stream(stream, is_selected)
            })
            .collect()
    }

    async fn server_subtitle_candidates(
        &self,
        item_id: &str,
        media_source_id: &str,
        session: &Session,
        server: &Arc<dyn MediaServer>,
    ) -> Vec<SubtitleCandidate> {
        if let Ok(item_info) = server
            .get_item_info(item_id, session.user_id.as_deref())
            .await
        {
            let streams = Self::subtitle_streams_for_media_source(&item_info, media_source_id);
            let candidates = Self::server_subtitle_candidates_from_streams(streams, session);
            if !candidates.is_empty() {
                return candidates;
            }
        }

        let Some(now_playing) = session.now_playing.as_ref() else {
            return Vec::new();
        };

        Self::server_subtitle_candidates_from_streams(&now_playing.media_streams, session)
    }

    fn is_supported_subtitle_extension(path: &Path) -> Option<String> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "srt" | "ass" | "ssa" | "vtt" => Some(extension),
            _ => None,
        }
    }

    fn sidecar_language_hint(video_stem: &str, subtitle_path: &Path) -> Option<String> {
        let stem = subtitle_path.file_stem()?.to_str()?;
        if stem == video_stem {
            return None;
        }

        let suffix = stem.strip_prefix(video_stem)?.strip_prefix('.')?;
        let ignored_tokens = [
            "default",
            "forced",
            "sdh",
            "cc",
            "signs",
            "dialogue",
            "dubtitle",
            "sub",
            "subs",
            "subtitle",
            "subtitles",
        ];

        for token in suffix.split('.') {
            let normalized = Self::normalize_language_tag(token);
            if normalized.is_empty() || ignored_tokens.contains(&normalized.as_str()) {
                continue;
            }
            if Self::is_language_metadata_token(&normalized) {
                return Some(normalized);
            }
        }

        None
    }

    fn sidecar_subtitle_candidate(
        video_stem: &str,
        subtitle_path: PathBuf,
    ) -> Option<SubtitleCandidate> {
        let extension = Self::is_supported_subtitle_extension(&subtitle_path)?;
        let file_name = subtitle_path.file_name()?.to_str()?.to_string();
        let stem = subtitle_path.file_stem()?.to_str()?;
        if stem != video_stem && !stem.starts_with(&format!("{video_stem}.")) {
            return None;
        }

        let is_exact_file_match = stem == video_stem;
        let language = Self::sidecar_language_hint(video_stem, &subtitle_path);

        Some(SubtitleCandidate {
            id: format!("sidecar:{file_name}"),
            source: SubtitleCandidateSource::Sidecar,
            stream_index: None,
            language,
            label: format!("{file_name} ({} Sidecar)", extension.to_ascii_uppercase()),
            codec: Some(extension),
            is_default: false,
            is_external: true,
            is_selected_in_session: false,
            match_confidence: None,
            local_path: Some(subtitle_path),
            is_exact_file_match,
        })
    }

    fn normalize_filename_for_match(value: &str) -> Vec<char> {
        value
            .chars()
            .flat_map(char::to_lowercase)
            .filter(|character| character.is_alphanumeric())
            .collect()
    }

    fn filename_number_tokens(value: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();

        for character in value.chars().chain(std::iter::once(' ')) {
            if character.is_ascii_digit() {
                current.push(character);
            } else if !current.is_empty() {
                let normalized = current.trim_start_matches('0');
                tokens.push(if normalized.is_empty() {
                    "0".to_string()
                } else {
                    normalized.to_string()
                });
                current.clear();
            }
        }

        tokens
    }

    /// Return a normalized Levenshtein similarity in the inclusive range 0..=100.
    /// Punctuation, spacing, and case are ignored so common release-name differences
    /// do not overwhelm the meaningful parts of the filename.
    fn subtitle_filename_confidence(media_stem: &str, subtitle_stem: &str) -> u8 {
        let media = Self::normalize_filename_for_match(media_stem);
        let subtitle = Self::normalize_filename_for_match(subtitle_stem);
        let max_len = media.len().max(subtitle.len());

        if max_len == 0 {
            return 0;
        }

        let mut previous: Vec<usize> = (0..=subtitle.len()).collect();
        let mut current = vec![0; subtitle.len() + 1];

        for (media_index, media_character) in media.iter().enumerate() {
            current[0] = media_index + 1;
            for (subtitle_index, subtitle_character) in subtitle.iter().enumerate() {
                let substitution_cost = usize::from(media_character != subtitle_character);
                current[subtitle_index + 1] = (previous[subtitle_index + 1] + 1)
                    .min(current[subtitle_index] + 1)
                    .min(previous[subtitle_index] + substitution_cost);
            }
            std::mem::swap(&mut previous, &mut current);
        }

        let distance = previous[subtitle.len()];
        let mut confidence = ((max_len - distance) * 100 + max_len / 2) / max_len;

        // Track/episode numbers carry more identity than the surrounding release
        // text. Penalize a conflicting final number so "Episode 02 (JP)" ranks
        // above an otherwise-nearer "Episode 03".
        let media_numbers = Self::filename_number_tokens(media_stem);
        let subtitle_numbers = Self::filename_number_tokens(subtitle_stem);
        if media_numbers.last().is_some()
            && subtitle_numbers.last().is_some()
            && media_numbers.last() != subtitle_numbers.last()
        {
            confidence /= 2;
        }

        confidence as u8
    }

    fn fuzzy_sidecar_subtitle_candidate(
        video_stem: &str,
        subtitle_path: PathBuf,
    ) -> Option<SubtitleCandidate> {
        let extension = Self::is_supported_subtitle_extension(&subtitle_path)?;
        if extension != "srt" {
            return None;
        }

        let file_name = subtitle_path.file_name()?.to_str()?.to_string();
        let stem = subtitle_path.file_stem()?.to_str()?;
        let match_confidence = Self::subtitle_filename_confidence(video_stem, stem);

        Some(SubtitleCandidate {
            id: format!("sidecar:{file_name}"),
            source: SubtitleCandidateSource::Sidecar,
            stream_index: None,
            language: None,
            label: format!(
                "{file_name} ({} Sidecar · {match_confidence}% match)",
                extension.to_ascii_uppercase()
            ),
            codec: Some(extension),
            is_default: false,
            is_external: true,
            is_selected_in_session: false,
            match_confidence: Some(match_confidence),
            local_path: Some(subtitle_path),
            is_exact_file_match: false,
        })
    }

    fn sidecar_subtitle_candidates(local_path: &Path) -> Vec<SubtitleCandidate> {
        let Some(parent) = local_path.parent() else {
            return Vec::new();
        };
        let Some(video_stem) = local_path.file_stem().and_then(|stem| stem.to_str()) else {
            return Vec::new();
        };

        let Ok(entries) = std::fs::read_dir(parent) else {
            return Vec::new();
        };

        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        paths.sort();

        let exact_candidates: Vec<SubtitleCandidate> = paths
            .iter()
            .filter_map(|path| Self::sidecar_subtitle_candidate(video_stem, path.clone()))
            .collect();
        if !exact_candidates.is_empty() {
            return exact_candidates;
        }

        // If no same-basename sidecar exists, make every SRT in the media
        // directory available for manual correction (and rank the best guess
        // first for auto-selection). Other formats remain exact-only so a
        // broad fallback cannot surface unrelated attachment/font-heavy ASS files.
        let mut fuzzy_candidates: Vec<SubtitleCandidate> = paths
            .into_iter()
            .filter_map(|path| Self::fuzzy_sidecar_subtitle_candidate(video_stem, path))
            .collect();
        fuzzy_candidates.sort_by(|left, right| {
            right
                .match_confidence
                .cmp(&left.match_confidence)
                .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
        });
        fuzzy_candidates
    }

    async fn mapped_media_path_for_session(
        &self,
        item_id: &str,
        session: &Session,
        server: &Arc<dyn MediaServer>,
    ) -> Option<PathBuf> {
        let server_path = match session
            .now_playing
            .as_ref()
            .and_then(|np| np.path.as_deref())
            .map(str::to_owned)
        {
            Some(path) => Some(path),
            None => server
                .get_item_info(item_id, session.user_id.as_deref())
                .await
                .ok()
                .and_then(|item_info| item_info.path),
        }?;

        let config = self.config.read().await.clone();
        Some(config.map_path(&server_path))
    }

    async fn subtitle_candidates_for_session(
        &self,
        media_source_id: &str,
        item_id: &str,
        session: &Session,
        server: &Arc<dyn MediaServer>,
    ) -> Vec<SubtitleCandidate> {
        let mut candidates = self
            .server_subtitle_candidates(item_id, media_source_id, session, server)
            .await;

        if let Some(local_path) = self
            .mapped_media_path_for_session(item_id, session, server)
            .await
        {
            candidates.extend(Self::sidecar_subtitle_candidates(&local_path));
        }

        candidates
    }

    fn resolve_subtitle_candidate(
        candidates: &[SubtitleCandidate],
        target_lang: &str,
        override_candidate_id: Option<&str>,
        server_kind: MediaServerKind,
    ) -> (Option<SubtitleCandidate>, SubtitleSelectionMode) {
        if let Some(candidate_id) = override_candidate_id {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.id == candidate_id)
            {
                return (Some(candidate.clone()), SubtitleSelectionMode::Manual);
            }
        }

        // ABS has no subtitle stream selected by the player to use as a hint.
        // Prefer an exact media-basename sidecar as soon as the open session
        // exposes the item, even when playback has not started reporting yet.
        if server_kind == MediaServerKind::Audiobookshelf
            && let Some(candidate) = candidates.iter().find(|candidate| {
                candidate.source == SubtitleCandidateSource::Sidecar
                    && candidate.is_exact_file_match
            })
        {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.source == SubtitleCandidateSource::Sidecar
                && Self::language_matches_target(candidate.language.as_deref(), target_lang)
        }) {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates.iter().find(|candidate| {
            Self::language_matches_target(candidate.language.as_deref(), target_lang)
        }) {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.is_selected_in_session)
        {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates.iter().find(|candidate| candidate.is_default) {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.source == SubtitleCandidateSource::Sidecar && candidate.language.is_none()
        }) {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.source == SubtitleCandidateSource::Sidecar)
        {
            return (Some(candidate.clone()), SubtitleSelectionMode::Auto);
        }

        (candidates.first().cloned(), SubtitleSelectionMode::Auto)
    }

    async fn snapshot_active_track_into_history(&self, history_id: &str) -> usize {
        let maybe_track = self.subtitles.read().await.clone();
        let subtitle_count = maybe_track
            .as_ref()
            .map(|track| track.lines.len())
            .unwrap_or(0);

        {
            let mut subtitle_history = self.subtitle_history.write().await;
            if let Some(track) = maybe_track {
                subtitle_history.insert(history_id.to_string(), track);
            } else {
                subtitle_history.remove(history_id);
            }
        }

        {
            let mut history = self.history.write().await;
            if let Some(entry) = history.get_mut(history_id) {
                entry.subtitle_count = subtitle_count;
            }
        }

        subtitle_count
    }

    pub async fn select_subtitle_candidate(
        &self,
        candidate_id: Option<String>,
    ) -> anyhow::Result<()> {
        let _poll_guard = self.poll_lock.lock().await;

        let servers = self.servers.read().await.clone();
        if servers.is_empty() {
            anyhow::bail!("No media servers are configured");
        }

        let active_session_id = self
            .state
            .read()
            .await
            .active_session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No active session"))?;

        let (target_lang, native_lang) = {
            let config = self.config.read().await;
            (
                config.target_language.clone(),
                config.native_language.clone(),
            )
        };
        let sessions = self.collect_playback_sessions(servers).await;
        let active = sessions
            .into_iter()
            .find(|server_session| server_session.id() == active_session_id)
            .ok_or_else(|| anyhow::anyhow!("Active session is no longer available"))?;

        let Some(now_playing) = active.session.now_playing.as_ref() else {
            anyhow::bail!("The active session is not playing any media");
        };

        let history_id = scoped_history_id(active.kind, &now_playing.item_id);
        let media_source_id = now_playing
            .media_source_id
            .clone()
            .unwrap_or_else(|| format!("mediasource_{}", now_playing.item_id));
        let candidates = self
            .subtitle_candidates_for_session(
                &media_source_id,
                &now_playing.item_id,
                &active.session,
                &active.server,
            )
            .await;
        let requested_override = candidate_id;
        let (candidate, selection_mode) = Self::resolve_subtitle_candidate(
            &candidates,
            &target_lang,
            requested_override.as_deref(),
            active.kind,
        );

        if requested_override.is_some() && selection_mode != SubtitleSelectionMode::Manual {
            anyhow::bail!("That subtitle track is no longer available");
        }

        {
            let mut subtitle_override = self.selected_subtitle_candidate_override.write().await;
            *subtitle_override = if selection_mode == SubtitleSelectionMode::Manual {
                candidate.as_ref().map(|track| track.id.clone())
            } else {
                None
            };
        }

        {
            let mut subtitle_candidates = self.subtitle_candidates.write().await;
            *subtitle_candidates = candidates.clone();
        }

        {
            let mut state = self.state.write().await;
            if let Some(now_playing_state) = state.now_playing.as_mut() {
                if now_playing_state.history_id == history_id {
                    now_playing_state.subtitle_stream_index =
                        candidate.as_ref().and_then(|track| track.stream_index);
                    now_playing_state.subtitle_candidate_id =
                        candidate.as_ref().map(|track| track.id.clone());
                    now_playing_state.subtitle_selection_mode = selection_mode;
                    now_playing_state.subtitle_loading = true;
                }
            }
        }

        // Drop the previous track before announcing the load so clients never
        // render subtitles from the old selection under the new track label.
        *self.subtitles.write().await = None;
        self.clear_native_subtitles().await;
        let snapshot = self.state.read().await.clone();
        let _ = self.state_tx.send(snapshot);

        self.load_subtitles_for_item(
            &now_playing.item_id,
            &media_source_id,
            &active.session,
            &target_lang,
            &native_lang,
            &active.server,
            candidate.as_ref(),
        )
        .await;

        self.restore_subtitle_offset(&history_id).await;

        {
            let mut state = self.state.write().await;
            if let Some(now_playing_state) = state.now_playing.as_mut() {
                if now_playing_state.history_id == history_id {
                    now_playing_state.subtitle_loading = false;
                }
            }
        }

        self.snapshot_active_track_into_history(&history_id).await;
        let snapshot = self.state.read().await.clone();
        let _ = self.state_tx.send(snapshot);
        self.save_history(true).await;

        Ok(())
    }

    pub async fn poll_once(&self) {
        let _poll_guard = self.poll_lock.lock().await;

        let servers = self.servers.read().await.clone();
        if servers.is_empty() {
            self.playback_sessions.lock().await.observed.clear();
            {
                let mut state = self.state.write().await;
                state.sessions.clear();
                state.active_session_id = None;
                state.now_playing = None;
            }
            let mut subs = self.subtitles.write().await;
            *subs = None;
            drop(subs);
            self.clear_native_subtitles().await;
            let mut subtitle_candidates = self.subtitle_candidates.write().await;
            *subtitle_candidates = Vec::new();
            drop(subtitle_candidates);
            let mut subtitle_override = self.selected_subtitle_candidate_override.write().await;
            *subtitle_override = None;
            drop(subtitle_override);
            let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
            *loaded_candidate = None;
            drop(loaded_candidate);
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
            return;
        }

        let config = self.config.read().await;
        let target_lang = config.target_language.clone();
        let native_lang = config.native_language.clone();
        drop(config);

        let mut sessions = self.collect_playback_sessions(servers).await;
        sessions.sort_by(|left, right| {
            compare_auto_session_priority(&left.session, &right.session, &target_lang)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id().cmp(&right.id()))
        });

        // Keep the device picker stable while automatic selection ranks the
        // live sessions separately. Check-ins must not move clickable rows.
        let mut summaries: Vec<SessionSummary> = sessions
            .iter()
            .filter(|s| s.session.now_playing.is_some())
            .map(|s| {
                let np = s.session.now_playing.as_ref().unwrap();
                let is_target = np.has_audio_language(&target_lang);
                SessionSummary {
                    id: s.id(),
                    remote_session_id: s.session.id.clone(),
                    server_kind: s.kind,
                    client: s.session.client.clone(),
                    device_name: s.session.device_name.clone(),
                    user_name: s.session.user_name.clone(),
                    title: Some(np.display_title()),
                    is_target_language: is_target,
                }
            })
            .collect();
        summaries.sort_by(|left, right| {
            left.server_kind
                .cmp(&right.server_kind)
                .then_with(|| left.device_name.cmp(&right.device_name))
                .then_with(|| left.client.cmp(&right.client))
                .then_with(|| left.id.cmp(&right.id))
        });

        // Determine active session
        let user_selected = self.selected_session_id.read().await.clone();
        let active_session = if let Some(ref sel_id) = user_selected {
            sessions.iter().find(|s| s.id() == *sel_id).cloned()
        } else {
            // Check-ins from another device must not steal the selection,
            // including when both devices are paused on the same episode.
            // A paused session still yields to actual playback elsewhere.
            let has_playing_session = sessions.iter().any(|s| {
                s.reported && s.session.now_playing.is_some() && !s.session.play_state.is_paused
            });
            let previous_active_id = self.state.read().await.active_session_id.clone();
            let sticky = previous_active_id.as_deref().and_then(|id| {
                sessions
                    .iter()
                    .find(|s| s.id() == id)
                    .filter(|previous| {
                        (!previous.session.play_state.is_paused || !has_playing_session)
                        // Follow a newly started episode immediately. A known
                        // tab checking in again must not steal the selection.
                        && (previous.reported || !sessions.iter().any(|current| {
                            current.reported && current.newly_observed
                                && !current.session.play_state.is_paused
                                && current.same_connection(previous)
                        }))
                    })
                    .cloned()
            });

            sticky.or_else(|| {
                // Sessions are pre-sorted with playing first, then target
                // language and freshness. Paused sessions are a final fallback.
                sessions
                    .iter()
                    .filter(|s| s.reported)
                    .find(|s| s.session.now_playing.is_some())
                    .cloned()
            })
        };

        // Collect info we need while holding the lock, then release
        let previous_loaded_candidate_id = self.loaded_subtitle_candidate_id.read().await.clone();
        let has_loaded_subtitle_track = self.subtitles.read().await.is_some();
        let previous_subtitle_override = self
            .selected_subtitle_candidate_override
            .read()
            .await
            .clone();
        let mut needs_subtitle_load: Option<(
            bool,
            String,
            String,
            String,
            Option<SubtitleCandidate>,
            ServerSession,
        )> = None;
        let mut next_candidates = Vec::new();
        let mut next_override_candidate_id = None;
        let active_item_ended;

        {
            let mut state = self.state.write().await;
            state.sessions = summaries;

            if let Some(active) = active_session
                .clone()
                .filter(|s| s.session.now_playing.is_some())
            {
                let prev_history_id = state.now_playing.as_ref().map(|np| np.history_id.clone());

                let session = &active.session;
                let np = session.now_playing.as_ref().unwrap();
                let item_id = np.item_id.clone();
                let history_id = scoped_history_id(active.kind, &item_id);
                let media_source_id = np
                    .media_source_id
                    .clone()
                    .unwrap_or_else(|| format!("mediasource_{}", np.item_id));
                let item_changed = prev_history_id.as_deref() != Some(&history_id);
                active_item_ended = item_changed && prev_history_id.is_some();
                let requested_override = if item_changed {
                    None
                } else {
                    previous_subtitle_override.clone()
                };
                let candidates = self
                    .subtitle_candidates_for_session(
                        &media_source_id,
                        &item_id,
                        session,
                        &active.server,
                    )
                    .await;
                let (candidate, selection_mode) = Self::resolve_subtitle_candidate(
                    &candidates,
                    &target_lang,
                    requested_override.as_deref(),
                    active.kind,
                );
                let selected_stream_index = candidate.as_ref().and_then(|track| track.stream_index);
                let selected_candidate_id = candidate.as_ref().map(|track| track.id.clone());
                let subtitle_loading = item_changed
                    || selected_candidate_id != previous_loaded_candidate_id
                    || (selected_candidate_id.is_some() && !has_loaded_subtitle_track);

                next_candidates = candidates;
                next_override_candidate_id = if selection_mode == SubtitleSelectionMode::Manual {
                    selected_candidate_id.clone()
                } else {
                    None
                };

                // ── Audio track resolution ──
                let audio_user_override = if item_changed {
                    None
                } else {
                    *self.selected_audio_track.read().await
                };
                let audio_tracks_list = Self::audio_tracks_from_streams(&np.media_streams);
                let (resolved_audio_index, audio_resolution) = Self::resolve_audio_track(
                    &audio_tracks_list,
                    &target_lang,
                    audio_user_override,
                );

                if item_changed {
                    *self.audio_tracks.write().await = audio_tracks_list;
                    *self.selected_audio_track.write().await = resolved_audio_index;
                    *self.audio_track_resolution.write().await = audio_resolution;
                }

                state.active_session_id = Some(active.id());
                state.now_playing = Some(NowPlayingState {
                    history_id: history_id.clone(),
                    server_kind: active.kind,
                    item_id: item_id.clone(),
                    title: np.display_title(),
                    series_name: np.series_name.clone(),
                    position_ms: session.position_ms().unwrap_or(0),
                    duration_ms: np.run_time_ticks.map(|t| t / 10_000),
                    is_paused: session.play_state.is_paused,
                    supports_remote_control: session.supports_remote_control,
                    subtitle_stream_index: selected_stream_index,
                    subtitle_candidate_id: selected_candidate_id.clone(),
                    subtitle_selection_mode: selection_mode,
                    media_source_id: media_source_id.clone(),
                    file_path: np.path.clone(),
                    audio_stream_index: resolved_audio_index,
                    subtitle_offset_ms: 0,
                    subtitle_loading,
                });

                if subtitle_loading {
                    needs_subtitle_load = Some((
                        item_changed,
                        history_id,
                        item_id,
                        media_source_id,
                        candidate,
                        active.clone(),
                    ));
                }
            } else {
                active_item_ended = state.now_playing.is_some();
                state.active_session_id = None;
                state.now_playing = None;
            }
        } // write lock released here

        {
            let mut subtitle_candidates = self.subtitle_candidates.write().await;
            *subtitle_candidates = next_candidates;
        }

        {
            let mut subtitle_override = self.selected_subtitle_candidate_override.write().await;
            *subtitle_override = next_override_candidate_id;
        }

        if active_session.is_none() {
            let mut subs = self.subtitles.write().await;
            *subs = None;
            drop(subs);
            self.clear_native_subtitles().await;
            let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
            *loaded_candidate = None;
            drop(loaded_candidate);
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
            if active_item_ended {
                debug!("Active playback ended; flushing session history to SQLite");
                self.flush_history().await;
                self.queue_automatic_tadoku_export();
            }
            return;
        }

        // Persist the latest position before subtitle work can delay the poll or
        // force a save that resets the position throttle.
        {
            let playback_activity_at = active_session
                .as_ref()
                .map(|active| session_playback_activity_at(&active.session))
                .unwrap_or_else(chrono::Utc::now);
            let state = self.state.read().await;
            if let Some(ref np) = state.now_playing {
                let mut hist = self.history.write().await;
                if let Some(entry) = hist.get_mut(&np.history_id) {
                    let position_changed = entry.last_position_ms != np.position_ms;
                    entry.last_position_ms = np.position_ms;
                    // A visible paused session is intentionally retained for
                    // a few minutes. Do not let those unchanged polls restart
                    // the long-form inactivity timer.
                    if !np.is_paused || position_changed {
                        entry.last_seen = playback_activity_at;
                    }
                }
            }
        }

        // Throttled save for position updates (at most once every 5 s)
        self.save_history(false).await;

        // Load subtitles outside the lock
        if let Some((item_changed, history_id, item_id, media_source_id, candidate, active)) =
            needs_subtitle_load
        {
            // Publish the new item and its loading state before fetching/parsing
            // the track. Large sidecars (especially audiobook ASS files) can
            // otherwise leave the UI looking frozen for several seconds.
            *self.subtitles.write().await = None;
            self.clear_native_subtitles().await;
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);

            let display_title = active
                .session
                .now_playing
                .as_ref()
                .map(|np| np.display_title())
                .unwrap_or_else(|| item_id.clone());
            let series_name = active
                .session
                .now_playing
                .as_ref()
                .and_then(|np| np.series_name.clone());
            if item_changed {
                info!(
                    "New item detected: {} ({}) on {}",
                    display_title, item_id, active.kind
                );
            }
            self.load_subtitles_for_item(
                &item_id,
                &media_source_id,
                &active.session,
                &target_lang,
                &native_lang,
                &active.server,
                candidate.as_ref(),
            )
            .await;

            self.restore_subtitle_offset(&history_id).await;

            let sub_count = self.snapshot_active_track_into_history(&history_id).await;

            {
                let mut state = self.state.write().await;
                if let Some(now_playing) = state.now_playing.as_mut()
                    && now_playing.history_id == history_id
                {
                    now_playing.subtitle_loading = false;
                }
            }
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);

            if item_changed {
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
                    series_name: series_name.clone(),
                    media_source_id: media_source_id.clone(),
                    file_path,
                    duration_ms: dur,
                    subtitle_count: sub_count,
                    audio_languages: active
                        .session
                        .now_playing
                        .as_ref()
                        .map(|now_playing| {
                            now_playing
                                .media_streams
                                .iter()
                                .filter(|stream| stream.stream_type == StreamType::Audio)
                                .filter_map(|stream| stream.language.clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                    last_position_ms: pos,
                    last_seen: session_playback_activity_at(&active.session),
                };
                self.history.write().await.insert(history_id.clone(), entry);
            }

            // Broadcast before the potentially-slow SQLite write so listeners
            // (e.g. the Anki poller) see the live session without waiting for
            // the full subtitle history to be flushed to disk.
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);

            self.save_history(true).await;
        }

        if active_item_ended {
            debug!("Previous playback item unloaded; checking automatic Tadoku sync");
            self.queue_automatic_tadoku_export();
        }

        // Broadcast final state (covers position-only polls where no subtitle
        // reload occurred; harmless double-send on new-item polls).
        let snapshot = self.state.read().await.clone();
        let _ = self.state_tx.send(snapshot);
    }

    pub async fn handle_plex_playing_event(
        &self,
        client_identifier: &str,
        rating_key: &str,
        playback_session_id: Option<&str>,
        view_offset_ms: Option<i64>,
        player_state: &str,
    ) {
        let scoped_id = scoped_playback_id(
            MediaServerKind::Plex,
            client_identifier,
            rating_key,
            playback_session_id,
        );
        let mut snapshot = None;
        let mut history_update = None;
        let mut should_poll = false;

        {
            let mut state = self.state.write().await;
            let known_session = state.sessions.iter().any(|session| session.id == scoped_id);
            let active_matches = state.active_session_id.as_deref() == Some(scoped_id.as_str());

            if player_state == "stopped" {
                should_poll = active_matches || known_session;
            } else if active_matches {
                match state.now_playing.as_mut() {
                    Some(now_playing)
                        if now_playing.server_kind == MediaServerKind::Plex
                            && now_playing.item_id == rating_key =>
                    {
                        if let Some(position_ms) = view_offset_ms {
                            now_playing.position_ms = position_ms;
                        }

                        match player_state {
                            "playing" => now_playing.is_paused = false,
                            "paused" => now_playing.is_paused = true,
                            _ => {}
                        }

                        history_update =
                            Some((now_playing.history_id.clone(), now_playing.position_ms));
                        snapshot = Some(state.clone());
                    }
                    Some(_) | None => {
                        should_poll = true;
                    }
                }
            } else if !known_session {
                should_poll = true;
            }
        }

        if let Some((history_id, position_ms)) = history_update {
            let mut history = self.history.write().await;
            if let Some(entry) = history.get_mut(&history_id) {
                let position_changed = entry.last_position_ms != position_ms;
                entry.last_position_ms = position_ms;
                if player_state == "playing" || position_changed {
                    entry.last_seen = chrono::Utc::now();
                }
            }
            drop(history);
            self.save_history(false).await;
        }

        if let Some(snapshot) = snapshot {
            let _ = self.state_tx.send(snapshot);
        }

        if should_poll {
            self.poll_once().await;
        }
    }

    fn remote_command_target_reached(
        snapshot: &SessionState,
        session_id: &str,
        target_position_ms: Option<i64>,
        target_paused: Option<bool>,
    ) -> bool {
        if snapshot.active_session_id.as_deref() != Some(session_id) {
            return false;
        }

        let Some(now_playing) = snapshot.now_playing.as_ref() else {
            return false;
        };

        let pause_synced = target_paused
            .map(|paused| now_playing.is_paused == paused)
            .unwrap_or(true);
        let position_synced = target_position_ms
            .map(|target| (now_playing.position_ms - target).abs() <= 1_500)
            .unwrap_or(true);

        pause_synced && position_synced
    }

    pub async fn force_refresh_after_remote_command(
        &self,
        session_id: String,
        target_position_ms: Option<i64>,
        target_paused: Option<bool>,
    ) -> bool {
        const ATTEMPTS: usize = 6;

        for attempt in 0..ATTEMPTS {
            self.poll_once().await;

            let snapshot = self.state.read().await.clone();
            if Self::remote_command_target_reached(
                &snapshot,
                &session_id,
                target_position_ms,
                target_paused,
            ) {
                return true;
            }

            if snapshot.active_session_id.as_deref() != Some(session_id.as_str()) {
                break;
            }

            if snapshot.now_playing.is_none() {
                break;
            }

            if attempt + 1 < ATTEMPTS {
                let delay_ms = if attempt < 2 { 75 } else { 150 };
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }

        let snapshot = self.state.read().await.clone();
        Self::remote_command_target_reached(
            &snapshot,
            &session_id,
            target_position_ms,
            target_paused,
        )
    }

    /// Fetch and parse a single subtitle candidate into a [`SubtitleTrack`].
    /// Returns `None` if the source could not be read.
    async fn fetch_subtitle_track_for_candidate(
        &self,
        item_id: &str,
        media_source_id: &str,
        candidate: &SubtitleCandidate,
        server: &Arc<dyn MediaServer>,
    ) -> Option<SubtitleTrack> {
        match candidate.source {
            SubtitleCandidateSource::Server => {
                let stream_index = candidate.stream_index?;
                debug!(
                    "Fetching subtitles: item={} msid={} index={}",
                    item_id, media_source_id, stream_index
                );
                match server
                    .get_subtitles(item_id, media_source_id, stream_index, SubtitleFormat::Vtt)
                    .await
                {
                    Ok(content) => Some(parse_subtitle(&content, None)),
                    Err(e) => {
                        warn!(
                            "Failed to fetch subtitles from API for stream {}: {}",
                            stream_index, e
                        );
                        None
                    }
                }
            }
            SubtitleCandidateSource::Sidecar => {
                let sidecar_path = candidate.local_path.as_ref()?;
                match std::fs::read_to_string(sidecar_path) {
                    Ok(content) => Some(parse_subtitle(&content, sidecar_path.to_str())),
                    Err(error) => {
                        warn!(
                            "Failed to read subtitle sidecar {:?}: {}",
                            sidecar_path, error
                        );
                        None
                    }
                }
            }
        }
    }

    async fn clear_native_subtitles(&self) {
        let mut native = self.native_subtitles.write().await;
        *native = None;
        drop(native);
        let mut loaded = self.loaded_native_candidate_id.write().await;
        *loaded = None;
    }

    async fn load_subtitles_for_item(
        &self,
        item_id: &str,
        media_source_id: &str,
        session: &Session,
        target_lang: &str,
        native_lang: &str,
        server: &Arc<dyn MediaServer>,
        candidate: Option<&SubtitleCandidate>,
    ) {
        let mut loaded_id: Option<String> = None;
        let mut track: Option<SubtitleTrack> = None;

        // 1. Try the resolved candidate (Server stream or sidecar file).
        if let Some(track_candidate) = candidate {
            if let Some(fetched) = self
                .fetch_subtitle_track_for_candidate(
                    item_id,
                    media_source_id,
                    track_candidate,
                    server,
                )
                .await
            {
                info!(
                    "Loaded {} subtitle lines for {}",
                    fetched.lines.len(),
                    track_candidate.id
                );
                loaded_id = Some(track_candidate.id.clone());
                track = Some(fetched);
            }
        } else {
            debug!(
                "No loadable subtitle track candidate selected for item {}; trying fallback sources",
                item_id
            );
        }

        let local_path = self
            .mapped_media_path_for_session(item_id, session, server)
            .await;

        // 2. Disk fallback when nothing loaded yet.
        if track.is_none() {
            if let Some(local_path) = local_path.as_ref() {
                debug!("Trying disk fallback at: {:?}", local_path);

                let fallback_candidates = Self::sidecar_subtitle_candidates(local_path);
                let sidecar_fallback = if let Some(track_candidate) = candidate {
                    fallback_candidates.iter().find(|sidecar| {
                        track_candidate.source == SubtitleCandidateSource::Server
                            && (Self::language_matches_target(
                                sidecar.language.as_deref(),
                                target_lang,
                            ) || (track_candidate.language.is_some()
                                && sidecar.language == track_candidate.language))
                    })
                } else {
                    fallback_candidates.iter().find(|sidecar| {
                        Self::language_matches_target(sidecar.language.as_deref(), target_lang)
                    })
                }
                .or_else(|| {
                    fallback_candidates
                        .iter()
                        .find(|sidecar| sidecar.language.is_none())
                })
                .or_else(|| fallback_candidates.first());

                if let Some(sidecar_candidate) = sidecar_fallback {
                    if let Some(sidecar_path) = sidecar_candidate.local_path.as_ref() {
                        if let Ok(content) = std::fs::read_to_string(sidecar_path) {
                            let parsed = parse_subtitle(&content, sidecar_path.to_str());
                            info!(
                                "Loaded {} subtitle lines from disk fallback: {:?}",
                                parsed.lines.len(),
                                sidecar_path
                            );
                            loaded_id = Some(sidecar_candidate.id.clone());
                            track = Some(parsed);
                        }
                    }
                }
            }
        }

        if track.is_none() {
            warn!("Could not load subtitles from any source");
        }

        // Store the resolved target track.
        {
            let mut subs = self.subtitles.write().await;
            *subs = track.clone();
        }
        {
            let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
            *loaded_candidate = loaded_id.clone();
        }

        // Load a native-language track alongside it (live playback only).
        self.load_native_subtitles_for_item(
            item_id,
            media_source_id,
            native_lang,
            server,
            track.as_ref(),
            loaded_id.as_deref(),
        )
        .await;
    }

    /// Pick and load the best native-language subtitle track for the active item.
    ///
    /// Native subtitles use their own raw timings; per-line pairing in the UI
    /// matches them against the (offset-baked) target line times, so applying a
    /// target offset can drift the pairing — acceptable for v1.
    async fn load_native_subtitles_for_item(
        &self,
        item_id: &str,
        media_source_id: &str,
        native_lang: &str,
        server: &Arc<dyn MediaServer>,
        target_track: Option<&SubtitleTrack>,
        target_candidate_id: Option<&str>,
    ) {
        let Some(target_track) = target_track else {
            self.clear_native_subtitles().await;
            return;
        };

        let native_candidates: Vec<SubtitleCandidate> = self
            .subtitle_candidates
            .read()
            .await
            .iter()
            .filter(|c| Some(c.id.as_str()) != target_candidate_id)
            .filter(|c| Self::language_matches_target(c.language.as_deref(), native_lang))
            .cloned()
            .collect();

        if native_candidates.is_empty() {
            self.clear_native_subtitles().await;
            return;
        }

        // With a single candidate, skip the cost of scoring; otherwise fetch
        // each and keep the one that best aligns with the target track.
        let chosen = if native_candidates.len() == 1 {
            let candidate = native_candidates.into_iter().next().unwrap();
            self.fetch_subtitle_track_for_candidate(item_id, media_source_id, &candidate, server)
                .await
                .map(|track| (candidate, track))
        } else {
            let mut best: Option<(SubtitleCandidate, SubtitleTrack, f64)> = None;
            for candidate in native_candidates {
                if let Some(track) = self
                    .fetch_subtitle_track_for_candidate(
                        item_id,
                        media_source_id,
                        &candidate,
                        server,
                    )
                    .await
                {
                    let score = crate::subtitle::score_native_candidate(target_track, &track);
                    if best.as_ref().map(|(_, _, s)| score > *s).unwrap_or(true) {
                        best = Some((candidate, track, score));
                    }
                }
            }
            best.map(|(candidate, track, _)| (candidate, track))
        };

        match chosen {
            Some((candidate, track)) => {
                info!(
                    "Loaded {} native subtitle lines from {}",
                    track.lines.len(),
                    candidate.id
                );
                let mut native = self.native_subtitles.write().await;
                *native = Some(track);
                drop(native);
                let mut loaded = self.loaded_native_candidate_id.write().await;
                *loaded = Some(candidate.id);
            }
            None => self.clear_native_subtitles().await,
        }
    }
}

/// Run the session polling loop.
pub async fn run_session_poller(manager: Arc<SessionManager>) {
    loop {
        manager.poll_once().await;
        tokio::time::sleep(manager.poll_interval().await).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MediaServerKind;
    use crate::media_server::{MediaStream, MediaUser, NowPlaying, PlayState, Session, StreamType};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_playing(name: &str, media_type: &str, series_name: Option<&str>) -> NowPlaying {
        NowPlaying {
            item_id: "item-1".to_string(),
            name: name.to_string(),
            series_name: series_name.map(str::to_string),
            season_index: Some(1),
            episode_index: series_name.map(|_| 1),
            media_type: media_type.to_string(),
            run_time_ticks: None,
            media_streams: Vec::new(),
            media_source_id: None,
            path: None,
        }
    }

    fn playback_session(
        id: &str,
        is_paused: bool,
        language: &str,
        last_activity_at_ms: Option<i64>,
    ) -> Session {
        let mut item = now_playing(id, "Episode", Some("Series"));
        item.media_streams.push(MediaStream {
            index: 0,
            stream_type: StreamType::Audio,
            codec: None,
            language: Some(language.to_string()),
            display_title: None,
            is_default: true,
            is_external: false,
            is_text_subtitle_stream: false,
            title: None,
        });

        Session {
            id: id.to_string(),
            playback_session_id: None,
            client: "Test".to_string(),
            device_name: id.to_string(),
            user_name: None,
            user_id: None,
            last_activity_at_ms,
            now_playing: Some(item),
            play_state: PlayState {
                can_seek: true,
                is_paused,
                position_ticks: Some(0),
                audio_stream_index: Some(0),
                subtitle_stream_index: None,
            },
            supports_remote_control: true,
        }
    }

    struct TestServer(RwLock<Vec<Session>>);

    #[async_trait::async_trait]
    impl MediaServer for TestServer {
        fn kind(&self) -> MediaServerKind {
            MediaServerKind::Jellyfin
        }
        async fn get_sessions(&self) -> anyhow::Result<Vec<Session>> {
            Ok(self.0.read().await.clone())
        }
        async fn get_users(&self) -> anyhow::Result<Vec<MediaUser>> {
            Ok(vec![])
        }
        async fn get_item_info(&self, _: &str, _: Option<&str>) -> anyhow::Result<ItemInfo> {
            anyhow::bail!("No item details")
        }
        async fn get_subtitles(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: SubtitleFormat,
        ) -> anyhow::Result<String> {
            anyhow::bail!("Subtitle unavailable; retry on the next poll")
        }
        fn get_stream_url(&self, _: &str, _: &str) -> String {
            String::new()
        }
        async fn seek_session(&self, _: &str, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn pause_session(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn unpause_session(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn playback_position_is_persisted_during_subtitle_reload() {
        let directory =
            std::env::temp_dir().join(format!("nagare-position-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let db = Arc::new(
            AppDatabase::new(directory.join("test.db"), None)
                .await
                .unwrap(),
        );
        let mut session = playback_session("player", false, "jpn", None);
        session
            .now_playing
            .as_mut()
            .unwrap()
            .media_streams
            .push(MediaStream {
                index: 1,
                stream_type: StreamType::Subtitle,
                codec: Some("srt".into()),
                language: Some("jpn".into()),
                display_title: None,
                is_default: true,
                is_external: true,
                is_text_subtitle_stream: true,
                title: None,
            });
        let server = Arc::new(TestServer(RwLock::new(vec![session])));
        let mut servers = ServerMap::new();
        servers.insert(MediaServerKind::Jellyfin, server.clone());
        let (tx, _rx) = watch::channel(SessionState {
            sessions: vec![],
            active_session_id: None,
            now_playing: None,
        });
        let manager = SessionManager::new(
            Arc::new(RwLock::new(servers)),
            Arc::new(RwLock::new(Config::default())),
            tx,
            directory.clone(),
            db.clone(),
        )
        .await
        .unwrap();

        manager.poll_once().await;
        for position_ms in [12_000, 24_000, 8_000] {
            server.0.write().await[0].play_state.position_ticks = Some(position_ms * 10_000);
            // Even within the throttle window, a forced subtitle save must
            // contain the current position, including backward seeks.
            manager.poll_once().await;
            let (history, _) = db
                .load_session_history(
                    directory.join("history.json"),
                    directory.join("subtitle_history.json"),
                )
                .await
                .unwrap();
            assert_eq!(history["jellyfin|item-1"].last_position_ms, position_ms);
            assert!(manager.state.read().await.now_playing.is_some());
        }
        drop(manager);
        drop(db);
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn concurrent_devices_keep_selection_position_and_list_order_stable() {
        let directory =
            std::env::temp_dir().join(format!("nagare-devices-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let db = Arc::new(
            AppDatabase::new(directory.join("test.db"), None)
                .await
                .unwrap(),
        );
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut tv = playback_session("tv", false, "jpn", Some(now_ms));
        let mut browser = playback_session("browser", false, "jpn", Some(now_ms - 1));
        tv.device_name = "Firefox".into();
        browser.device_name = "Firefox".into();
        // Both devices use the same provider item ID, but have different clocks.
        tv.play_state.position_ticks = Some(600_000 * 10_000);
        browser.play_state.position_ticks = Some(420_000 * 10_000);
        let server = Arc::new(TestServer(RwLock::new(vec![tv.clone(), browser.clone()])));
        let mut servers = ServerMap::new();
        servers.insert(MediaServerKind::Jellyfin, server.clone());
        let (tx, _rx) = watch::channel(SessionState {
            sessions: vec![],
            active_session_id: None,
            now_playing: None,
        });
        let manager = SessionManager::new(
            Arc::new(RwLock::new(servers)),
            Arc::new(RwLock::new(Config::default())),
            tx,
            directory.clone(),
            db.clone(),
        )
        .await
        .unwrap();
        manager.poll_once().await;
        let original_order: Vec<_> = manager
            .state
            .read()
            .await
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();

        // Alternating check-ins and response order must not move either the
        // selected device or the buttons under the user's pointer, even paused.
        for paused in [false, true] {
            tv.play_state.is_paused = paused;
            browser.play_state.is_paused = paused;
            for poll in 0..6 {
                tv.last_activity_at_ms = Some(now_ms + poll * 2);
                browser.last_activity_at_ms = Some(now_ms + poll * 2 + 1);
                if poll % 2 == 0 {
                    std::mem::swap(
                        &mut tv.last_activity_at_ms,
                        &mut browser.last_activity_at_ms,
                    );
                }
                let mut sessions = vec![tv.clone(), browser.clone()];
                if poll % 2 == 0 {
                    sessions.reverse();
                }
                *server.0.write().await = sessions;
                manager.poll_once().await;
                let state = manager.state.read().await;
                assert_eq!(
                    state.active_session_id.as_deref(),
                    Some("jellyfin|tv|item-1")
                );
                assert_eq!(state.now_playing.as_ref().unwrap().position_ms, 600_000);
                assert_eq!(
                    state
                        .sessions
                        .iter()
                        .map(|session| session.id.clone())
                        .collect::<Vec<_>>(),
                    original_order
                );
                assert_eq!(
                    manager.history.read().await["jellyfin|item-1"].last_position_ms,
                    600_000
                );
            }
        }

        // A playing session still takes over from a paused automatic selection.
        browser.play_state.is_paused = false;
        *server.0.write().await = vec![tv.clone(), browser.clone()];
        manager.poll_once().await;
        assert_eq!(
            manager.state.read().await.active_session_id.as_deref(),
            Some("jellyfin|browser|item-1")
        );

        // An explicit selection stays on that device, including while paused.
        manager
            .select_session(Some("jellyfin|tv|item-1".into()))
            .await;
        manager.poll_once().await;
        assert_eq!(
            manager.state.read().await.active_session_id.as_deref(),
            Some("jellyfin|tv|item-1")
        );
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            600_000
        );
        manager
            .select_session(Some("jellyfin|browser|item-1".into()))
            .await;
        manager.poll_once().await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            420_000
        );

        // Automatic selection releases a device that unloads the episode.
        manager.select_session(None).await;
        browser.now_playing = None;
        *server.0.write().await = vec![browser, tv];
        manager.poll_once().await;
        assert_eq!(
            manager.state.read().await.active_session_id.as_deref(),
            Some("jellyfin|tv|item-1")
        );

        // Expired sessions must not be kept alive by the sticky selection.
        server.0.write().await[1].last_activity_at_ms =
            Some(now_ms - PAUSED_SESSION_VISIBLE_AFTER.as_millis() as i64 - 1);
        manager.poll_once().await;
        assert!(manager.state.read().await.active_session_id.is_none());
        drop(manager);
        drop(db);
        fs::remove_dir_all(directory).unwrap();
    }

    async fn session_test_manager(
        kind: MediaServerKind,
        sessions: Vec<Session>,
    ) -> (SessionManager, Arc<TestServer>, PathBuf) {
        let directory = std::env::temp_dir().join(format!("nagare-tabs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let db = Arc::new(
            AppDatabase::new(directory.join("test.db"), None)
                .await
                .unwrap(),
        );
        let server = Arc::new(TestServer(RwLock::new(sessions)));
        let mut servers = ServerMap::new();
        servers.insert(kind, server.clone());
        let (tx, _rx) = watch::channel(SessionState {
            sessions: vec![],
            active_session_id: None,
            now_playing: None,
        });
        let manager = SessionManager::new(
            Arc::new(RwLock::new(servers)),
            Arc::new(RwLock::new(Config::default())),
            tx,
            directory.clone(),
            db,
        )
        .await
        .unwrap();
        (manager, server, directory)
    }

    fn browser_tab(item_id: &str, paused: bool, position_ms: i64) -> Session {
        let mut session = playback_session("browser", paused, "jpn", None);
        session.client = "Web".into();
        session.device_name = "Firefox".into();
        let item = session.now_playing.as_mut().unwrap();
        item.item_id = item_id.into();
        item.name = item_id.into();
        session.play_state.position_ticks = Some(position_ms * 10_000);
        session
    }

    #[tokio::test]
    async fn shared_browser_id_keeps_items_and_manual_selection_stable() {
        for kind in [
            MediaServerKind::Jellyfin,
            MediaServerKind::Emby,
            MediaServerKind::Plex,
        ] {
            let first = browser_tab("episode-a", true, 4_355);
            let second = browser_tab("episode-b", true, 1_446_070);
            let (manager, server, directory) =
                session_test_manager(kind, vec![first.clone()]).await;
            manager.poll_once().await;
            let first_id = format!("{kind}|browser|episode-a");
            let second_id = format!("{kind}|browser|episode-b");

            // Reproduce Jellyfin's single row alternating between two paused
            // Firefox tabs. Neither selection nor playback position may bounce.
            for manual in [false, true] {
                if manual {
                    manager.select_session(Some(second_id.clone())).await;
                }
                for poll in 0..12 {
                    *server.0.write().await = vec![if poll % 2 == 0 {
                        second.clone()
                    } else {
                        first.clone()
                    }];
                    manager.poll_once().await;
                    let state = manager.state.read().await;
                    assert_eq!(
                        state.active_session_id.as_deref(),
                        Some(if manual {
                            second_id.as_str()
                        } else {
                            first_id.as_str()
                        })
                    );
                    let playing = state.now_playing.as_ref().unwrap();
                    assert_eq!(
                        playing.item_id,
                        if manual { "episode-b" } else { "episode-a" }
                    );
                    assert_eq!(playing.position_ms, if manual { 1_446_070 } else { 4_355 });
                    assert!(!playing.supports_remote_control);
                    assert_eq!(state.active_remote_session_id(), Some("browser"));
                    assert_eq!(
                        state
                            .sessions
                            .iter()
                            .map(|session| &session.id)
                            .collect::<Vec<_>>(),
                        vec![&first_id, &second_id]
                    );
                }
            }
            // Subtitle selection must resolve the chosen item from the same
            // observations, even while the provider row describes its sibling.
            *server.0.write().await = vec![first.clone()];
            manager.select_subtitle_candidate(None).await.unwrap();
            assert_eq!(
                manager
                    .state
                    .read()
                    .await
                    .now_playing
                    .as_ref()
                    .unwrap()
                    .item_id,
                "episode-b"
            );
            let history = manager.history.read().await;
            assert_eq!(
                history[&format!("{kind}|episode-a")].last_position_ms,
                4_355
            );
            assert_eq!(
                history[&format!("{kind}|episode-b")].last_position_ms,
                1_446_070
            );
            drop(history);

            // Missing tab observations expire, even while the shared device is
            // still checking in. Commands then use the original provider ID.
            manager
                .playback_sessions
                .lock()
                .await
                .observed
                .get_mut(&second_id)
                .unwrap()
                .1 = Instant::now() - SHARED_SESSION_RETENTION;
            *server.0.write().await = vec![first.clone()];
            manager.poll_once().await;
            assert!(manager.state.read().await.active_session_id.is_none());
            manager.select_session(None).await;
            manager.poll_once().await;
            let state = manager.state.read().await;
            assert_eq!(state.sessions.len(), 1);
            assert_eq!(state.active_remote_session_id(), Some("browser"));
            assert!(state.now_playing.as_ref().unwrap().supports_remote_control);
            drop(state);

            // An explicit unload removes cached items immediately.
            let mut unloaded = first;
            unloaded.now_playing = None;
            *server.0.write().await = vec![unloaded];
            manager.poll_once().await;
            assert!(manager.state.read().await.sessions.is_empty());
            drop(manager);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn new_playing_item_takes_over_but_known_tab_checkins_do_not() {
        let first = browser_tab("episode-a", false, 50_000);
        let second = browser_tab("episode-b", false, 1_000);
        let (manager, server, directory) =
            session_test_manager(MediaServerKind::Jellyfin, vec![first.clone()]).await;
        manager.poll_once().await;
        *server.0.write().await = vec![second.clone()];
        manager.poll_once().await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .item_id,
            "episode-b"
        );
        for poll in 0..6 {
            *server.0.write().await = vec![if poll % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            }];
            manager.poll_once().await;
            assert_eq!(
                manager
                    .state
                    .read()
                    .await
                    .now_playing
                    .as_ref()
                    .unwrap()
                    .item_id,
                "episode-b"
            );
        }
        let mut paused = second;
        paused.play_state.is_paused = true;
        *server.0.write().await = vec![paused];
        manager.poll_once().await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .item_id,
            "episode-b"
        );
        *server.0.write().await = vec![first];
        manager.poll_once().await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .item_id,
            "episode-a"
        );
        *server.0.write().await = vec![];
        manager.poll_once().await;
        assert!(manager.state.read().await.sessions.is_empty());
        drop(manager);
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn plex_websocket_only_updates_the_selected_playback_instance() {
        let mut first = browser_tab("episode", false, 10_000);
        first.playback_session_id = Some("11".into());
        let mut second = browser_tab("episode", false, 80_000);
        second.playback_session_id = Some("22".into());
        let (manager, server, directory) =
            session_test_manager(MediaServerKind::Plex, vec![first, second.clone()]).await;
        manager.poll_once().await;
        assert_eq!(manager.state.read().await.sessions.len(), 2);
        manager
            .handle_plex_playing_event("browser", "episode", Some("22"), Some(81_000), "paused")
            .await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            10_000
        );
        manager
            .handle_plex_playing_event("browser", "episode", Some("11"), Some(11_000), "playing")
            .await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            11_000
        );
        // Old servers/events without an instance key fall back to polling,
        // instead of applying a possibly unrelated tab's playback clock.
        manager
            .handle_plex_playing_event("browser", "episode", None, Some(999_000), "playing")
            .await;
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            10_000
        );
        // A vanished Plex instance is not retained merely because its sibling
        // shares the same machineIdentifier and item.
        *server.0.write().await = vec![second];
        manager.poll_once().await;
        assert_eq!(manager.state.read().await.sessions.len(), 1);
        assert_eq!(
            manager
                .state
                .read()
                .await
                .now_playing
                .as_ref()
                .unwrap()
                .position_ms,
            80_000
        );
        drop(manager);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn auto_selection_prioritizes_playing_over_paused_target_session() {
        let mut sessions = [
            playback_session("stale-abs", true, "jpn", Some(99_000)),
            playback_session("active-jellyfin", false, "eng", Some(90_000)),
        ];

        sessions.sort_by(|left, right| compare_auto_session_priority(left, right, "jpn"));

        assert_eq!(sessions[0].id, "active-jellyfin");
    }

    #[test]
    fn auto_selection_uses_language_then_freshness_within_same_play_state() {
        let mut sessions = [
            playback_session("older-target", false, "jpn", Some(80_000)),
            playback_session("newer-other", false, "eng", Some(99_000)),
            playback_session("newer-target", false, "jpn", Some(90_000)),
        ];

        sessions.sort_by(|left, right| compare_auto_session_priority(left, right, "jpn"));

        assert_eq!(sessions[0].id, "newer-target");
        assert_eq!(sessions[1].id, "older-target");
        assert_eq!(sessions[2].id, "newer-other");
    }

    #[test]
    fn timestamped_sessions_expire_according_to_play_state() {
        let now_ms = 1_000_000;
        let paused_lifetime_ms = PAUSED_SESSION_VISIBLE_AFTER.as_millis() as i64;
        let playing_lifetime_ms = PLAYING_SESSION_STALE_AFTER.as_millis() as i64;

        assert!(session_is_visible(
            MediaServerKind::Jellyfin,
            &playback_session(
                "recent-pause",
                true,
                "jpn",
                Some(now_ms - paused_lifetime_ms)
            ),
            now_ms
        ));
        assert!(!session_is_visible(
            MediaServerKind::Jellyfin,
            &playback_session(
                "old-pause",
                true,
                "jpn",
                Some(now_ms - paused_lifetime_ms - 1),
            ),
            now_ms
        ));
        assert!(!session_is_visible(
            MediaServerKind::Jellyfin,
            &playback_session(
                "stalled-player",
                false,
                "jpn",
                Some(now_ms - playing_lifetime_ms - 1),
            ),
            now_ms
        ));
        assert!(session_is_visible(
            MediaServerKind::Jellyfin,
            &playback_session("no-clock", true, "jpn", None),
            now_ms
        ));

        assert!(session_is_visible(
            MediaServerKind::Audiobookshelf,
            &playback_session(
                "audiobookshelf-local",
                true,
                "jpn",
                Some(now_ms - AUDIOBOOKSHELF_PAUSED_SESSION_VISIBLE_AFTER.as_millis() as i64),
            ),
            now_ms,
        ));
    }

    #[test]
    fn ignores_theme_episode_regardless_of_case() {
        let episode = now_playing(" Theme ", "Video", Some("Example Series"));

        assert!(is_ignored_episode(&episode));
    }

    #[test]
    fn does_not_ignore_other_episodes_or_theme_movies() {
        let other_episode = now_playing("Pilot", "Episode", Some("Example Series"));
        let theme_movie = now_playing("Theme", "Movie", None);

        assert!(!is_ignored_episode(&other_episode));
        assert!(!is_ignored_episode(&theme_movie));
    }

    #[test]
    fn abs_prefers_exact_sidecar_before_language_tagged_alternative() {
        let media_stem = "Japanese Audiobook";
        let language_candidate = SessionManager::sidecar_subtitle_candidate(
            media_stem,
            PathBuf::from("/audiobooks/Japanese Audiobook.ja.srt"),
        )
        .unwrap();
        let exact_candidate = SessionManager::sidecar_subtitle_candidate(
            media_stem,
            PathBuf::from("/audiobooks/Japanese Audiobook.srt"),
        )
        .unwrap();

        let (selected, mode) = SessionManager::resolve_subtitle_candidate(
            &[language_candidate, exact_candidate.clone()],
            "jpn",
            None,
            MediaServerKind::Audiobookshelf,
        );

        assert_eq!(mode, SubtitleSelectionMode::Auto);
        assert_eq!(selected.unwrap().id, exact_candidate.id);
        assert_eq!(exact_candidate.source, SubtitleCandidateSource::Sidecar);
    }

    #[test]
    fn manual_abs_subtitle_choice_still_wins_over_exact_match() {
        let media_stem = "Japanese Audiobook";
        let manual_candidate = SessionManager::sidecar_subtitle_candidate(
            media_stem,
            PathBuf::from("/audiobooks/Japanese Audiobook.ja.srt"),
        )
        .unwrap();
        let exact_candidate = SessionManager::sidecar_subtitle_candidate(
            media_stem,
            PathBuf::from("/audiobooks/Japanese Audiobook.srt"),
        )
        .unwrap();

        let (selected, mode) = SessionManager::resolve_subtitle_candidate(
            &[exact_candidate, manual_candidate.clone()],
            "jpn",
            Some(&manual_candidate.id),
            MediaServerKind::Audiobookshelf,
        );

        assert_eq!(mode, SubtitleSelectionMode::Manual);
        assert_eq!(selected.unwrap().id, manual_candidate.id);
    }

    #[test]
    fn fuzzy_sidecars_are_ranked_by_filename_confidence() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nagare-subtitle-fuzzy-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();

        let media_path = directory.join("Book Chapter 02.mp3");
        fs::write(&media_path, []).unwrap();
        fs::write(directory.join("Book Chapter 19.srt"), []).unwrap();
        fs::write(directory.join("Book Chapter 02 Japanese.srt"), []).unwrap();
        fs::write(directory.join("Book Chapter 02 Japanese.ass"), []).unwrap();

        let candidates = SessionManager::sidecar_subtitle_candidates(&media_path);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, "sidecar:Book Chapter 02 Japanese.srt");
        assert!(candidates[0].match_confidence > candidates[1].match_confidence);
        assert!(candidates[0].label.contains("% match"));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exact_sidecars_suppress_directory_wide_fuzzy_fallback() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nagare-subtitle-exact-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();

        let media_path = directory.join("Book Chapter 02.mp3");
        fs::write(&media_path, []).unwrap();
        fs::write(directory.join("Book Chapter 02.ja.srt"), []).unwrap();
        fs::write(directory.join("Another Book.srt"), []).unwrap();

        let candidates = SessionManager::sidecar_subtitle_candidates(&media_path);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "sidecar:Book Chapter 02.ja.srt");
        assert_eq!(candidates[0].match_confidence, None);

        fs::remove_dir_all(directory).unwrap();
    }
}
