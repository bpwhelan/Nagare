use super::*;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

#[derive(Clone, Copy)]
enum ServerFlavor {
    Emby,
    Jellyfin,
}

pub struct MediaBrowserClient {
    base_url: String,
    api_key: String,
    http: Client,
    flavor: ServerFlavor,
}

pub type EmbyClient = MediaBrowserClient;
pub type JellyfinClient = MediaBrowserClient;

impl MediaBrowserClient {
    pub fn new(url: &str, api_key: &str) -> Self {
        Self::with_flavor(url, api_key, ServerFlavor::Emby)
    }

    pub fn new_jellyfin(url: &str, api_key: &str) -> Self {
        Self::with_flavor(url, api_key, ServerFlavor::Jellyfin)
    }

    fn with_flavor(url: &str, api_key: &str, flavor: ServerFlavor) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        Self {
            base_url,
            api_key: api_key.to_string(),
            http: Client::new(),
            flavor,
        }
    }

    fn url(&self, path: &str) -> String {
        let prefix = match self.flavor {
            ServerFlavor::Emby => "/emby",
            ServerFlavor::Jellyfin => "",
        };
        format!("{}{}{}", self.base_url, prefix, path)
    }

    fn auth_param(&self) -> (&str, &str) {
        ("api_key", &self.api_key)
    }

    fn parse_stream_type(s: &str) -> StreamType {
        match s {
            "Video" => StreamType::Video,
            "Audio" => StreamType::Audio,
            "Subtitle" => StreamType::Subtitle,
            _ => StreamType::Other,
        }
    }

    fn parse_media_stream(v: &Value) -> MediaStream {
        MediaStream {
            index: v["Index"].as_u64().unwrap_or(0) as u32,
            stream_type: Self::parse_stream_type(v["Type"].as_str().unwrap_or("")),
            codec: v["Codec"].as_str().map(String::from),
            language: v["Language"].as_str().map(String::from),
            display_title: v["DisplayTitle"].as_str().map(String::from),
            is_default: v["IsDefault"].as_bool().unwrap_or(false),
            is_external: v["IsExternal"].as_bool().unwrap_or(false),
            is_text_subtitle_stream: v["IsTextSubtitleStream"].as_bool().unwrap_or(false),
            title: v["Title"].as_str().map(String::from),
        }
    }

    fn parse_session(v: &Value) -> Option<Session> {
        let id = v["Id"].as_str()?.to_string();
        let client = v["Client"].as_str().unwrap_or("Unknown").to_string();
        let device_name = v["DeviceName"].as_str().unwrap_or("Unknown").to_string();
        let user_name = v["UserName"].as_str().map(String::from);
        let user_id = v["UserId"].as_str().map(String::from);
        let supports_remote_control = v["SupportsRemoteControl"].as_bool().unwrap_or(false);

        let play_state_val = &v["PlayState"];
        let play_state = PlayState {
            can_seek: play_state_val["CanSeek"].as_bool().unwrap_or(false),
            is_paused: play_state_val["IsPaused"].as_bool().unwrap_or(false),
            position_ticks: play_state_val["PositionTicks"].as_i64(),
            audio_stream_index: play_state_val["AudioStreamIndex"]
                .as_u64()
                .map(|v| v as u32),
            subtitle_stream_index: play_state_val["SubtitleStreamIndex"]
                .as_i64()
                .map(|v| v as i32),
        };

        let now_playing = if let Some(npi) = v.get("NowPlayingItem") {
            if npi.is_null() {
                None
            } else {
                let media_streams = npi["MediaStreams"]
                    .as_array()
                    .map(|arr| arr.iter().map(Self::parse_media_stream).collect())
                    .unwrap_or_default();

                let media_source_id = npi["MediaSourceId"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| play_state_val["MediaSourceId"].as_str().map(String::from))
                    .or_else(|| {
                        npi["MediaSources"]
                            .as_array()
                            .and_then(|sources| sources.first())
                            .and_then(|source| source["Id"].as_str().map(String::from))
                    });
                let path = npi["Path"].as_str().map(String::from).or_else(|| {
                    let target_media_source = media_source_id.as_deref();
                    npi["MediaSources"].as_array().and_then(|sources| {
                        sources.iter().find_map(|source| {
                            let matches_selected = match target_media_source {
                                Some(target) => source["Id"].as_str() == Some(target),
                                None => true,
                            };
                            matches_selected
                                .then(|| source["Path"].as_str().map(String::from))
                                .flatten()
                        })
                    })
                });

                Some(NowPlaying {
                    item_id: npi["Id"].as_str().unwrap_or("").to_string(),
                    name: npi["Name"].as_str().unwrap_or("").to_string(),
                    series_name: npi["SeriesName"].as_str().map(String::from),
                    season_index: npi["ParentIndexNumber"].as_u64().map(|v| v as u32),
                    episode_index: npi["IndexNumber"].as_u64().map(|v| v as u32),
                    media_type: npi["MediaType"].as_str().unwrap_or("").to_string(),
                    run_time_ticks: npi["RunTimeTicks"].as_i64(),
                    media_streams,
                    media_source_id,
                    path,
                })
            }
        } else {
            None
        };

        Some(Session {
            id,
            client,
            device_name,
            user_name,
            user_id,
            now_playing,
            play_state,
            supports_remote_control,
        })
    }
}

#[async_trait]
impl MediaServer for MediaBrowserClient {
    fn kind(&self) -> MediaServerKind {
        match self.flavor {
            ServerFlavor::Emby => MediaServerKind::Emby,
            ServerFlavor::Jellyfin => MediaServerKind::Jellyfin,
        }
    }

    async fn get_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let resp = self
            .http
            .get(self.url("/Sessions"))
            .query(&[self.auth_param()])
            .send()
            .await?;

        let body: Value = resp.json().await?;
        let sessions = body
            .as_array()
            .map(|arr| arr.iter().filter_map(Self::parse_session).collect())
            .unwrap_or_default();

        Ok(sessions)
    }

    async fn get_item_info(
        &self,
        item_id: &str,
        user_id: Option<&str>,
    ) -> anyhow::Result<ItemInfo> {
        let mut req = self
            .http
            .get(self.url(&format!("/Items/{item_id}")))
            .query(&[
                self.auth_param(),
                ("Fields", "MediaStreams,Path,MediaSources"),
            ]);

        if let Some(user_id) = user_id {
            req = req.query(&[("userId", user_id)]);
        }

        let resp = req.send().await?;

        let v: Value = resp.json().await?;

        let media_streams = v["MediaStreams"]
            .as_array()
            .map(|arr| arr.iter().map(Self::parse_media_stream).collect())
            .unwrap_or_default();

        let media_sources = v["MediaSources"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|ms| {
                        let streams = ms["MediaStreams"]
                            .as_array()
                            .map(|a| a.iter().map(Self::parse_media_stream).collect())
                            .unwrap_or_default();
                        MediaSource {
                            id: ms["Id"].as_str().unwrap_or("").to_string(),
                            path: ms["Path"].as_str().map(String::from),
                            media_streams: streams,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(ItemInfo {
            id: v["Id"].as_str().unwrap_or("").to_string(),
            name: v["Name"].as_str().unwrap_or("").to_string(),
            path: v["Path"].as_str().map(String::from),
            media_streams,
            media_sources,
        })
    }

    async fn get_subtitles(
        &self,
        item_id: &str,
        media_source_id: &str,
        stream_index: u32,
        format: SubtitleFormat,
    ) -> anyhow::Result<String> {
        let ext = format.extension();
        let url = format!(
            "{}/Videos/{}/{}/Subtitles/{}/Stream.{}",
            self.base_url, item_id, media_source_id, stream_index, ext
        );

        let resp = self
            .http
            .get(&url)
            .query(&[self.auth_param()])
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("Failed to fetch subtitles: HTTP {}", resp.status());
        }

        Ok(resp.text().await?)
    }

    fn get_stream_url(&self, item_id: &str, media_source_id: &str) -> String {
        format!(
            "{}/Videos/{}/stream?MediaSourceId={}&api_key={}&Static=true",
            self.base_url, item_id, media_source_id, self.api_key
        )
    }

    async fn seek_session(&self, session_id: &str, position_ticks: i64) -> anyhow::Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            self.http
                .post(self.url(&format!("/Sessions/{session_id}/Playing/Seek")))
                .query(&[
                    self.auth_param(),
                    ("SeekPositionTicks", &position_ticks.to_string()),
                ])
                .send()
                .await?;
            Ok(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("Seek timed out after 5s"))?
    }

    async fn pause_session(&self, session_id: &str) -> anyhow::Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            self.http
                .post(self.url(&format!("/Sessions/{session_id}/Playing/Pause")))
                .query(&[self.auth_param()])
                .send()
                .await?;
            Ok(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("Pause timed out after 5s"))?
    }

    async fn unpause_session(&self, session_id: &str) -> anyhow::Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            self.http
                .post(self.url(&format!("/Sessions/{session_id}/Playing/Unpause")))
                .query(&[self.auth_param()])
                .send()
                .await?;
            Ok(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("Unpause timed out after 5s"))?
    }
}
