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

    #[serde(default = "default_target_language")]
    pub target_language: String,

    #[serde(default)]
    pub anki: AnkiConfig,

    #[serde(default)]
    pub path_mappings: Vec<PathMapping>,

    #[serde(default = "default_media_access_mode")]
    pub media_access_mode: MediaAccessMode,

    #[serde(default)]
    pub mining: MiningConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum MediaServerKind {
    Emby,
    Jellyfin,
    Plex,
}

impl MediaServerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Emby => "emby",
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Emby => "Emby",
            Self::Jellyfin => "Jellyfin",
            Self::Plex => "Plex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "emby" => Some(Self::Emby),
            "jellyfin" => Some(Self::Jellyfin),
            "plex" => Some(Self::Plex),
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JellyfinConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlexConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub url: String,
    pub token: String,
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

    #[serde(default = "default_true")]
    pub generate_avif: bool,

    /// Legacy server-side setting kept temporarily so existing configs can be
    /// migrated into client-side local storage. It is ignored by the backend
    /// and stripped when the config is written back to disk.
    #[serde(default)]
    pub auto_approve: bool,
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
            target_language: default_target_language(),
            anki: AnkiConfig::default(),
            path_mappings: Vec::new(),
            media_access_mode: default_media_access_mode(),
            mining: MiningConfig::default(),
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
        kinds
    }

    /// Whether a media server is configured.
    pub fn has_server(&self) -> bool {
        !self.enabled_server_kinds().is_empty()
    }
}

impl Default for AnkiConfig {
    fn default() -> Self {
        Self {
            url: default_ankiconnect_url(),
            fields: AnkiFieldMapping::default(),
            add_tags: Vec::new(),
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
        }
    }
}

impl Default for MiningConfig {
    fn default() -> Self {
        Self {
            audio_start_offset_ms: default_audio_start_offset_ms(),
            audio_end_offset_ms: default_audio_end_offset_ms(),
            generate_avif: true,
            auto_approve: false,
        }
    }
}

fn default_listen_address() -> String {
    "0.0.0.0:9470".to_string()
}
fn default_target_language() -> String {
    "jpn".to_string()
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
fn default_true() -> bool {
    true
}
