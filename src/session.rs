use crate::config::{Config, MediaServerKind};
use crate::media_server::{
    ItemInfo, MediaServer, MediaStream, ServerMap, Session, StreamType, SubtitleFormat,
};
use crate::mining::AppDatabase;
use crate::subtitle::{SubtitleTrack, parse_subtitle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    #[serde(skip_serializing, skip_deserializing, default)]
    local_path: Option<PathBuf>,
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
    subtitle_candidates: Arc<RwLock<Vec<SubtitleCandidate>>>,
    state_tx: watch::Sender<SessionState>,
    /// User override for which session to track (None = auto-select)
    selected_session_id: Arc<RwLock<Option<String>>>,
    /// User override for which subtitle track to load on the active item (None = auto-select).
    selected_subtitle_candidate_override: Arc<RwLock<Option<String>>>,
    /// The subtitle candidate currently loaded into Nagare, if any.
    loaded_subtitle_candidate_id: Arc<RwLock<Option<String>>>,
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
    /// Throttle: only persist to SQLite at most once per 30 s during position updates.
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
            subtitle_candidates: Arc::new(RwLock::new(Vec::new())),
            state_tx,
            selected_session_id: Arc::new(RwLock::new(None)),
            selected_subtitle_candidate_override: Arc::new(RwLock::new(None)),
            loaded_subtitle_candidate_id: Arc::new(RwLock::new(None)),
            subtitle_history: Arc::new(RwLock::new(subtitle_history)),
            history: Arc::new(RwLock::new(history)),
            audio_tracks: Arc::new(RwLock::new(Vec::new())),
            selected_audio_track: Arc::new(RwLock::new(None)),
            audio_track_resolution: Arc::new(RwLock::new(AudioTrackResolution::Single)),
            db,
            poll_lock: Mutex::new(()),
            last_save,
            plex_ws_connected: AtomicBool::new(false),
        })
    }

    pub fn subtitles(&self) -> Arc<RwLock<Option<SubtitleTrack>>> {
        self.subtitles.clone()
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
            return (tracks.first().map(|t| t.index), AudioTrackResolution::Single);
        }

        // Multiple tracks — try target language match.
        if let Some(track) = tracks.iter().find(|t| {
            Self::language_matches_target(t.language.as_deref(), target_lang)
        }) {
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

    fn language_matches_target(language: Option<&str>, target_lang: &str) -> bool {
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
            local_path: None,
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
            local_path: Some(subtitle_path),
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

        paths
            .into_iter()
            .filter_map(|path| Self::sidecar_subtitle_candidate(video_stem, path))
            .collect()
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
        &self,
        candidates: &[SubtitleCandidate],
        target_lang: &str,
        override_candidate_id: Option<&str>,
    ) -> (Option<SubtitleCandidate>, SubtitleSelectionMode) {
        if let Some(candidate_id) = override_candidate_id {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.id == candidate_id)
            {
                return (Some(candidate.clone()), SubtitleSelectionMode::Manual);
            }
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

        let target_lang = self.config.read().await.target_language.clone();
        let sessions = self.collect_server_sessions(servers).await;
        let active = sessions
            .into_iter()
            .find(|server_session| {
                scoped_session_id(server_session.kind, &server_session.session.id)
                    == active_session_id
            })
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
        let (candidate, selection_mode) = self.resolve_subtitle_candidate(
            &candidates,
            &target_lang,
            requested_override.as_deref(),
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

        self.load_subtitles_for_item(
            &now_playing.item_id,
            &media_source_id,
            &active.session,
            &target_lang,
            &active.server,
            candidate.as_ref(),
        )
        .await;

        {
            let mut state = self.state.write().await;
            if let Some(now_playing_state) = state.now_playing.as_mut() {
                if now_playing_state.history_id == history_id {
                    now_playing_state.subtitle_stream_index =
                        candidate.as_ref().and_then(|track| track.stream_index);
                    now_playing_state.subtitle_candidate_id =
                        candidate.as_ref().map(|track| track.id.clone());
                    now_playing_state.subtitle_selection_mode = selection_mode;
                }
            }
        }

        self.snapshot_active_track_into_history(&history_id).await;
        self.save_history(true).await;

        let snapshot = self.state.read().await.clone();
        let _ = self.state_tx.send(snapshot);

        Ok(())
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
        drop(config);

        let sessions = self.collect_server_sessions(servers).await;

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
            // Auto-select: prefer target-language sessions, fall back to any playing session
            let target_match = sessions
                .iter()
                .find(|s| {
                    s.session
                        .now_playing
                        .as_ref()
                        .map(|np| np.has_audio_language(&target_lang))
                        .unwrap_or(false)
                })
                .cloned();

            target_match.or_else(|| {
                sessions
                    .iter()
                    .find(|s| s.session.now_playing.is_some())
                    .cloned()
            })
        };

        // Collect info we need while holding the lock, then release
        let previous_loaded_candidate_id = self.loaded_subtitle_candidate_id.read().await.clone();
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
                let (candidate, selection_mode) = self.resolve_subtitle_candidate(
                    &candidates,
                    &target_lang,
                    requested_override.as_deref(),
                );
                let selected_stream_index = candidate.as_ref().and_then(|track| track.stream_index);
                let selected_candidate_id = candidate.as_ref().map(|track| track.id.clone());

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
                let (resolved_audio_index, audio_resolution) =
                    Self::resolve_audio_track(&audio_tracks_list, &target_lang, audio_user_override);

                if item_changed {
                    *self.audio_tracks.write().await = audio_tracks_list;
                    *self.selected_audio_track.write().await = resolved_audio_index;
                    *self.audio_track_resolution.write().await = audio_resolution;
                }

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
                    subtitle_stream_index: selected_stream_index,
                    subtitle_candidate_id: selected_candidate_id.clone(),
                    subtitle_selection_mode: selection_mode,
                    media_source_id: media_source_id.clone(),
                    file_path: np.path.clone(),
                    audio_stream_index: resolved_audio_index,
                });

                if item_changed || selected_candidate_id != previous_loaded_candidate_id {
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
            let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
            *loaded_candidate = None;
            drop(loaded_candidate);
            let snapshot = self.state.read().await.clone();
            let _ = self.state_tx.send(snapshot);
            return;
        }

        // Load subtitles outside the lock
        if let Some((item_changed, history_id, item_id, media_source_id, candidate, active)) =
            needs_subtitle_load
        {
            let display_title = active
                .session
                .now_playing
                .as_ref()
                .map(|np| np.display_title())
                .unwrap_or_else(|| item_id.clone());
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
                &active.server,
                candidate.as_ref(),
            )
            .await;

            let sub_count = self.snapshot_active_track_into_history(&history_id).await;

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
                    media_source_id: media_source_id.clone(),
                    file_path,
                    duration_ms: dur,
                    subtitle_count: sub_count,
                    last_position_ms: pos,
                    last_seen: chrono::Utc::now(),
                };
                self.history.write().await.insert(history_id.clone(), entry);
            }

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

    pub async fn handle_plex_playing_event(
        &self,
        client_identifier: &str,
        rating_key: &str,
        view_offset_ms: Option<i64>,
        player_state: &str,
    ) {
        let scoped_id = scoped_session_id(MediaServerKind::Plex, client_identifier);
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
                entry.last_position_ms = position_ms;
                entry.last_seen = chrono::Utc::now();
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

    async fn load_subtitles_for_item(
        &self,
        item_id: &str,
        media_source_id: &str,
        session: &Session,
        target_lang: &str,
        server: &Arc<dyn MediaServer>,
        candidate: Option<&SubtitleCandidate>,
    ) {
        let selected_candidate_id = candidate.map(|track| track.id.clone());

        if let Some(track_candidate) = candidate {
            match track_candidate.source {
                SubtitleCandidateSource::Server => {
                    if let Some(stream_index) = track_candidate.stream_index {
                        debug!(
                            "Fetching subtitles: item={} msid={} index={}",
                            item_id, media_source_id, stream_index
                        );

                        match server
                            .get_subtitles(
                                item_id,
                                media_source_id,
                                stream_index,
                                SubtitleFormat::Vtt,
                            )
                            .await
                        {
                            Ok(content) => {
                                let track = parse_subtitle(&content, None);
                                info!("Loaded {} subtitle lines from API", track.lines.len());
                                let mut subs = self.subtitles.write().await;
                                *subs = Some(track);
                                drop(subs);
                                let mut loaded_candidate =
                                    self.loaded_subtitle_candidate_id.write().await;
                                *loaded_candidate = selected_candidate_id;
                                return;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to fetch subtitles from API for stream {}: {}",
                                    stream_index, e
                                );
                            }
                        }
                    }
                }
                SubtitleCandidateSource::Sidecar => {
                    if let Some(sidecar_path) = track_candidate.local_path.as_ref() {
                        match std::fs::read_to_string(sidecar_path) {
                            Ok(content) => {
                                let track = parse_subtitle(&content, sidecar_path.to_str());
                                info!(
                                    "Loaded {} subtitle lines from sidecar: {:?}",
                                    track.lines.len(),
                                    sidecar_path
                                );
                                let mut subs = self.subtitles.write().await;
                                *subs = Some(track);
                                drop(subs);
                                let mut loaded_candidate =
                                    self.loaded_subtitle_candidate_id.write().await;
                                *loaded_candidate = selected_candidate_id;
                                return;
                            }
                            Err(error) => {
                                warn!(
                                    "Failed to read subtitle sidecar {:?}: {}",
                                    sidecar_path, error
                                );
                            }
                        }
                    }
                }
            }
        }

        let local_path = self
            .mapped_media_path_for_session(item_id, session, server)
            .await;

        if candidate.is_none() {
            debug!(
                "No loadable subtitle track candidate selected for item {}; trying fallback sources",
                item_id
            );
        }

        // Disk fallback
        if let Some(local_path) = local_path.as_ref() {
            debug!("Trying disk fallback at: {:?}", local_path);

            let fallback_candidates = Self::sidecar_subtitle_candidates(local_path);
            let sidecar_fallback = if let Some(track_candidate) = candidate {
                fallback_candidates.iter().find(|sidecar| {
                    track_candidate.source == SubtitleCandidateSource::Server
                        && (Self::language_matches_target(sidecar.language.as_deref(), target_lang)
                            || (track_candidate.language.is_some()
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
                        let track = parse_subtitle(&content, sidecar_path.to_str());
                        info!(
                            "Loaded {} subtitle lines from disk fallback: {:?}",
                            track.lines.len(),
                            sidecar_path
                        );
                        let mut subs = self.subtitles.write().await;
                        *subs = Some(track);
                        drop(subs);
                        let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
                        *loaded_candidate = Some(sidecar_candidate.id.clone());
                        return;
                    }
                }
            }
        }

        warn!("Could not load subtitles from any source");
        let mut subs = self.subtitles.write().await;
        *subs = None;
        drop(subs);
        let mut loaded_candidate = self.loaded_subtitle_candidate_id.write().await;
        *loaded_candidate = None;
    }
}

/// Run the session polling loop.
pub async fn run_session_poller(manager: Arc<SessionManager>) {
    loop {
        manager.poll_once().await;
        tokio::time::sleep(manager.poll_interval().await).await;
    }
}
