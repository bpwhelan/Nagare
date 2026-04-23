use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tracing::{debug, info, warn};

use crate::config::{AnkiConfig, Config};
use crate::session::SessionState;

pub struct AnkiClient {
    url: String,
    http: Client,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnkiConnectionState {
    Unknown,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnkiStatus {
    pub state: AnkiConnectionState,
    pub message: Option<String>,
}

impl Default for AnkiStatus {
    fn default() -> Self {
        Self {
            state: AnkiConnectionState::Unknown,
            message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteInfo {
    pub note_id: i64,
    pub model_name: String,
    pub tags: Vec<String>,
    pub fields: HashMap<String, NoteField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteField {
    pub value: String,
    pub order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCardEvent {
    pub note_id: i64,
    pub sentence: String,
    pub fields: HashMap<String, NoteField>,
    pub model_name: String,
    pub tags: Vec<String>,
}

impl AnkiClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            http: Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|error| {
                    warn!("Failed to build timed AnkiConnect client: {}", error);
                    Client::new()
                }),
        }
    }

    async fn invoke(&self, action: &str, params: Value) -> anyhow::Result<Value> {
        self.invoke_retried(action, params, 0).await
    }

    /// Like `invoke`, but retries up to `max_retries` times with exponential backoff.
    /// Mirrors GSM's `invoke(action, retries=N)` pattern.
    async fn invoke_retried(
        &self,
        action: &str,
        params: Value,
        max_retries: u32,
    ) -> anyhow::Result<Value> {
        let mut backoff_ms: u64 = 500;
        const MAX_BACKOFF_MS: u64 = 5_000;

        let mut last_err = anyhow::anyhow!("no attempts");
        for attempt in 0..=max_retries {
            let body = json!({
                "action": action,
                "version": 6,
                "params": params,
            });

            match self.http.post(&self.url).json(&body).send().await {
                Ok(resp) => match resp.json::<Value>().await {
                    Ok(result) => {
                        if let Some(err) = result.get("error") {
                            if !err.is_null() {
                                // AnkiConnect-level errors are not retried (they're deterministic)
                                anyhow::bail!("AnkiConnect error: {}", err);
                            }
                        }
                        return Ok(result["result"].clone());
                    }
                    Err(e) => last_err = anyhow::anyhow!(e),
                },
                Err(e) => last_err = anyhow::anyhow!(e),
            }

            if attempt < max_retries {
                warn!(
                    "AnkiConnect {} failed (attempt {}/{}), retrying in {}ms: {}",
                    action,
                    attempt + 1,
                    max_retries + 1,
                    backoff_ms,
                    last_err
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
        Err(last_err)
    }

    pub async fn find_notes(&self, query: &str) -> anyhow::Result<Vec<i64>> {
        let result = self.invoke("findNotes", json!({ "query": query })).await?;
        let ids = result
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();
        Ok(ids)
    }

    pub async fn find_cards(&self, query: &str) -> anyhow::Result<Vec<i64>> {
        let result = self.invoke("findCards", json!({ "query": query })).await?;
        let ids = result
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();
        Ok(ids)
    }

    pub async fn find_cards_for_note(&self, note_id: i64) -> anyhow::Result<Vec<i64>> {
        self.find_cards(&format!("nid:{note_id}")).await
    }

    pub async fn notes_info(&self, note_ids: &[i64]) -> anyhow::Result<Vec<NoteInfo>> {
        let result = self
            .invoke("notesInfo", json!({ "notes": note_ids }))
            .await?;

        let notes = result
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let note_id = v["noteId"].as_i64()?;
                        let model_name = v["modelName"].as_str()?.to_string();
                        let tags: Vec<String> = v["tags"]
                            .as_array()
                            .map(|t| {
                                t.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();

                        let fields: HashMap<String, NoteField> = v["fields"]
                            .as_object()
                            .map(|obj| {
                                obj.iter()
                                    .map(|(k, v)| {
                                        let field = NoteField {
                                            value: v["value"].as_str().unwrap_or("").to_string(),
                                            order: v["order"].as_i64().unwrap_or(0) as i32,
                                        };
                                        (k.clone(), field)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        Some(NoteInfo {
                            note_id,
                            model_name,
                            tags,
                            fields,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(notes)
    }

    pub async fn update_note_fields(
        &self,
        note_id: i64,
        fields: HashMap<String, String>,
        audio: Option<Vec<AnkiMedia>>,
        picture: Option<Vec<AnkiMedia>>,
    ) -> anyhow::Result<()> {
        let mut note = json!({
            "id": note_id,
            "fields": fields,
        });

        if let Some(audio) = audio {
            note["audio"] = serde_json::to_value(audio)?;
        }
        if let Some(picture) = picture {
            note["picture"] = serde_json::to_value(picture)?;
        }

        self.invoke_retried("updateNoteFields", json!({ "note": note }), 2)
            .await?;
        Ok(())
    }

    pub async fn store_media_file(
        &self,
        filename: &str,
        data_base64: &str,
    ) -> anyhow::Result<String> {
        let result = self
            .invoke_retried(
                "storeMediaFile",
                json!({
                    "filename": filename,
                    "data": data_base64,
                }),
                5,
            )
            .await?;
        Ok(result.as_str().unwrap_or(filename).to_string())
    }

    pub async fn add_tags(&self, note_ids: &[i64], tags: &str) -> anyhow::Result<()> {
        self.invoke("addTags", json!({ "notes": note_ids, "tags": tags }))
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnkiMedia {
    pub filename: String,
    pub data: String, // base64
    pub fields: Vec<String>,
}

fn has_live_session(state: &SessionState) -> bool {
    !state.sessions.is_empty() || state.now_playing.is_some()
}

fn grace_remaining_since(last_live_session_at: Option<std::time::Instant>) -> Option<Duration> {
    const SESSION_POLL_GRACE: Duration = Duration::from_secs(600);

    last_live_session_at.map(|last_seen| SESSION_POLL_GRACE.saturating_sub(last_seen.elapsed()))
}

async fn set_idle_anki_status(status: &Arc<RwLock<AnkiStatus>>) {
    let mut current_status = status.write().await;
    *current_status = AnkiStatus {
        state: AnkiConnectionState::Unknown,
        message: Some("Waiting for an active playback session before polling AnkiConnect.".into()),
    };
}

/// Determine whether a note should be filtered out before surfacing it to the user.
///
/// Mirrors GSM's filter chain in `update_new_cards`:
/// 1. Note-type whitelist
/// 2. Ignore-tag blacklist
/// 3. Require-tag whitelist
/// 4. Media-field population (skip only when *all* configured fields already have values)
fn should_skip_note(note: &NoteInfo, cfg: &AnkiConfig) -> bool {
    // Note type filter
    if !cfg.note_types.is_empty() && !cfg.note_types.contains(&note.model_name) {
        debug!(
            "Skipping note {} (model '{}' not in note_types)",
            note.note_id, note.model_name
        );
        return true;
    }

    // Ignore-tags filter
    if !cfg.ignore_tags.is_empty() && note.tags.iter().any(|t| cfg.ignore_tags.contains(t)) {
        info!(
            "Skipping note {} (has ignored tag: {:?})",
            note.note_id, note.tags
        );
        return true;
    }

    // Require-tags filter
    if !cfg.require_tags.is_empty() && !note.tags.iter().any(|t| cfg.require_tags.contains(t)) {
        debug!("Skipping note {} (missing required tag)", note.note_id);
        return true;
    }

    // Media-field population check.
    //
    // GSM-inspired: skip only when there is *nothing to do* — i.e. every field
    // that is guarded by a skip flag already has a value.  If at least one
    // guarded field is still empty the card is worth surfacing.
    //
    // Examples with skip_if_audio=true, skip_if_picture=true (defaults):
    //   audio exists, picture exists  →  nothing to do → skip
    //   audio exists, picture empty   →  picture needs update → show
    //   audio empty,  picture exists  →  audio needs update   → show
    //   audio empty,  picture empty   →  both need update     → show
    let any_skip_flag_active = cfg.skip_if_audio_exists || cfg.skip_if_picture_exists;
    if any_skip_flag_active {
        let audio_already_set = cfg.skip_if_audio_exists
            && note
                .fields
                .get(&cfg.fields.sentence_audio)
                .map(|f| !f.value.trim().is_empty())
                .unwrap_or(false);

        let picture_already_set = cfg.skip_if_picture_exists
            && note
                .fields
                .get(&cfg.fields.picture)
                .map(|f| !f.value.trim().is_empty())
                .unwrap_or(false);

        // "Needs update" on an axis = the flag is active AND the field is empty.
        let needs_audio_update = cfg.skip_if_audio_exists && !audio_already_set;
        let needs_picture_update = cfg.skip_if_picture_exists && !picture_already_set;

        if !needs_audio_update && !needs_picture_update {
            debug!(
                "Skipping note {} (all configured media fields already populated)",
                note.note_id
            );
            return true;
        }
    }

    false
}

/// Polls AnkiConnect for newly added cards.
///
/// Reads config and client from shared RwLock so that URL/settings changes
/// take effect without restarting the poller.
pub async fn run_anki_poller(
    anki_client: Arc<RwLock<Arc<AnkiClient>>>,
    config: Arc<RwLock<Config>>,
    status: Arc<RwLock<AnkiStatus>>,
    tx: mpsc::Sender<NewCardEvent>,
    mut session_rx: watch::Receiver<SessionState>,
) {
    let mut known_ids: HashSet<i64> = HashSet::new();
    let mut initialized = false;
    let mut poller_idle = false;
    let mut last_live_session_at = if has_live_session(&session_rx.borrow()) {
        Some(std::time::Instant::now())
    } else {
        None
    };

    // Connection-state tracking
    let mut consecutive_errors: u32 = 0;
    let mut final_warning_shown = false;
    const MAX_LOGGED_ERRORS: u32 = 5;

    let max_interval = std::time::Duration::from_secs(5);

    loop {
        let session_state = session_rx.borrow().clone();
        let live_session = has_live_session(&session_state);
        if live_session {
            last_live_session_at = Some(std::time::Instant::now());
            poller_idle = false;
        } else if grace_remaining_since(last_live_session_at).is_none() {
            if !poller_idle {
                known_ids.clear();
                initialized = false;
                consecutive_errors = 0;
                final_warning_shown = false;
                set_idle_anki_status(&status).await;
                poller_idle = true;
            }

            if session_rx.changed().await.is_err() {
                return;
            }
            continue;
        }

        let client = anki_client.read().await.clone();
        let anki_config = config.read().await.anki.clone();
        let sentence_field = anki_config.fields.sentence.clone();
        let base_interval = std::time::Duration::from_millis(anki_config.polling_rate_ms);

        let wait_after_poll = match client.find_notes("added:1").await {
            Ok(current_ids) => {
                {
                    let mut current_status = status.write().await;
                    if current_status.state != AnkiConnectionState::Connected {
                        info!("AnkiConnect reachable at {}", anki_config.url);
                    }
                    *current_status = AnkiStatus {
                        state: AnkiConnectionState::Connected,
                        message: None,
                    };
                }

                // Connection restored after errors
                if consecutive_errors > 0 {
                    info!(
                        "AnkiConnect reconnected after {} failed poll(s)",
                        consecutive_errors
                    );
                    consecutive_errors = 0;
                    final_warning_shown = false;
                }

                let current_set: HashSet<i64> = current_ids.into_iter().collect();

                if !initialized {
                    known_ids = current_set;
                    initialized = true;
                    debug!(
                        "Anki poller initialized with {} known notes",
                        known_ids.len()
                    );
                } else {
                    // Only surface notes created within the last 5 minutes.
                    // Anki note IDs encode the creation time as Unix ms, so we can
                    // derive age without an extra API call.  This prevents a burst of
                    // stale notes firing if AnkiConnect was temporarily unreachable.
                    const MAX_NOTE_AGE_SECS: i64 = 300;
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    let new_ids: Vec<i64> = current_set
                        .difference(&known_ids)
                        .copied()
                        .filter(|&id| {
                            let created_secs = id / 1000;
                            (now_secs - created_secs).abs() <= MAX_NOTE_AGE_SECS
                        })
                        .collect();

                    if !new_ids.is_empty() {
                        debug!("Detected {} new Anki notes: {:?}", new_ids.len(), new_ids);

                        match client.notes_info(&new_ids).await {
                            Ok(notes) => {
                                for note in notes {
                                    if should_skip_note(&note, &anki_config) {
                                        continue;
                                    }

                                    let sentence = note
                                        .fields
                                        .get(&sentence_field)
                                        .map(|f| f.value.clone())
                                        .unwrap_or_default();

                                    let event = NewCardEvent {
                                        note_id: note.note_id,
                                        sentence,
                                        fields: note.fields,
                                        model_name: note.model_name,
                                        tags: note.tags,
                                    };

                                    if tx.send(event).await.is_err() {
                                        debug!("New-card processor dropped; stopping Anki poller");
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to fetch note info: {}", e);
                            }
                        }
                    }

                    known_ids = current_set;
                }

                base_interval
            }
            Err(e) => {
                consecutive_errors += 1;

                {
                    let mut current_status = status.write().await;
                    *current_status = AnkiStatus {
                        state: AnkiConnectionState::Disconnected,
                        message: Some(format!(
                            "Cannot reach AnkiConnect at {}. Card enrichment will stay unavailable until Anki is running. Last error: {}",
                            anki_config.url, e
                        )),
                    };
                }

                if !final_warning_shown {
                    if consecutive_errors >= MAX_LOGGED_ERRORS {
                        warn!(
                            "AnkiConnect unreachable after {} attempts — suppressing further \
                             warnings until reconnected. Make sure Anki is running and \
                             AnkiConnect is installed.",
                            consecutive_errors
                        );
                        final_warning_shown = true;
                    } else {
                        warn!(
                            "AnkiConnect poll failed ({}/{}): {}",
                            consecutive_errors, MAX_LOGGED_ERRORS, e
                        );
                    }
                }

                // Adaptive back-off: double the interval each consecutive failure,
                // capped at max_interval.
                let backoff = std::cmp::min(
                    base_interval * 2u32.pow(consecutive_errors.min(10)),
                    max_interval,
                );
                backoff
            }
        };

        let wait_duration = if live_session {
            wait_after_poll
        } else if let Some(remaining_grace) = grace_remaining_since(last_live_session_at) {
            wait_after_poll.min(remaining_grace)
        } else {
            Duration::ZERO
        };

        if wait_duration.is_zero() {
            continue;
        }

        tokio::select! {
            _ = tokio::time::sleep(wait_duration) => {}
            changed = session_rx.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}
