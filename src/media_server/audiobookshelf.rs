use super::*;
use anyhow::{Context, bail};
use async_trait::async_trait;
use reqwest::{Client, Response};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// ABS clients report progress periodically rather than exposing a live player
/// clock. Allow more than one reporting interval before treating a session as
/// probably paused. The reported position itself must never be projected.
const PLAYBACK_STALE_AFTER_MS: i64 = 25_000;
/// Downloaded playback is saved as a listening-history session rather than an
/// open session. Poll that slower endpoint at most once per second; the mobile
/// app itself only syncs local progress every 15 seconds on an unmetered link.
const LOCAL_SESSION_POLL_INTERVAL_MS: i64 = 1_000;
/// ABS does not explicitly close a downloaded playback session. Keep a recent
/// one visible while its user is online, but do not resurrect old history as a
/// current player indefinitely.
const LOCAL_SESSION_VISIBLE_AFTER_MS: i64 = 10 * 60 * 1_000;
const LOCAL_PLAY_METHOD: u64 = 3;

#[derive(Debug, Clone)]
struct AbsAudioTrack {
    index: u32,
    start_offset_seconds: f64,
    duration_seconds: Option<f64>,
    title: Option<String>,
    content_url: Option<String>,
    codec: Option<String>,
    language: Option<String>,
    path: String,
}

#[derive(Debug, Clone)]
struct AbsSessionDetails {
    library_item_id: String,
    display_title: String,
    series_name: Option<String>,
    media_type: String,
    audio_tracks: Vec<AbsAudioTrack>,
}

#[derive(Debug, Clone, Default)]
struct AbsLocalSessionSnapshot {
    fetched_at_ms: i64,
    sessions: Vec<Value>,
}

pub struct AudiobookshelfClient {
    base_url: String,
    token: String,
    http: Client,
    session_details: RwLock<HashMap<String, AbsSessionDetails>>,
    item_info: RwLock<HashMap<String, ItemInfo>>,
    local_sessions: RwLock<AbsLocalSessionSnapshot>,
}

impl AudiobookshelfClient {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            base_url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: Client::new(),
            session_details: RwLock::new(HashMap::new()),
            item_info: RwLock::new(HashMap::new()),
            local_sessions: RwLock::new(AbsLocalSessionSnapshot::default()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn get(&self, path: &str) -> anyhow::Result<Response> {
        Ok(self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await?)
    }

    fn sessions_from_response(body: &Value) -> Option<Vec<Value>> {
        body.get("sessions")
            .or_else(|| body.get("openSessions"))
            .and_then(Value::as_array)
            .cloned()
    }

    async fn get_open_sessions(&self) -> anyhow::Result<Vec<Value>> {
        let current_response = self.get("/api/sessions/open").await?;
        let current_status = current_response.status();
        if current_status.is_success() {
            let body: Value = current_response.json().await?;
            return Self::sessions_from_response(&body)
                .context("AudioBookShelf /api/sessions/open response omitted sessions");
        }

        // ABS versions predating /api/sessions/open expose the same collection
        // as openSessions on the online-users endpoint.
        let legacy_response = self.get("/api/users/online").await?;
        let legacy_status = legacy_response.status();
        if legacy_status.is_success() {
            let body: Value = legacy_response.json().await?;
            return Self::sessions_from_response(&body)
                .context("AudioBookShelf /api/users/online response omitted openSessions");
        }

        bail!(
            "Failed to fetch AudioBookShelf playback sessions (current endpoint: HTTP {}; legacy endpoint: HTTP {}). An admin API token is required",
            current_status,
            legacy_status
        )
    }

    async fn get_latest_local_sessions(&self, now_ms: i64) -> anyhow::Result<Vec<Value>> {
        {
            let snapshot = self.local_sessions.read().await;
            if now_ms.saturating_sub(snapshot.fetched_at_ms) < LOCAL_SESSION_POLL_INTERVAL_MS {
                return Ok(snapshot.sessions.clone());
            }
        }

        let response = self.get("/api/users/online").await?;
        if !response.status().is_success() {
            bail!(
                "Failed to fetch AudioBookShelf online users: HTTP {}. An admin API token is required",
                response.status()
            );
        }
        let body: Value = response.json().await?;
        let online_users = body["usersOnline"]
            .as_array()
            .context("AudioBookShelf /api/users/online response omitted usersOnline")?;

        let mut sessions = Vec::new();
        for user in online_users {
            let Some(user_id) = user["id"].as_str() else {
                continue;
            };
            let response = self
                .get(&format!(
                    "/api/users/{user_id}/listening-sessions?itemsPerPage=1&page=0"
                ))
                .await?;
            if !response.status().is_success() {
                tracing::warn!(
                    "Failed to fetch recent AudioBookShelf sessions for user {}: HTTP {}",
                    user_id,
                    response.status()
                );
                continue;
            }

            let body: Value = response.json().await?;
            let Some(mut session) = body["sessions"]
                .as_array()
                .and_then(|items| items.first())
                .cloned()
            else {
                continue;
            };
            if session["playMethod"].as_u64() != Some(LOCAL_PLAY_METHOD) {
                continue;
            }

            // Current ABS versions attach this object already. Fill it for
            // older versions so the normal session parser can expose the user.
            if !session["user"].is_object() {
                session["user"] = serde_json::json!({
                    "id": user_id,
                    "username": user["username"].as_str().unwrap_or("Unknown")
                });
            }
            if session["userId"].as_str().is_none() {
                session["userId"] = Value::String(user_id.to_string());
            }
            sessions.push(session);
        }

        *self.local_sessions.write().await = AbsLocalSessionSnapshot {
            fetched_at_ms: now_ms,
            sessions: sessions.clone(),
        };
        Ok(sessions)
    }

    fn session_updated_at(session: &Value) -> i64 {
        session["updatedAt"].as_i64().unwrap_or(0)
    }

    fn same_user(left: &Value, right: &Value) -> bool {
        left["userId"].as_str().is_some_and(|left_id| {
            right["userId"]
                .as_str()
                .is_some_and(|right_id| left_id == right_id)
        })
    }

    fn same_device(left: &Value, right: &Value) -> bool {
        left["deviceInfo"]["deviceId"]
            .as_str()
            .zip(right["deviceInfo"]["deviceId"].as_str())
            .is_some_and(|(left_id, right_id)| left_id == right_id)
    }

    fn local_session_supersedes_open(local: &Value, open: &Value, now_ms: i64) -> bool {
        if !Self::same_user(local, open)
            || Self::session_updated_at(local) <= Self::session_updated_at(open)
        {
            return false;
        }

        let open_is_stale =
            now_ms.saturating_sub(Self::session_updated_at(open)) > PLAYBACK_STALE_AFTER_MS;
        Self::same_device(local, open) || open_is_stale
    }

    fn is_visible_local_session(session: &Value, now_ms: i64) -> bool {
        session["playMethod"].as_u64() == Some(LOCAL_PLAY_METHOD)
            && now_ms.saturating_sub(Self::session_updated_at(session))
                <= LOCAL_SESSION_VISIBLE_AFTER_MS
    }

    fn language_by_track(library_item: &Value) -> HashMap<u32, String> {
        library_item["media"]["audioFiles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let index = file["index"].as_u64()? as u32;
                let language = file["language"].as_str()?.trim();
                (!language.is_empty()).then(|| (index, language.to_string()))
            })
            .collect()
    }

    fn is_supported_audio_path(path: &str) -> bool {
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mp3" | "m4b"))
            .unwrap_or(false)
    }

    fn parse_session_details(value: &Value) -> anyhow::Result<AbsSessionDetails> {
        let library_item_id = value["libraryItemId"]
            .as_str()
            .context("AudioBookShelf session omitted libraryItemId")?
            .to_string();
        let library_item = &value["libraryItem"];
        let languages = Self::language_by_track(library_item);
        let item_path = library_item["path"].as_str();

        let mut audio_tracks: Vec<AbsAudioTrack> = value["audioTracks"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(ordinal, track)| {
                let path = track["metadata"]["path"]
                    .as_str()
                    .or_else(|| {
                        (value["audioTracks"].as_array()?.len() == 1).then_some(item_path?)
                    })?
                    .to_string();
                if !Self::is_supported_audio_path(&path) {
                    return None;
                }

                let index = track["index"].as_u64().unwrap_or(ordinal as u64) as u32;
                Some(AbsAudioTrack {
                    index,
                    start_offset_seconds: track["startOffset"].as_f64().unwrap_or(0.0),
                    duration_seconds: track["duration"].as_f64(),
                    title: track["title"]
                        .as_str()
                        .or_else(|| track["metadata"]["filename"].as_str())
                        .map(String::from),
                    content_url: track["contentUrl"].as_str().map(String::from),
                    codec: track["codec"].as_str().map(String::from),
                    language: languages.get(&index).cloned().or_else(|| {
                        track["language"]
                            .as_str()
                            .map(str::trim)
                            .filter(|language| !language.is_empty())
                            .map(String::from)
                    }),
                    path,
                })
            })
            .collect();
        audio_tracks.sort_by(|left, right| {
            left.start_offset_seconds
                .total_cmp(&right.start_offset_seconds)
        });

        Ok(AbsSessionDetails {
            library_item_id,
            display_title: value["displayTitle"]
                .as_str()
                .or_else(|| library_item["media"]["metadata"]["title"].as_str())
                .unwrap_or("Audiobook")
                .to_string(),
            series_name: library_item["media"]["metadata"]["seriesName"]
                .as_str()
                .or_else(|| value["mediaMetadata"]["seriesName"].as_str())
                .or_else(|| value["mediaMetadata"]["series"][0]["name"].as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from),
            media_type: value["mediaType"].as_str().unwrap_or("book").to_string(),
            audio_tracks,
        })
    }

    async fn details_for_session(
        &self,
        session_id: &str,
        open_session: &Value,
    ) -> anyhow::Result<AbsSessionDetails> {
        if let Some(details) = self.session_details.read().await.get(session_id).cloned() {
            return Ok(details);
        }

        // Legacy online-user records and downloaded listening sessions may
        // already contain all audio tracks even when libraryItem is omitted.
        let details_value = if open_session["audioTracks"]
            .as_array()
            .is_some_and(|tracks| !tracks.is_empty())
        {
            open_session.clone()
        } else {
            let response = self.get(&format!("/api/session/{session_id}")).await?;
            if !response.status().is_success() {
                bail!(
                    "Failed to fetch AudioBookShelf session {}: HTTP {}",
                    session_id,
                    response.status()
                );
            }
            response.json().await?
        };

        let details = Self::parse_session_details(&details_value)?;
        self.session_details
            .write()
            .await
            .insert(session_id.to_string(), details.clone());
        Ok(details)
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    fn reported_position_seconds(session: &Value, now_ms: i64) -> (f64, bool) {
        let reported = session["currentTime"].as_f64().unwrap_or(0.0).max(0.0);
        let Some(updated_at) = session["updatedAt"].as_i64() else {
            return (reported, true);
        };
        let age_ms = now_ms.saturating_sub(updated_at).max(0);
        let is_probably_paused = age_ms > PLAYBACK_STALE_AFTER_MS;
        let duration = session["duration"].as_f64().unwrap_or(f64::INFINITY);
        (reported.min(duration), is_probably_paused)
    }

    fn active_track(tracks: &[AbsAudioTrack], position_seconds: f64) -> Option<&AbsAudioTrack> {
        tracks
            .iter()
            .rev()
            .find(|track| track.start_offset_seconds <= position_seconds)
            .or_else(|| tracks.first())
    }

    fn media_stream(track: &AbsAudioTrack) -> MediaStream {
        MediaStream {
            index: track.index,
            stream_type: StreamType::Audio,
            codec: track.codec.clone(),
            language: track.language.clone(),
            display_title: track.title.clone(),
            is_default: true,
            is_external: false,
            is_text_subtitle_stream: false,
            title: track.title.clone(),
        }
    }

    fn device_name(session: &Value) -> String {
        for field in ["clientName", "model", "browserName", "deviceType"] {
            if let Some(value) = session["deviceInfo"][field].as_str()
                && !value.trim().is_empty()
            {
                return value.to_string();
            }
        }
        "AudioBookShelf Player".to_string()
    }

    fn parse_session(
        &self,
        value: &Value,
        details: &AbsSessionDetails,
        now_ms: i64,
    ) -> Option<(Session, ItemInfo)> {
        let id = value["id"].as_str()?.to_string();
        let (global_position_seconds, is_paused) = Self::reported_position_seconds(value, now_ms);
        let track = Self::active_track(&details.audio_tracks, global_position_seconds)?;
        let local_position_seconds =
            (global_position_seconds - track.start_offset_seconds).max(0.0);
        let track_count = details.audio_tracks.len();
        let item_id = if track_count > 1 {
            format!("{}:abs-track:{}", details.library_item_id, track.index)
        } else {
            details.library_item_id.clone()
        };
        let title = if track_count > 1 {
            track
                .title
                .as_ref()
                .map(|track_title| format!("{} — {}", details.display_title, track_title))
                .unwrap_or_else(|| details.display_title.clone())
        } else {
            details.display_title.clone()
        };
        let media_source_id = track
            .content_url
            .clone()
            .unwrap_or_else(|| format!("abs-track-{}", track.index));
        let stream = Self::media_stream(track);
        let duration_seconds = track
            .duration_seconds
            .or_else(|| value["duration"].as_f64());
        let duration_ticks = duration_seconds.map(|duration| (duration * 10_000_000.0) as i64);
        let position_ticks = Some((local_position_seconds * 10_000_000.0) as i64);

        let now_playing = NowPlaying {
            item_id: item_id.clone(),
            name: title.clone(),
            series_name: details.series_name.clone(),
            season_index: None,
            episode_index: None,
            media_type: details.media_type.clone(),
            run_time_ticks: duration_ticks,
            media_streams: vec![stream.clone()],
            media_source_id: Some(media_source_id.clone()),
            path: Some(track.path.clone()),
        };
        let item_info = ItemInfo {
            id: item_id,
            name: title,
            path: Some(track.path.clone()),
            media_streams: vec![stream.clone()],
            media_sources: vec![MediaSource {
                id: media_source_id,
                path: Some(track.path.clone()),
                media_streams: vec![stream],
            }],
        };

        Some((
            Session {
                id,
                client: value["mediaPlayer"]
                    .as_str()
                    .unwrap_or("AudioBookShelf")
                    .to_string(),
                device_name: Self::device_name(value),
                user_name: value["user"]["username"].as_str().map(String::from),
                user_id: value["userId"].as_str().map(String::from),
                now_playing: Some(now_playing),
                play_state: PlayState {
                    can_seek: false,
                    is_paused,
                    position_ticks,
                    audio_stream_index: Some(track.index),
                    subtitle_stream_index: None,
                },
                supports_remote_control: false,
            },
            item_info,
        ))
    }

    fn stream_url(&self, content_url: &str) -> String {
        let separator = if content_url.contains('?') { '&' } else { '?' };
        format!(
            "{}{}{}token={}",
            self.base_url, content_url, separator, self.token
        )
    }
}

#[async_trait]
impl MediaServer for AudiobookshelfClient {
    fn kind(&self) -> MediaServerKind {
        MediaServerKind::Audiobookshelf
    }

    async fn get_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let now_ms = Self::now_ms();
        let mut open_sessions = self.get_open_sessions().await?;
        let latest_local_sessions = match self.get_latest_local_sessions(now_ms).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(
                    "Failed to discover downloaded AudioBookShelf playback: {}",
                    error
                );
                let mut snapshot = self.local_sessions.write().await;
                snapshot.fetched_at_ms = now_ms;
                snapshot.sessions.clone()
            }
        };

        // A downloaded session never enters ABS's in-memory open-session list.
        // The same Android player can therefore leave an older streamed session
        // behind; hide that stale record in favor of the newer local one.
        open_sessions.retain(|open| {
            !latest_local_sessions
                .iter()
                .any(|local| Self::local_session_supersedes_open(local, open, now_ms))
        });

        let mut discovered_sessions: Vec<Value> = latest_local_sessions
            .into_iter()
            .filter(|session| Self::is_visible_local_session(session, now_ms))
            .collect();
        // Prefer a currently syncing local session during automatic selection.
        discovered_sessions.extend(open_sessions);

        let active_ids: Vec<String> = discovered_sessions
            .iter()
            .filter_map(|session| session["id"].as_str().map(String::from))
            .collect();
        self.session_details
            .write()
            .await
            .retain(|id, _| active_ids.contains(id));

        let mut sessions = Vec::new();
        for open_session in discovered_sessions {
            let Some(session_id) = open_session["id"].as_str() else {
                continue;
            };
            let details = match self.details_for_session(session_id, &open_session).await {
                Ok(details) => details,
                Err(error) => {
                    tracing::warn!(
                        "Failed to resolve AudioBookShelf session {}: {}",
                        session_id,
                        error
                    );
                    continue;
                }
            };
            if let Some((session, item_info)) = self.parse_session(&open_session, &details, now_ms)
            {
                self.item_info
                    .write()
                    .await
                    .insert(item_info.id.clone(), item_info);
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    async fn get_users(&self) -> anyhow::Result<Vec<MediaUser>> {
        let response = self.get("/api/users").await?;
        if !response.status().is_success() {
            bail!(
                "Failed to fetch AudioBookShelf users: HTTP {}. An admin API token is required",
                response.status()
            );
        }
        let body: Value = response.json().await?;
        Ok(body
            .get("users")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|user| {
                Some(MediaUser {
                    id: user["id"].as_str()?.to_string(),
                    name: user["username"].as_str().unwrap_or("Unknown").to_string(),
                })
            })
            .collect())
    }

    async fn get_item_info(
        &self,
        item_id: &str,
        _user_id: Option<&str>,
    ) -> anyhow::Result<ItemInfo> {
        self.item_info
            .read()
            .await
            .get(item_id)
            .cloned()
            .with_context(|| format!("No cached AudioBookShelf item info for {item_id}"))
    }

    async fn get_subtitles(
        &self,
        _item_id: &str,
        _media_source_id: &str,
        _stream_index: u32,
        _format: SubtitleFormat,
    ) -> anyhow::Result<String> {
        bail!("AudioBookShelf subtitles are loaded from local sidecar files")
    }

    fn get_stream_url(&self, _item_id: &str, media_source_id: &str) -> String {
        if media_source_id.starts_with('/') {
            self.stream_url(media_source_id)
        } else {
            String::new()
        }
    }

    async fn seek_session(&self, _session_id: &str, _position_ticks: i64) -> anyhow::Result<()> {
        bail!("AudioBookShelf does not expose remote playback controls")
    }

    async fn pause_session(&self, _session_id: &str) -> anyhow::Result<()> {
        bail!("AudioBookShelf does not expose remote playback controls")
    }

    async fn unpause_session(&self, _session_id: &str) -> anyhow::Result<()> {
        bail!("AudioBookShelf does not expose remote playback controls")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expanded_session() -> Value {
        json!({
            "libraryItemId": "book-1",
            "displayTitle": "Japanese Audiobook",
            "mediaType": "book",
            "audioTracks": [
                {
                    "index": 0,
                    "startOffset": 0.0,
                    "duration": 600.0,
                    "title": "Japanese Audiobook.m4b",
                    "contentUrl": "/api/items/book-1/file/123",
                    "codec": "aac",
                    "metadata": {
                        "filename": "Japanese Audiobook.m4b",
                        "path": "/audiobooks/Japanese Audiobook/Japanese Audiobook.m4b"
                    }
                }
            ],
            "libraryItem": {
                "path": "/audiobooks/Japanese Audiobook",
                "media": {
                    "metadata": {
                        "title": "Japanese Audiobook",
                        "seriesName": "Japanese Series"
                    },
                    "audioFiles": [{ "index": 0, "language": "jpn" }]
                }
            }
        })
    }

    fn local_session() -> Value {
        json!({
            "id": "local-session-1",
            "userId": "user-1",
            "libraryItemId": "book-1",
            "displayTitle": "Downloaded Japanese Audiobook",
            "mediaType": "book",
            "playMethod": 3,
            "currentTime": 75.0,
            "duration": 600.0,
            "updatedAt": 20_000,
            "mediaMetadata": {
                "series": [{ "name": "Japanese Series" }]
            },
            "deviceInfo": {
                "deviceId": "android-1",
                "clientName": "Abs Android"
            },
            "audioTracks": [{
                "index": 0,
                "startOffset": 0.0,
                "duration": 600.0,
                "title": "Japanese Audiobook.m4b",
                "language": "jpn",
                "metadata": {
                    "path": "/downloads/Japanese Audiobook.m4b"
                }
            }]
        })
    }

    #[test]
    fn extracts_current_and_legacy_open_session_arrays() {
        let sessions = vec![json!({ "id": "session-1" })];
        assert_eq!(
            AudiobookshelfClient::sessions_from_response(&json!({ "sessions": sessions })),
            Some(sessions.clone())
        );
        assert_eq!(
            AudiobookshelfClient::sessions_from_response(&json!({ "openSessions": sessions })),
            Some(sessions)
        );
    }

    #[test]
    fn parses_supported_single_file_track_and_language() {
        let details = AudiobookshelfClient::parse_session_details(&expanded_session()).unwrap();
        assert_eq!(details.library_item_id, "book-1");
        assert_eq!(details.series_name.as_deref(), Some("Japanese Series"));
        assert_eq!(details.audio_tracks.len(), 1);
        assert_eq!(details.audio_tracks[0].language.as_deref(), Some("jpn"));
        assert!(details.audio_tracks[0].path.ends_with(".m4b"));
    }

    #[test]
    fn parses_downloaded_session_without_expanded_library_item() {
        let details = AudiobookshelfClient::parse_session_details(&local_session()).unwrap();
        assert_eq!(details.display_title, "Downloaded Japanese Audiobook");
        assert_eq!(details.series_name.as_deref(), Some("Japanese Series"));
        assert_eq!(details.audio_tracks.len(), 1);
        assert_eq!(details.audio_tracks[0].language.as_deref(), Some("jpn"));
    }

    #[test]
    fn newer_downloaded_session_supersedes_stale_open_session() {
        let local = local_session();
        let open = json!({
            "id": "open-session-1",
            "userId": "user-1",
            "updatedAt": 10_000,
            "deviceInfo": { "deviceId": "android-1" }
        });
        assert!(AudiobookshelfClient::local_session_supersedes_open(
            &local, &open, 21_000
        ));

        let newer_open = json!({
            "id": "open-session-2",
            "userId": "user-1",
            "updatedAt": 30_000,
            "deviceInfo": { "deviceId": "android-1" }
        });
        assert!(!AudiobookshelfClient::local_session_supersedes_open(
            &local,
            &newer_open,
            31_000
        ));
    }

    #[test]
    fn downloaded_session_visibility_expires() {
        let local = local_session();
        assert!(AudiobookshelfClient::is_visible_local_session(
            &local, 21_000
        ));
        assert!(!AudiobookshelfClient::is_visible_local_session(
            &local,
            20_000 + LOCAL_SESSION_VISIBLE_AFTER_MS + 1
        ));
    }

    #[test]
    fn preserves_reported_progress_and_marks_stale_sessions_paused() {
        let recent = json!({
            "currentTime": 100.0,
            "duration": 500.0,
            "updatedAt": 1_000
        });
        assert_eq!(
            AudiobookshelfClient::reported_position_seconds(&recent, 6_000),
            (100.0, false)
        );
        assert_eq!(
            AudiobookshelfClient::reported_position_seconds(&recent, 31_000),
            (100.0, true)
        );
    }

    #[test]
    fn selects_track_using_global_book_position() {
        let tracks = vec![
            AbsAudioTrack {
                index: 0,
                start_offset_seconds: 0.0,
                duration_seconds: Some(60.0),
                title: None,
                content_url: None,
                codec: None,
                language: None,
                path: "one.mp3".to_string(),
            },
            AbsAudioTrack {
                index: 1,
                start_offset_seconds: 60.0,
                duration_seconds: Some(60.0),
                title: None,
                content_url: None,
                codec: None,
                language: None,
                path: "two.mp3".to_string(),
            },
        ];
        assert_eq!(
            AudiobookshelfClient::active_track(&tracks, 75.0).map(|track| track.index),
            Some(1)
        );
    }
}
