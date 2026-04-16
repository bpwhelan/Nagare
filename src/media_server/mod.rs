pub mod mediabrowser;
pub mod plex;

use crate::config::MediaServerKind;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub use mediabrowser::{EmbyClient, JellyfinClient};
pub use plex::PlexClient;

pub type ServerMap = BTreeMap<MediaServerKind, Arc<dyn MediaServer>>;

/// Abstraction over Emby/Jellyfin APIs for future portability.
#[async_trait]
pub trait MediaServer: Send + Sync {
    fn kind(&self) -> MediaServerKind;
    async fn get_sessions(&self) -> anyhow::Result<Vec<Session>>;
    async fn get_item_info(&self, item_id: &str, user_id: Option<&str>)
    -> anyhow::Result<ItemInfo>;
    async fn get_subtitles(
        &self,
        item_id: &str,
        media_source_id: &str,
        stream_index: u32,
        format: SubtitleFormat,
    ) -> anyhow::Result<String>;
    fn get_stream_url(&self, item_id: &str, media_source_id: &str) -> String;
    async fn seek_session(&self, session_id: &str, position_ticks: i64) -> anyhow::Result<()>;
    async fn pause_session(&self, session_id: &str) -> anyhow::Result<()>;
    async fn unpause_session(&self, session_id: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SubtitleFormat {
    Vtt,
    Srt,
}

impl SubtitleFormat {
    pub fn extension(&self) -> &str {
        match self {
            SubtitleFormat::Vtt => "vtt",
            SubtitleFormat::Srt => "srt",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub client: String,
    pub device_name: String,
    pub user_name: Option<String>,
    pub user_id: Option<String>,
    pub now_playing: Option<NowPlaying>,
    pub play_state: PlayState,
    pub supports_remote_control: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NowPlaying {
    pub item_id: String,
    pub name: String,
    pub series_name: Option<String>,
    pub season_index: Option<u32>,
    pub episode_index: Option<u32>,
    pub media_type: String,
    pub run_time_ticks: Option<i64>,
    pub media_streams: Vec<MediaStream>,
    pub media_source_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayState {
    pub can_seek: bool,
    pub is_paused: bool,
    pub position_ticks: Option<i64>,
    pub audio_stream_index: Option<u32>,
    pub subtitle_stream_index: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStream {
    pub index: u32,
    pub stream_type: StreamType,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub display_title: Option<String>,
    pub is_default: bool,
    pub is_external: bool,
    pub is_text_subtitle_stream: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
    pub media_streams: Vec<MediaStream>,
    pub media_sources: Vec<MediaSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSource {
    pub id: String,
    pub path: Option<String>,
    pub media_streams: Vec<MediaStream>,
}

impl NowPlaying {
    pub fn has_audio_language(&self, lang: &str) -> bool {
        self.media_streams.iter().any(|s| {
            s.stream_type == StreamType::Audio
                && s.language.as_deref().map(|l| l == lang).unwrap_or(false)
        })
    }

    pub fn subtitle_streams_for_language(&self, lang: &str) -> Vec<&MediaStream> {
        self.media_streams
            .iter()
            .filter(|s| {
                s.stream_type == StreamType::Subtitle
                    && s.language.as_deref().map(|l| l == lang).unwrap_or(false)
            })
            .collect()
    }

    pub fn display_title(&self) -> String {
        if let Some(ref series) = self.series_name {
            let s = self.season_index.unwrap_or(0);
            let e = self.episode_index.unwrap_or(0);
            format!("{series} S{s:02}E{e:02} - {}", self.name)
        } else {
            self.name.clone()
        }
    }
}

impl Session {
    pub fn position_ms(&self) -> Option<i64> {
        self.play_state.position_ticks.map(|t| t / 10_000) // Emby uses 10,000 ticks per ms
    }
}
