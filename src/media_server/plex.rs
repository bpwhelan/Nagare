use super::*;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, warn};
use uuid::Uuid;

struct PlexSubtitleStream {
    id: u64,
    codec: Option<String>,
    key: Option<String>,
}

struct SessionControlInfo {
    identifiers: Vec<String>,
    player_address: Option<String>,
}

/// A companion-capable client discovered via /clients or plex.tv resources.
struct DiscoveredClient {
    address: String,
    port: u16,
    protocol: String,
}

pub struct PlexClient {
    base_url: String,
    token: String,
    http: Client,
    client_identifier: String,
    command_id: AtomicU64,
}

impl PlexClient {
    pub fn new(url: &str, token: &str) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        Self {
            base_url,
            token: token.to_string(),
            http: Client::new(),
            client_identifier: format!("nagare-{}", Uuid::new_v4()),
            command_id: AtomicU64::new(1),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn with_plex_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("X-Plex-Product", "Nagare")
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
            .header("X-Plex-Platform", std::env::consts::OS)
            .header("X-Plex-Device", "Nagare")
            .header("X-Plex-Device-Name", "Nagare")
    }

    fn parse_stream_type(v: u64) -> StreamType {
        match v {
            1 => StreamType::Video,
            2 => StreamType::Audio,
            3 => StreamType::Subtitle,
            _ => StreamType::Other,
        }
    }

    fn value_as_u64(v: &Value) -> Option<u64> {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    }

    fn stream_lookup_index(v: &Value) -> u32 {
        Self::value_as_u64(&v["index"])
            .or_else(|| Self::value_as_u64(&v["id"]))
            .unwrap_or(0) as u32
    }

    fn parse_media_stream(v: &Value) -> MediaStream {
        let stream_type = Self::parse_stream_type(v["streamType"].as_u64().unwrap_or(0));
        let is_text_subtitle = if stream_type == StreamType::Subtitle {
            // In Plex, text subtitle codecs: srt, ass, ssa, vtt, subrip
            // Image subtitle codecs: pgs, vobsub, dvdsub
            let codec = v["codec"].as_str().unwrap_or("");
            matches!(
                codec,
                "srt" | "subrip" | "ass" | "ssa" | "vtt" | "webvtt" | "mov_text" | "text"
            )
        } else {
            false
        };
        let is_external = stream_type == StreamType::Subtitle
            && v["key"].as_str().map(|s| !s.is_empty()).unwrap_or(false);

        MediaStream {
            index: Self::stream_lookup_index(v),
            stream_type,
            codec: v["codec"].as_str().map(String::from),
            language: v["languageCode"].as_str().map(String::from),
            display_title: v["displayTitle"].as_str().map(String::from),
            is_default: v["default"].as_bool().unwrap_or(false),
            is_external,
            is_text_subtitle_stream: is_text_subtitle,
            title: v["title"].as_str().map(String::from),
        }
    }

    fn subtitle_extension(codec: Option<&str>, requested: SubtitleFormat) -> String {
        match codec
            .unwrap_or(requested.extension())
            .to_ascii_lowercase()
            .as_str()
        {
            "subrip" => "srt".to_string(),
            "webvtt" => "vtt".to_string(),
            "mov_text" | "text" => requested.extension().to_string(),
            other => other.to_string(),
        }
    }

    fn parse_session(v: &Value) -> Option<Session> {
        let player = &v["Player"];
        // Use Player.machineIdentifier as the session ID for remote control
        let id = player["machineIdentifier"].as_str()?.to_string();
        let client = player["product"].as_str().unwrap_or("Unknown").to_string();
        let device_name = player["title"]
            .as_str()
            .or_else(|| player["device"].as_str())
            .unwrap_or("Unknown")
            .to_string();
        let user_name = v["User"]["title"].as_str().map(String::from);
        let user_id = v["User"]["id"].as_u64().map(|id| id.to_string());
        // PMS companion control consistently rejects Plex Web sessions with 404,
        // even when they appear in /status/sessions, so don't advertise them as
        // remotely controllable through the server.
        let supports_remote_control = client != "Plex Web";

        let player_state = player["state"].as_str().unwrap_or("stopped");
        let is_paused = player_state == "paused";

        // Plex uses milliseconds — convert to ticks (10,000 per ms) for compatibility
        let view_offset_ms = v["viewOffset"].as_i64().unwrap_or(0);
        let position_ticks = Some(view_offset_ms * 10_000);

        // Find the selected audio and subtitle stream indices
        let mut audio_stream_index: Option<u32> = None;
        let mut subtitle_stream_index: Option<i32> = None;

        if let Some(media_arr) = v["Media"].as_array() {
            for media in media_arr {
                if let Some(parts) = media["Part"].as_array() {
                    for part in parts {
                        if let Some(streams) = part["Stream"].as_array() {
                            for stream in streams {
                                let st = stream["streamType"].as_u64().unwrap_or(0);
                                let selected = stream["selected"].as_bool().unwrap_or(false);
                                let idx = Self::stream_lookup_index(stream);
                                if st == 2 && selected {
                                    audio_stream_index = Some(idx);
                                }
                                if st == 3 && selected {
                                    subtitle_stream_index = Some(idx as i32);
                                }
                            }
                        }
                    }
                }
            }
        }

        let play_state = PlayState {
            can_seek: true,
            is_paused,
            position_ticks,
            audio_stream_index,
            subtitle_stream_index,
        };

        // Build NowPlaying from the metadata
        let item_id = v["ratingKey"].as_str().unwrap_or("").to_string();
        if item_id.is_empty() {
            return Some(Session {
                id,
                client,
                device_name,
                user_name,
                user_id,
                now_playing: None,
                play_state,
                supports_remote_control,
            });
        }

        // Collect media streams from all parts
        let mut media_streams = Vec::new();
        let mut media_source_id = None;
        let mut file_path = None;

        if let Some(media_arr) = v["Media"].as_array() {
            // Pick the selected media or the first one
            let media = media_arr
                .iter()
                .find(|m| m["selected"].as_bool().unwrap_or(false))
                .or_else(|| media_arr.first());

            if let Some(media) = media {
                if let Some(parts) = media["Part"].as_array() {
                    if let Some(part) = parts.first() {
                        media_source_id = Self::value_as_u64(&part["id"]).map(|id| id.to_string());
                        file_path = part["file"].as_str().map(String::from);

                        if let Some(streams) = part["Stream"].as_array() {
                            media_streams = streams.iter().map(Self::parse_media_stream).collect();
                        }
                    }
                }
            }
        }

        let duration_ms = v["duration"].as_i64().unwrap_or(0);

        let now_playing = Some(NowPlaying {
            item_id: item_id.clone(),
            name: v["title"].as_str().unwrap_or("").to_string(),
            series_name: v["grandparentTitle"].as_str().map(String::from),
            season_index: v["parentIndex"].as_u64().map(|v| v as u32),
            episode_index: v["index"].as_u64().map(|v| v as u32),
            media_type: v["type"].as_str().unwrap_or("video").to_string(),
            run_time_ticks: Some(duration_ms * 10_000),
            media_streams,
            media_source_id,
            path: file_path,
        });

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

    fn push_unique(values: &mut Vec<String>, candidate: Option<String>) {
        if let Some(candidate) = candidate {
            let trimmed = candidate.trim();
            if !trimmed.is_empty() && !values.iter().any(|value| value == trimmed) {
                values.push(trimmed.to_string());
            }
        }
    }

    async fn get_sessions_body(&self) -> anyhow::Result<Value> {
        let resp = self
            .with_plex_headers(self.http.get(self.url("/status/sessions")))
            .header("Accept", "application/json")
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    async fn resolve_session_control_info(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionControlInfo> {
        let mut identifiers = vec![session_id.to_string()];
        let mut player_address = None;
        let body = self.get_sessions_body().await?;

        if let Some(sessions) = body["MediaContainer"]["Metadata"].as_array() {
            for session in sessions {
                let player = &session["Player"];
                let machine_identifier = player["machineIdentifier"].as_str();
                if machine_identifier != Some(session_id) {
                    continue;
                }

                Self::push_unique(
                    &mut identifiers,
                    player["machineIdentifier"].as_str().map(String::from),
                );
                Self::push_unique(
                    &mut identifiers,
                    session["Session"]["id"].as_str().map(String::from),
                );
                Self::push_unique(
                    &mut identifiers,
                    player["playbackSessionId"].as_str().map(String::from),
                );
                Self::push_unique(
                    &mut identifiers,
                    player["playbackId"].as_str().map(String::from),
                );
                Self::push_unique(
                    &mut identifiers,
                    session["sessionKey"]
                        .as_u64()
                        .map(|value| value.to_string()),
                );

                player_address = player["address"].as_str().map(String::from);
                break;
            }
        }

        Ok(SessionControlInfo {
            identifiers,
            player_address,
        })
    }

    fn next_command_id(&self) -> u64 {
        self.command_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Query PMS /clients endpoint to discover companion-capable clients.
    async fn discover_clients_from_pms(&self, machine_identifier: &str) -> Vec<DiscoveredClient> {
        let resp = match self
            .with_plex_headers(self.http.get(self.url("/clients")))
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!("Failed to query /clients: {}", e);
                return Vec::new();
            }
        };

        let body: Value = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                debug!("Failed to parse /clients response: {}", e);
                return Vec::new();
            }
        };

        debug!("/clients response: {}", body);

        // PMS /clients returns Server array (JSON) or <Server> elements (XML).
        let servers = body["MediaContainer"]["Server"]
            .as_array()
            .or_else(|| body["MediaContainer"]["server"].as_array());

        let Some(servers) = servers else {
            return Vec::new();
        };

        servers
            .iter()
            .filter_map(|v| {
                let mid = v["machineIdentifier"].as_str()?;
                if mid != machine_identifier {
                    return None;
                }
                let host = v["host"].as_str().or_else(|| v["address"].as_str())?;
                let port = Self::value_as_u64(&v["port"]).unwrap_or(32433) as u16;
                let protocol = v["protocol"].as_str().unwrap_or("http").to_string();
                debug!(
                    "Discovered client via /clients: {}://{}:{}",
                    protocol, host, port
                );
                Some(DiscoveredClient {
                    address: host.to_string(),
                    port,
                    protocol,
                })
            })
            .collect()
    }

    /// Query plex.tv resources API to discover client connection URIs.
    async fn discover_clients_from_plex_tv(
        &self,
        machine_identifier: &str,
    ) -> Vec<DiscoveredClient> {
        let resp = match self
            .http
            .get("https://plex.tv/api/v2/resources")
            .query(&[("includeHttps", "1"), ("includeRelay", "1")])
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!("Failed to query plex.tv resources: {}", e);
                return Vec::new();
            }
        };

        let body: Value = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                debug!("Failed to parse plex.tv resources response: {}", e);
                return Vec::new();
            }
        };

        let resources = match body.as_array() {
            Some(arr) => arr,
            None => return Vec::new(),
        };

        let mut results = Vec::new();
        for resource in resources {
            let client_id = resource["clientIdentifier"].as_str().unwrap_or("");
            if client_id != machine_identifier {
                continue;
            }
            debug!(
                "Found matching plex.tv resource: {} ({})",
                resource["name"], client_id
            );
            if let Some(connections) = resource["connections"].as_array() {
                for conn in connections {
                    let address = match conn["address"].as_str() {
                        Some(a) => a,
                        None => continue,
                    };
                    let port = Self::value_as_u64(&conn["port"]).unwrap_or(32433) as u16;
                    let protocol = conn["protocol"].as_str().unwrap_or("http").to_string();
                    let uri = conn["uri"].as_str().unwrap_or("");
                    let is_local = conn["local"].as_bool().unwrap_or(false);
                    debug!(
                        "  connection: {}://{}:{} (uri={}, local={})",
                        protocol, address, port, uri, is_local
                    );
                    results.push(DiscoveredClient {
                        address: address.to_string(),
                        port,
                        protocol,
                    });
                }
            }
        }
        results
    }

    /// Send a command directly to the client device's HTTP endpoint,
    /// bypassing the PMS proxy. This is the non-proxy path from the
    /// Plex companion protocol.
    async fn send_direct_command(
        &self,
        client: &DiscoveredClient,
        path: &str,
        extra_query: &[(&str, String)],
    ) -> anyhow::Result<()> {
        let command_id = self.next_command_id();
        let url = format!(
            "{}://{}:{}{}",
            client.protocol, client.address, client.port, path
        );

        let mut query: Vec<(&str, String)> = vec![
            ("type", "video".to_string()),
            ("commandID", command_id.to_string()),
        ];
        query.extend(extra_query.iter().map(|(k, v)| (*k, v.clone())));

        let resp = self
            .http
            .get(&url)
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("X-Plex-Product", "Nagare")
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
            .header("X-Plex-Platform", std::env::consts::OS)
            .header("X-Plex-Device", "Nagare")
            .header("X-Plex-Device-Name", "Nagare")
            .query(&query)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let detail = body.lines().next().unwrap_or_default().trim();
        if detail.is_empty() {
            anyhow::bail!("HTTP {status}")
        } else {
            anyhow::bail!("HTTP {status}: {detail}")
        }
    }

    async fn send_playback_command(
        &self,
        session_id: &str,
        path: &str,
        extra_query: &[(&str, String)],
    ) -> anyhow::Result<()> {
        let info = self
            .resolve_session_control_info(session_id)
            .await
            .unwrap_or_else(|_| SessionControlInfo {
                identifiers: vec![session_id.to_string()],
                player_address: None,
            });

        // --- Try proxy through PMS server first ---
        let command_id = self.next_command_id();
        let mut failures = Vec::new();

        for target_id in &info.identifiers {
            let mut query = vec![
                ("type", "video".to_string()),
                ("commandID", command_id.to_string()),
            ];
            query.extend(extra_query.iter().map(|(key, value)| (*key, value.clone())));

            let resp = self
                .with_plex_headers(self.http.get(self.url(path)))
                .header("X-Plex-Target-Client-Identifier", target_id)
                .query(&query)
                .send()
                .await?;

            if resp.status().is_success() {
                return Ok(());
            }

            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let detail = body.lines().next().unwrap_or_default().trim();
            failures.push(if detail.is_empty() {
                format!("proxy {target_id} -> HTTP {status}")
            } else {
                format!("proxy {target_id} -> HTTP {status}: {detail}")
            });
        }

        // --- Proxy failed — discover direct client endpoints ---
        let machine_id = info
            .identifiers
            .first()
            .map(|s| s.as_str())
            .unwrap_or(session_id);

        // Collect all candidate direct endpoints: /clients, plex.tv, session address
        let mut candidates: Vec<DiscoveredClient> = Vec::new();
        candidates.extend(self.discover_clients_from_pms(machine_id).await);
        candidates.extend(self.discover_clients_from_plex_tv(machine_id).await);

        // Add the session-reported address as fallback with common companion ports
        if let Some(ref address) = info.player_address {
            // Deduplicate: only add if not already covered
            for (port, protocol) in [(32433, "http"), (32500, "http")] {
                let dominated = candidates
                    .iter()
                    .any(|c| c.address == *address && c.port == port);
                if !dominated {
                    candidates.push(DiscoveredClient {
                        address: address.clone(),
                        port,
                        protocol: protocol.to_string(),
                    });
                }
            }
        }

        if candidates.is_empty() {
            warn!("No direct client endpoints found for {}", machine_id);
        }

        for client in &candidates {
            debug!(
                "Trying direct {}://{}:{}",
                client.protocol, client.address, client.port
            );
            match self.send_direct_command(client, path, extra_query).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    failures.push(format!(
                        "direct {}://{}:{} -> {}",
                        client.protocol, client.address, client.port, e
                    ));
                }
            }
        }

        anyhow::bail!(
            "Plex remote control command failed. Targets tried: {}",
            failures.join(", ")
        )
    }

    /// Get the download URL for a Part by querying item metadata.
    async fn get_part_info(
        &self,
        item_id: &str,
        media_source_id: &str,
    ) -> anyhow::Result<(String, String)> {
        let resp = self
            .with_plex_headers(
                self.http
                    .get(self.url(&format!("/library/metadata/{}", item_id))),
            )
            .header("Accept", "application/json")
            .send()
            .await?;

        let body: Value = resp.json().await?;
        let metadata = body["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|arr| arr.first())
            .ok_or_else(|| anyhow::anyhow!("No metadata found for item {}", item_id))?;

        // Search through Media/Part to find the matching part
        if let Some(media_arr) = metadata["Media"].as_array() {
            for media in media_arr {
                if let Some(parts) = media["Part"].as_array() {
                    for part in parts {
                        let part_id = part["id"].as_u64().map(|id| id.to_string());
                        let part_id = part_id.or_else(|| part["id"].as_str().map(String::from));
                        if part_id.as_deref() == Some(media_source_id) || media_arr.len() == 1 {
                            let key = part["key"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("Part has no key"))?;
                            let file = part["file"].as_str().unwrap_or("").to_string();
                            return Ok((key.to_string(), file));
                        }
                    }
                }
            }
        }

        anyhow::bail!("Part not found for media_source_id {}", media_source_id)
    }

    /// Look up Plex metadata for a subtitle stream by file stream `index`.
    async fn find_plex_subtitle_stream(
        &self,
        item_id: &str,
        media_source_id: &str,
        stream_index: u32,
    ) -> anyhow::Result<PlexSubtitleStream> {
        let resp = self
            .with_plex_headers(
                self.http
                    .get(self.url(&format!("/library/metadata/{}", item_id))),
            )
            .header("Accept", "application/json")
            .send()
            .await?;

        let body: Value = resp.json().await?;
        let metadata = body["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|arr| arr.first())
            .ok_or_else(|| anyhow::anyhow!("No metadata for item {}", item_id))?;

        if let Some(media_arr) = metadata["Media"].as_array() {
            for media in media_arr {
                if let Some(parts) = media["Part"].as_array() {
                    for part in parts {
                        let part_id = part["id"].as_u64().map(|id| id.to_string());
                        let part_id = part_id.or_else(|| part["id"].as_str().map(String::from));
                        if part_id.as_deref() != Some(media_source_id) && media_arr.len() > 1 {
                            continue;
                        }
                        if let Some(streams) = part["Stream"].as_array() {
                            for stream in streams {
                                let st = stream["streamType"].as_u64().unwrap_or(0);
                                let idx = Self::stream_lookup_index(stream);
                                if st == 3 && idx == stream_index {
                                    return Ok(PlexSubtitleStream {
                                        id: Self::value_as_u64(&stream["id"])
                                            .ok_or_else(|| anyhow::anyhow!("Stream has no id"))?,
                                        codec: stream["codec"].as_str().map(String::from),
                                        key: stream["key"].as_str().map(String::from),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        anyhow::bail!(
            "Stream index {} not found in item {}",
            stream_index,
            item_id
        )
    }
}

#[async_trait]
impl MediaServer for PlexClient {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Plex
    }

    async fn get_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let body = self.get_sessions_body().await?;
        let sessions = body["MediaContainer"]["Metadata"]
            .as_array()
            .map(|arr| arr.iter().filter_map(Self::parse_session).collect())
            .unwrap_or_default();

        Ok(sessions)
    }

    async fn get_item_info(
        &self,
        item_id: &str,
        _user_id: Option<&str>,
    ) -> anyhow::Result<ItemInfo> {
        let resp = self
            .with_plex_headers(
                self.http
                    .get(self.url(&format!("/library/metadata/{}", item_id))),
            )
            .header("Accept", "application/json")
            .send()
            .await?;

        let body: Value = resp.json().await?;
        let metadata = body["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|arr| arr.first())
            .ok_or_else(|| anyhow::anyhow!("No metadata found for item {}", item_id))?;

        let mut media_streams = Vec::new();
        let mut media_sources = Vec::new();

        if let Some(media_arr) = metadata["Media"].as_array() {
            for media in media_arr {
                if let Some(parts) = media["Part"].as_array() {
                    for part in parts {
                        let streams: Vec<MediaStream> = part["Stream"]
                            .as_array()
                            .map(|arr| arr.iter().map(Self::parse_media_stream).collect())
                            .unwrap_or_default();

                        let source = MediaSource {
                            id: Self::value_as_u64(&part["id"])
                                .map(|id| id.to_string())
                                .or_else(|| part["id"].as_str().map(String::from))
                                .unwrap_or_default(),
                            path: part["file"].as_str().map(String::from),
                            media_streams: streams.clone(),
                        };
                        media_sources.push(source);

                        if media_streams.is_empty() {
                            media_streams = streams;
                        }
                    }
                }
            }
        }

        let path = media_sources.first().and_then(|s| s.path.clone());

        Ok(ItemInfo {
            id: metadata["ratingKey"].as_str().unwrap_or("").to_string(),
            name: metadata["title"].as_str().unwrap_or("").to_string(),
            path,
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
        // Look up the Plex stream metadata for the given stream index.
        let subtitle_stream = self
            .find_plex_subtitle_stream(item_id, media_source_id, stream_index)
            .await?;

        if let Some(stream_key) = subtitle_stream.key.as_deref() {
            debug!(
                "Trying Plex sidecar subtitle stream key for stream {}: {}",
                stream_index, stream_key
            );

            let resp = self
                .with_plex_headers(self.http.get(self.url(stream_key)))
                .send()
                .await?;

            if resp.status().is_success() {
                let content = resp.text().await?;
                if !content.trim().is_empty() && !content.contains("<html") {
                    debug!("Got subtitle from Plex sidecar stream key");
                    return Ok(content);
                }
            }
        }

        let ext = Self::subtitle_extension(subtitle_stream.codec.as_deref(), format);

        // Try /library/streams/{id}.{ext}?format=srt first (works for sidecar subs)
        let url = self.url(&format!("/library/streams/{}.{}", subtitle_stream.id, ext));
        debug!(
            "Trying Plex /library/streams/{}.{} for subtitle",
            subtitle_stream.id, ext
        );

        let resp = self
            .with_plex_headers(self.http.get(&url))
            .query(&[("format", "srt")])
            .send()
            .await?;

        if resp.status().is_success() {
            let content = resp.text().await?;
            if !content.trim().is_empty() && !content.contains("<html>") {
                debug!("Got subtitle from /library/streams endpoint");
                return Ok(content);
            }
        }

        // Fall back to ffmpeg extraction for embedded subtitles (501 case)
        debug!(
            "Falling back to ffmpeg extraction for embedded subtitle stream {}",
            stream_index
        );

        let (part_key, _file_path) = self.get_part_info(item_id, media_source_id).await?;
        let download_url = format!("{}{}?X-Plex-Token={}", self.base_url, part_key, self.token);

        let output = Command::new("ffmpeg")
            .args([
                "-i",
                &download_url,
                "-map",
                &format!("0:{}", stream_index),
                "-f",
                "srt",
                "-",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "ffmpeg subtitle extraction failed: {}",
                stderr.lines().last().unwrap_or("unknown error")
            );
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|e| anyhow::anyhow!("Subtitle content is not valid UTF-8: {}", e))?;

        if content.trim().is_empty() {
            anyhow::bail!("Extracted subtitle is empty (stream may be image-based)");
        }

        Ok(content)
    }

    fn get_stream_url(&self, _item_id: &str, media_source_id: &str) -> String {
        // media_source_id is the Plex Part ID
        format!(
            "{}/library/parts/{}/file?X-Plex-Token={}",
            self.base_url, media_source_id, self.token
        )
    }

    async fn seek_session(&self, session_id: &str, position_ticks: i64) -> anyhow::Result<()> {
        let offset_ms = position_ticks / 10_000;
        tokio::time::timeout(
            Duration::from_secs(5),
            self.send_playback_command(
                session_id,
                "/player/playback/seekTo",
                &[("offset", offset_ms.to_string())],
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Plex seek timed out after 5s"))?
    }

    async fn pause_session(&self, session_id: &str) -> anyhow::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.send_playback_command(session_id, "/player/playback/pause", &[]),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Plex pause timed out after 5s"))?
    }

    async fn unpause_session(&self, session_id: &str) -> anyhow::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.send_playback_command(session_id, "/player/playback/play", &[]),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Plex unpause timed out after 5s"))?
    }
}
