use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen_address")]
    pub listen_address: String,

    #[serde(default)]
    pub emby: Option<EmbyConfig>,
    #[serde(default)]
    pub jellyfin: Option<JellyfinConfig>,
    #[serde(default)]
    pub plex: Option<PlexConfig>,
    #[serde(default)]
    pub audiobookshelf: Option<AudiobookshelfConfig>,

    #[serde(default = "default_target_language")]
    pub target_language: String,

    /// Native/comprehension language for the secondary subtitle track shown
    /// alongside the target-language subtitles (default English).
    #[serde(default = "default_native_language")]
    pub native_language: String,

    #[serde(default)]
    pub anki: AnkiConfig,

    #[serde(default)]
    pub path_mappings: Vec<PathMapping>,

    #[serde(default = "default_media_access_mode")]
    pub media_access_mode: MediaAccessMode,

    #[serde(default)]
    pub mining: MiningConfig,

    #[serde(default)]
    pub tadoku: TadokuConfig,

    #[serde(default)]
    pub kechimochi: KechimochiConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum MediaServerKind {
    Emby,
    Jellyfin,
    Plex,
    Audiobookshelf,
}

impl MediaServerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Emby => "emby",
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
            Self::Audiobookshelf => "audiobookshelf",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Emby => "Emby",
            Self::Jellyfin => "Jellyfin",
            Self::Plex => "Plex",
            Self::Audiobookshelf => "AudioBookShelf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "emby" => Some(Self::Emby),
            "jellyfin" => Some(Self::Jellyfin),
            "plex" => Some(Self::Plex),
            "audiobookshelf" => Some(Self::Audiobookshelf),
            _ => None,
        }
    }
}

impl fmt::Display for MediaServerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    pub api_key: String,
    /// User IDs to monitor. Empty means all users on the server.
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JellyfinConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    pub api_key: String,
    /// User IDs to monitor. Empty means all users on the server.
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlexConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    pub token: String,
    /// User IDs (account IDs) to monitor. Empty means all users on the server.
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudiobookshelfConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    /// An admin API token is required to enumerate open and listening-history sessions.
    pub token: String,
    /// User IDs to monitor. Empty means all users on the server.
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnkiConfig {
    #[serde(default = "default_ankiconnect_url")]
    pub url: String,

    #[serde(default)]
    pub fields: AnkiFieldMapping,

    /// Tags to add to every enriched card
    #[serde(default)]
    pub add_tags: Vec<String>,

    /// Automatically enhance new cards with any of these tags without showing confirmation.
    #[serde(default)]
    pub skip_confirmation_tags: Vec<String>,

    /// Tags to remove after a successful enhancement.
    #[serde(default)]
    pub remove_tags: Vec<String>,

    /// When enabled, a per-series tag is added, derived from the show title.
    #[serde(default)]
    pub series_tag_enabled: bool,

    /// Optional parent tag under which the per-series tag is nested
    /// (e.g. "anime" produces `anime::Series_Name`). Empty means the series
    /// name is used as a top-level tag. Only applies when `series_tag_enabled`.
    #[serde(default)]
    pub series_tag_parent: String,

    /// Ignore cards that already have ANY of these tags
    #[serde(default)]
    pub ignore_tags: Vec<String>,

    /// Only process cards that have at least one of these tags (empty = process all)
    #[serde(default)]
    pub require_tags: Vec<String>,

    /// Skip cards whose sentence_audio field is already non-empty
    #[serde(default = "default_true")]
    pub skip_if_audio_exists: bool,

    /// Skip cards whose picture field is already non-empty
    #[serde(default = "default_true")]
    pub skip_if_picture_exists: bool,

    /// Only process cards from these note types (empty = all note types)
    #[serde(default)]
    pub note_types: Vec<String>,

    /// How often to poll AnkiConnect for new cards, in milliseconds
    #[serde(default = "default_polling_rate_ms")]
    pub polling_rate_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnkiFieldMapping {
    #[serde(default = "default_sentence_field")]
    pub sentence: String,

    #[serde(default = "default_sentence_audio_field")]
    pub sentence_audio: String,

    #[serde(default = "default_picture_field")]
    pub picture: String,

    /// Optional field to write the show/source name into
    #[serde(default)]
    pub source_name: Option<String>,

    /// Optional field to write the native-language sentence translation into.
    /// When unset/empty the translation feature is disabled.
    #[serde(default)]
    pub sentence_translation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathMapping {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningConfig {
    #[serde(default = "default_audio_start_offset_ms")]
    pub audio_start_offset_ms: i64,

    #[serde(default = "default_audio_end_offset_ms")]
    pub audio_end_offset_ms: i64,

    #[serde(default = "default_audio_codec")]
    pub audio_codec: AudioCodec,

    #[serde(default = "default_true")]
    pub generate_avif: bool,

    #[serde(default = "default_animated_screenshot_encoder")]
    pub animated_screenshot_encoder: AnimatedScreenshotEncoder,

    /// Upper bound on the animated screenshot width in pixels. The source is
    /// never upscaled past its native width. Adaptive sizing may lower this cap.
    #[serde(default = "default_avif_max_width")]
    pub avif_max_width: u32,

    /// Upper bound on the animated screenshot frame rate.
    #[serde(default = "default_avif_max_fps")]
    pub avif_max_fps: u32,

    #[serde(default)]
    pub avif_sizing_mode: AvifSizingMode,

    /// Approximate animated image size in KiB (1,024 bytes), used in target-size mode.
    #[serde(default = "default_avif_target_size_kb")]
    pub avif_target_size_kb: u32,

    #[serde(default)]
    pub avif_size_priority: AvifSizePriority,

    #[serde(default = "default_static_screenshot_format")]
    pub static_screenshot_format: StaticScreenshotFormat,

    /// Legacy server-side setting kept temporarily so existing configs can be
    /// migrated into client-side local storage. It is ignored by the backend
    /// and stripped when the config is written back to disk.
    #[serde(default)]
    pub auto_approve: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TadokuConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Automatic export cadence. This only applies when `enabled` is true;
    /// disabling Tadoku automatic exports keeps the manual review workflow.
    #[serde(default)]
    pub sync_mode: TadokuSyncMode,

    /// Tadoku username or email used by the browser login flow.
    #[serde(default)]
    pub username: String,

    /// Tadoku password. This is persisted in the existing application config,
    /// but is redacted from settings API responses.
    #[serde(default)]
    pub password: String,

    /// Internal browser session cookie obtained after credential login. This
    /// is never returned by the settings API.
    #[serde(default)]
    pub session_cookie: String,

    /// ISO 639-3 language code used for exported listening logs.
    #[serde(default = "default_target_language")]
    pub language_code: String,

    /// Hour of day in America/New_York. The scheduler observes DST.
    #[serde(default = "default_tadoku_export_hour")]
    pub export_hour_eastern: u32,

    #[serde(default = "default_tadoku_api_url")]
    pub api_url: String,

    #[serde(default = "default_tadoku_session_url")]
    pub session_url: String,

    /// Case-insensitive file-path substring matches used to add Tadoku tags.
    #[serde(default = "default_tadoku_path_tag_rules")]
    pub path_tag_rules: Vec<TadokuPathTagRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TadokuPathTagRule {
    pub contains: String,
    pub tag: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TadokuSyncMode {
    #[default]
    Daily,
    Automatic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    Mp3,
    Aac,
    Opus,
}

impl AudioCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Opus => "opus",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Aac => "m4a",
            Self::Opus => "opus",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Aac => "audio/mp4",
            Self::Opus => "audio/ogg; codecs=opus",
        }
    }

    pub fn ffmpeg_args(self) -> &'static [&'static str] {
        match self {
            Self::Mp3 => &["-acodec", "libmp3lame", "-b:a", "96k", "-ac", "1"],
            Self::Aac => &[
                "-acodec",
                "aac",
                "-b:a",
                "96k",
                "-ac",
                "1",
                "-movflags",
                "+faststart",
            ],
            Self::Opus => &["-acodec", "libopus", "-b:a", "64k", "-ac", "1"],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimatedScreenshotEncoder {
    #[serde(rename = "libsvtav1")]
    Libsvtav1,
    #[serde(rename = "libaom-av1")]
    LibaomAv1,
}

impl AnimatedScreenshotEncoder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Libsvtav1 => "libsvtav1",
            Self::LibaomAv1 => "libaom-av1",
        }
    }

    pub fn fallback(self) -> Self {
        match self {
            Self::Libsvtav1 => Self::LibaomAv1,
            Self::LibaomAv1 => Self::Libsvtav1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvifSizingMode {
    #[default]
    Duration,
    TargetSize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvifSizePriority {
    PreferFps,
    #[default]
    PreferQuality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StaticScreenshotFormat {
    Webp,
    Jpg,
    Png,
    Avif,
}

impl StaticScreenshotFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Jpg => "jpg",
            Self::Png => "png",
            Self::Avif => "avif",
        }
    }

    pub fn extension(self) -> &'static str {
        self.as_str()
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Webp => "image/webp",
            Self::Jpg => "image/jpeg",
            Self::Png => "image/png",
            Self::Avif => "image/avif",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MediaAccessMode {
    Api,
    Disk,
    Auto,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: default_listen_address(),
            emby: None,
            jellyfin: None,
            plex: None,
            audiobookshelf: None,
            target_language: default_target_language(),
            native_language: default_native_language(),
            anki: AnkiConfig::default(),
            path_mappings: Vec::new(),
            media_access_mode: default_media_access_mode(),
            mining: MiningConfig::default(),
            tadoku: TadokuConfig::default(),
            kechimochi: KechimochiConfig::default(),
        }
    }
}

impl Config {
    /// Load from a JSON file in the data directory, falling back to defaults.
    pub fn load_or_default(path: &Path) -> Self {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<Config>(&content) {
                    Ok(config) => {
                        if config.mining.auto_approve {
                            tracing::warn!(
                                "Legacy server-side mining.auto_approve is set in {:?}; \
                                 this setting is now client-local and will be ignored by the backend",
                                path
                            );
                        }
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse config from {:?}: {}", path, e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read config from {:?}: {}", path, e);
                }
            }
        }
        Config::default()
    }

    /// Save config to a JSON file (atomic write).
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let mut json_value = serde_json::to_value(self)?;
        if let Some(mining) = json_value
            .get_mut("mining")
            .and_then(|value| value.as_object_mut())
        {
            mining.remove("auto_approve");
        }
        let json = serde_json::to_string_pretty(&json_value)?;
        // Write to temp file then rename for atomicity
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn normalize_mapping_path(value: &str) -> String {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }

    pub fn map_path(&self, server_path: &str) -> PathBuf {
        let normalized_server = Self::normalize_mapping_path(server_path);

        for mapping in &self.path_mappings {
            let normalized_from = Self::normalize_mapping_path(&mapping.from);
            let is_exact_match = normalized_server == normalized_from;
            let is_prefix_match = normalized_server
                .strip_prefix(&normalized_from)
                .map(|rest| rest.starts_with('/'))
                .unwrap_or(false);

            if is_exact_match || is_prefix_match {
                let relative = server_path
                    .get(mapping.from.len()..)
                    .or_else(|| server_path.get(normalized_from.len()..))
                    .unwrap_or("");

                return PathBuf::from(&mapping.to).join(relative.trim_start_matches(['/', '\\']));
            }
        }
        PathBuf::from(server_path)
    }

    pub fn enabled_server_kinds(&self) -> Vec<MediaServerKind> {
        let mut kinds = Vec::new();
        if self.emby.as_ref().map(|cfg| cfg.enabled).unwrap_or(false) {
            kinds.push(MediaServerKind::Emby);
        }
        if self
            .jellyfin
            .as_ref()
            .map(|cfg| cfg.enabled)
            .unwrap_or(false)
        {
            kinds.push(MediaServerKind::Jellyfin);
        }
        if self.plex.as_ref().map(|cfg| cfg.enabled).unwrap_or(false) {
            kinds.push(MediaServerKind::Plex);
        }
        if self
            .audiobookshelf
            .as_ref()
            .map(|cfg| cfg.enabled)
            .unwrap_or(false)
        {
            kinds.push(MediaServerKind::Audiobookshelf);
        }
        kinds
    }

    /// Whether a media server is configured.
    pub fn has_server(&self) -> bool {
        !self.enabled_server_kinds().is_empty()
    }

    /// The configured user-ID allowlist for a server (empty = all users).
    pub fn allowed_users(&self, kind: MediaServerKind) -> &[String] {
        match kind {
            MediaServerKind::Emby => self.emby.as_ref().map(|c| c.users.as_slice()),
            MediaServerKind::Jellyfin => self.jellyfin.as_ref().map(|c| c.users.as_slice()),
            MediaServerKind::Plex => self.plex.as_ref().map(|c| c.users.as_slice()),
            MediaServerKind::Audiobookshelf => {
                self.audiobookshelf.as_ref().map(|c| c.users.as_slice())
            }
        }
        .unwrap_or(&[])
    }

    /// Whether a session belonging to `user_id`/`user_name` on `kind` should be
    /// monitored. An empty allowlist matches every user. Matching is done
    /// against the user ID first, falling back to the display name.
    pub fn is_user_allowed(
        &self,
        kind: MediaServerKind,
        user_id: Option<&str>,
        user_name: Option<&str>,
    ) -> bool {
        let allowed = self.allowed_users(kind);
        if allowed.is_empty() {
            return true;
        }
        allowed.iter().any(|entry| {
            user_id.map(|id| id == entry).unwrap_or(false)
                || user_name.map(|name| name == entry).unwrap_or(false)
        })
    }

    /// Whether the connection parameters (URL/key/enabled) of any server differ
    /// from `other`, ignoring unrelated fields such as the user allowlist. Used
    /// to decide whether media-server clients need to be rebuilt.
    pub fn server_connection_changed(&self, other: &Self) -> bool {
        let emby = match (&self.emby, &other.emby) {
            (Some(a), Some(b)) => {
                a.url != b.url || a.api_key != b.api_key || a.enabled != b.enabled
            }
            (None, None) => false,
            _ => true,
        };
        let jellyfin = match (&self.jellyfin, &other.jellyfin) {
            (Some(a), Some(b)) => {
                a.url != b.url || a.api_key != b.api_key || a.enabled != b.enabled
            }
            (None, None) => false,
            _ => true,
        };
        let plex = match (&self.plex, &other.plex) {
            (Some(a), Some(b)) => a.url != b.url || a.token != b.token || a.enabled != b.enabled,
            (None, None) => false,
            _ => true,
        };
        let audiobookshelf = match (&self.audiobookshelf, &other.audiobookshelf) {
            (Some(a), Some(b)) => a.url != b.url || a.token != b.token || a.enabled != b.enabled,
            (None, None) => false,
            _ => true,
        };
        emby || jellyfin || plex || audiobookshelf
    }
}

impl Default for AnkiConfig {
    fn default() -> Self {
        Self {
            url: default_ankiconnect_url(),
            fields: AnkiFieldMapping::default(),
            add_tags: Vec::new(),
            skip_confirmation_tags: Vec::new(),
            remove_tags: Vec::new(),
            series_tag_enabled: false,
            series_tag_parent: String::new(),
            ignore_tags: Vec::new(),
            require_tags: Vec::new(),
            skip_if_audio_exists: true,
            skip_if_picture_exists: true,
            note_types: Vec::new(),
            polling_rate_ms: default_polling_rate_ms(),
        }
    }
}

impl Default for AnkiFieldMapping {
    fn default() -> Self {
        Self {
            sentence: default_sentence_field(),
            sentence_audio: default_sentence_audio_field(),
            picture: default_picture_field(),
            source_name: None,
            sentence_translation: None,
        }
    }
}

impl Default for MiningConfig {
    fn default() -> Self {
        Self {
            audio_start_offset_ms: default_audio_start_offset_ms(),
            audio_end_offset_ms: default_audio_end_offset_ms(),
            audio_codec: default_audio_codec(),
            generate_avif: true,
            animated_screenshot_encoder: default_animated_screenshot_encoder(),
            avif_max_width: default_avif_max_width(),
            avif_max_fps: default_avif_max_fps(),
            avif_sizing_mode: AvifSizingMode::default(),
            avif_target_size_kb: default_avif_target_size_kb(),
            avif_size_priority: AvifSizePriority::default(),
            static_screenshot_format: default_static_screenshot_format(),
            auto_approve: false,
        }
    }
}

impl MiningConfig {
    pub fn validate_avif_settings(&self) -> anyhow::Result<()> {
        if self.avif_sizing_mode == AvifSizingMode::TargetSize && self.avif_target_size_kb == 0 {
            bail!("Target image size must be at least 1 KB");
        }
        Ok(())
    }
}

#[cfg(test)]
mod avif_settings_tests {
    use super::*;

    #[test]
    fn legacy_mining_settings_keep_duration_sizing() {
        let config: MiningConfig =
            serde_json::from_str(r#"{"avif_max_width":640,"avif_max_fps":15}"#).unwrap();
        assert_eq!(config.avif_sizing_mode, AvifSizingMode::Duration);
        assert_eq!(config.avif_target_size_kb, 500);
        assert_eq!(config.avif_size_priority, AvifSizePriority::PreferQuality);
        assert_eq!(config.avif_max_width, 640);
        assert_eq!(config.avif_max_fps, 15);
    }

    #[test]
    fn size_settings_round_trip_and_reject_invalid_values() {
        for priority in [AvifSizePriority::PreferFps, AvifSizePriority::PreferQuality] {
            let config = MiningConfig {
                avif_sizing_mode: AvifSizingMode::TargetSize,
                avif_target_size_kb: 256,
                avif_size_priority: priority,
                ..MiningConfig::default()
            };
            let mut loaded: MiningConfig =
                serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
            assert_eq!(loaded.avif_sizing_mode, AvifSizingMode::TargetSize);
            assert_eq!(loaded.avif_target_size_kb, 256);
            assert_eq!(loaded.avif_size_priority, priority);
            assert!(loaded.validate_avif_settings().is_ok());
            loaded.avif_target_size_kb = 0;
            assert!(loaded.validate_avif_settings().is_err());
            loaded.avif_sizing_mode = AvifSizingMode::Duration;
            assert!(loaded.validate_avif_settings().is_ok());
        }
        for value in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::Value::Null,
        ] {
            assert!(
                serde_json::from_value::<MiningConfig>(
                    serde_json::json!({"avif_target_size_kb": value})
                )
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod anki_tag_settings_tests {
    use super::AnkiConfig;

    #[test]
    fn tag_options_default_for_existing_configs_and_round_trip() {
        let mut config: AnkiConfig = serde_json::from_str("{}").unwrap();
        assert!(config.skip_confirmation_tags.is_empty());
        assert!(config.remove_tags.is_empty());

        config.skip_confirmation_tags = vec!["quick".into()];
        config.remove_tags = vec!["temporary".into()];
        let loaded: AnkiConfig =
            serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert_eq!(loaded.skip_confirmation_tags, ["quick"]);
        assert_eq!(loaded.remove_tags, ["temporary"]);
    }
}

impl Default for TadokuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sync_mode: TadokuSyncMode::Daily,
            username: String::new(),
            password: String::new(),
            session_cookie: String::new(),
            language_code: default_target_language(),
            export_hour_eastern: default_tadoku_export_hour(),
            api_url: default_tadoku_api_url(),
            session_url: default_tadoku_session_url(),
            path_tag_rules: default_tadoku_path_tag_rules(),
        }
    }
}

impl TadokuConfig {
    pub fn has_credentials(&self) -> bool {
        !self.username.trim().is_empty() && !self.password.is_empty()
    }

    pub fn automatic_sync_enabled(&self) -> bool {
        self.enabled && self.sync_mode == TadokuSyncMode::Automatic
    }

    /// Normalize known Tadoku production URLs. The OpenAPI document still
    /// advertises `/api/immersion`, while the deployed web app currently
    /// routes the service through `/api/internal/immersion`.
    pub fn normalize(&mut self) {
        self.username = self.username.trim().to_string();
        let api_url = self.api_url.trim().trim_end_matches('/');
        self.api_url = match api_url {
            "" | "https://tadoku.app/api/immersion" | "https://tadoku.app/api/internal" => {
                default_tadoku_api_url()
            }
            value => value.to_string(),
        };
        if self.session_url.trim().is_empty() {
            self.session_url = default_tadoku_session_url();
        } else {
            self.session_url = self.session_url.trim().to_string();
        }
        self.language_code = self.language_code.trim().to_ascii_lowercase();
        self.export_hour_eastern = self.export_hour_eastern.min(23);
        for rule in &mut self.path_tag_rules {
            rule.contains = rule.contains.trim().to_string();
            rule.tag = rule.tag.trim().to_string();
        }
        self.path_tag_rules
            .retain(|rule| !rule.contains.is_empty() && !rule.tag.is_empty());
    }
}

fn default_listen_address() -> String {
    "0.0.0.0:9470".to_string()
}
fn default_target_language() -> String {
    "jpn".to_string()
}
fn default_native_language() -> String {
    "eng".to_string()
}
fn default_ankiconnect_url() -> String {
    "http://localhost:8765".to_string()
}
fn default_sentence_field() -> String {
    "Sentence".to_string()
}
fn default_sentence_audio_field() -> String {
    "SentenceAudio".to_string()
}
fn default_picture_field() -> String {
    "Picture".to_string()
}
fn default_media_access_mode() -> MediaAccessMode {
    MediaAccessMode::Auto
}
fn default_polling_rate_ms() -> u64 {
    1_000
}
fn default_audio_start_offset_ms() -> i64 {
    100
}
fn default_audio_end_offset_ms() -> i64 {
    500
}
fn default_audio_codec() -> AudioCodec {
    AudioCodec::Mp3
}
fn default_animated_screenshot_encoder() -> AnimatedScreenshotEncoder {
    AnimatedScreenshotEncoder::Libsvtav1
}
fn default_avif_max_width() -> u32 {
    480
}
fn default_avif_max_fps() -> u32 {
    10
}
fn default_avif_target_size_kb() -> u32 {
    500
}
fn default_static_screenshot_format() -> StaticScreenshotFormat {
    StaticScreenshotFormat::Webp
}
fn default_true() -> bool {
    true
}
fn default_tadoku_export_hour() -> u32 {
    20
}
fn default_tadoku_api_url() -> String {
    "https://tadoku.app/api/internal/immersion".to_string()
}
fn default_tadoku_session_url() -> String {
    "https://account.tadoku.app/kratos/sessions/whoami".to_string()
}

fn default_tadoku_path_tag_rules() -> Vec<TadokuPathTagRule> {
    vec![TadokuPathTagRule {
        contains: "anime".to_string(),
        tag: "anime".to_string(),
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct KechimochiConfig {
    pub enabled: bool,
    pub api_url: String,
    pub sync_mode: KechimochiSyncMode,
    pub interval_minutes: u32,
    pub daily_hour: u32,
    pub daily_minute: u32,
    pub timezone: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KechimochiSyncMode {
    #[default]
    Automatic,
    Daily,
}

impl Default for KechimochiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_url: "http://127.0.0.1:3031".into(),
            sync_mode: KechimochiSyncMode::Automatic,
            interval_minutes: 5,
            daily_hour: 20,
            daily_minute: 0,
            timezone: "America/New_York".into(),
        }
    }
}

impl KechimochiConfig {
    pub fn normalize_and_validate(&mut self) -> anyhow::Result<()> {
        let mut url = reqwest::Url::parse(self.api_url.trim())
            .context("Kechimochi needs a valid HTTP or HTTPS base URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!(
                "Kechimochi URL must use HTTP or HTTPS without credentials, a query, or a fragment"
            );
        }
        let path = url
            .path()
            .trim_end_matches('/')
            .trim_end_matches("/api")
            .to_string();
        url.set_path(&path);
        self.api_url = url.as_str().trim_end_matches('/').to_string();
        self.timezone = self.timezone.trim().to_string();
        self.timezone
            .parse::<chrono_tz::Tz>()
            .context("Unknown Kechimochi time zone; use an IANA name such as America/New_York")?;
        if !(1..=1440).contains(&self.interval_minutes) {
            bail!("Kechimochi sync interval must be between 1 and 1440 minutes");
        }
        if self.daily_hour > 23 || self.daily_minute > 59 {
            bail!("Kechimochi daily sync time must be a valid hour and minute");
        }
        Ok(())
    }
}
