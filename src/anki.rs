use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

#[derive(Debug, Clone)]
pub struct NewCardNotification {
    pub event: NewCardEvent,
    pub card_ids: Option<Vec<i64>>,
    pub needs_enhancement: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnkiBeaconEventKind {
    Heartbeat,
    NoteAdded,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AnkiBeaconEvent {
    #[serde(default)]
    pub addon: Option<String>,
    #[serde(default)]
    pub addon_name: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub event: AnkiBeaconEventKind,
    #[serde(default)]
    pub note_id: Option<i64>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub heartbeat_interval_seconds: Option<f64>,
    #[serde(default)]
    pub payload_mode: Option<String>,
    #[serde(default)]
    pub note_type_id: Option<i64>,
    #[serde(default)]
    pub note_type_name: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_note_fields")]
    pub fields: Option<HashMap<String, NoteField>>,
    #[serde(default)]
    pub card_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub cards: Option<Vec<Value>>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

impl AnkiBeaconEvent {
    pub fn heartbeat_interval(&self) -> Duration {
        let seconds = self
            .heartbeat_interval_seconds
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(10.0);
        Duration::from_secs_f64(seconds)
    }

    fn note_info(&self) -> Option<NoteInfo> {
        Some(NoteInfo {
            note_id: self.note_id?,
            model_name: self.note_type_name.clone()?,
            tags: self.tags.clone().unwrap_or_default(),
            fields: self.fields.clone()?,
        })
    }

    fn provided_card_ids(&self) -> Option<Vec<i64>> {
        if let Some(card_ids) = self.card_ids.as_ref().filter(|ids| !ids.is_empty()) {
            return Some(card_ids.clone());
        }

        let ids: Vec<i64> = self
            .cards
            .as_ref()?
            .iter()
            .filter_map(card_id_from_value)
            .collect();

        if ids.is_empty() { None } else { Some(ids) }
    }
}

fn deserialize_optional_note_fields<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, NoteField>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<HashMap<String, Value>>::deserialize(deserializer)?;
    Ok(raw.map(|fields| {
        fields
            .into_iter()
            .map(|(name, value)| (name, note_field_from_value(value)))
            .collect()
    }))
}

fn note_field_from_value(value: Value) -> NoteField {
    match value {
        Value::String(value) => NoteField { value, order: 0 },
        Value::Object(mut field) => NoteField {
            value: field
                .remove("value")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            order: field
                .remove("order")
                .and_then(|value| value.as_i64())
                .unwrap_or(0) as i32,
        },
        _ => NoteField {
            value: String::new(),
            order: 0,
        },
    }
}

fn card_id_from_value(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value.as_object().and_then(|card| {
            card.get("card_id")
                .or_else(|| card.get("cardId"))
                .or_else(|| card.get("id"))
                .and_then(Value::as_i64)
        })
    })
}

impl AnkiClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            http: Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                // AnkiConnect is a small local/LAN HTTP server; avoiding idle
                // connection reuse prevents stale pooled sockets from turning
                // into recurring one-off poll failures.
                .pool_max_idle_per_host(0)
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

    last_live_session_at.and_then(|last_seen| {
        let remaining = SESSION_POLL_GRACE.saturating_sub(last_seen.elapsed());
        if remaining.is_zero() {
            None
        } else {
            Some(remaining)
        }
    })
}

async fn set_idle_anki_status(status: &Arc<RwLock<AnkiStatus>>) {
    let mut current_status = status.write().await;
    *current_status = AnkiStatus {
        state: AnkiConnectionState::Unknown,
        message: Some(
            "Waiting for an active playback session before polling AnkiConnect fallback.".into(),
        ),
    };
}

pub fn note_info_to_event(note: NoteInfo, sentence_field: &str) -> NewCardEvent {
    let sentence = note
        .fields
        .get(sentence_field)
        .map(|field| field.value.clone())
        .unwrap_or_default();

    NewCardEvent {
        note_id: note.note_id,
        sentence,
        fields: note.fields,
        model_name: note.model_name,
        tags: note.tags,
    }
}

/// Determine whether a note should be filtered out before surfacing it to the user.
///
/// Mirrors GSM's filter chain in `update_new_cards`:
/// 1. Note-type whitelist
/// 2. Ignore-tag blacklist
/// 3. Require-tag whitelist
/// 4. Media-field population (skip only when *all* configured fields already have values)
fn should_skip_note(note: &NoteInfo, cfg: &AnkiConfig) -> bool {
    let sentence = note
        .fields
        .get(&cfg.fields.sentence)
        .map(|field| field.value.trim())
        .unwrap_or_default();
    if sentence.is_empty() {
        debug!(
            "Skipping note {} (configured sentence field '{}' is empty or missing)",
            note.note_id, cfg.fields.sentence
        );
        return true;
    }

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

#[derive(Debug, Clone)]
struct HeartbeatState {
    received_at: Instant,
    session_id: Option<String>,
    interval: Duration,
}

impl HeartbeatState {
    fn stale_after(&self) -> Duration {
        std::cmp::max(Duration::from_secs(30), self.interval.saturating_mul(3))
    }

    fn is_fresh(&self) -> bool {
        self.received_at.elapsed() < self.stale_after()
    }
}

fn heartbeat_is_fresh(heartbeat: Option<&HeartbeatState>) -> bool {
    heartbeat.map(HeartbeatState::is_fresh).unwrap_or(false)
}

async fn set_disconnected_anki_status(status: &Arc<RwLock<AnkiStatus>>, message: String) {
    let mut current_status = status.write().await;
    *current_status = AnkiStatus {
        state: AnkiConnectionState::Disconnected,
        message: Some(message),
    };
}

async fn emit_note_info(
    note: NoteInfo,
    anki_config: &AnkiConfig,
    tx: &mpsc::Sender<NewCardNotification>,
    card_ids: Option<Vec<i64>>,
) -> bool {
    let mut intake_config = anki_config.clone();
    intake_config.skip_if_audio_exists = false;
    intake_config.skip_if_picture_exists = false;
    if should_skip_note(&note, &intake_config) {
        return true;
    }
    let needs_enhancement = !should_skip_note(&note, anki_config);

    let event = note_info_to_event(note, &anki_config.fields.sentence);
    if tx
        .send(NewCardNotification {
            event,
            card_ids,
            needs_enhancement,
        })
        .await
        .is_err()
    {
        debug!("New-card processor dropped; stopping Anki event producer");
        return false;
    }

    true
}

enum PollStep {
    Continue(Duration),
    Stop,
}

async fn poll_ankiconnect_once(
    client: Arc<AnkiClient>,
    anki_config: AnkiConfig,
    status: &Arc<RwLock<AnkiStatus>>,
    tx: &mpsc::Sender<NewCardNotification>,
    known_ids: &mut HashSet<i64>,
    initialized: &mut bool,
    consecutive_errors: &mut u32,
    final_warning_shown: &mut bool,
) -> PollStep {
    let base_interval = Duration::from_millis(anki_config.polling_rate_ms);
    let max_interval = Duration::from_secs(5);
    const DISCONNECT_AFTER_ERRORS: u32 = 5;

    match client.find_notes("added:1").await {
        Ok(current_ids) => {
            let was_disconnected = {
                let mut current_status = status.write().await;
                let was_disconnected = current_status.state == AnkiConnectionState::Disconnected;
                if current_status.state != AnkiConnectionState::Connected {
                    info!("AnkiConnect reachable at {}", anki_config.url);
                }
                *current_status = AnkiStatus {
                    state: AnkiConnectionState::Connected,
                    message: None,
                };
                was_disconnected
            };

            if *consecutive_errors > 0 {
                if was_disconnected || *final_warning_shown {
                    info!(
                        "AnkiConnect reconnected after {} failed poll(s)",
                        *consecutive_errors
                    );
                } else {
                    debug!(
                        "AnkiConnect recovered after {} transient failed poll(s)",
                        *consecutive_errors
                    );
                }
                *consecutive_errors = 0;
                *final_warning_shown = false;
            }

            let current_set: HashSet<i64> = current_ids.into_iter().collect();

            if !*initialized {
                *known_ids = current_set;
                *initialized = true;
                debug!(
                    "Anki polling fallback initialized with {} known notes",
                    known_ids.len()
                );
            } else {
                // Only surface notes created within the last 5 minutes.
                // Anki note IDs encode the creation time as Unix ms, so we can
                // derive age without an extra API call. This prevents a burst of
                // stale notes firing if AnkiConnect was temporarily unreachable.
                const MAX_NOTE_AGE_SECS: i64 = 300;
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;

                let new_ids: Vec<i64> = current_set
                    .difference(known_ids)
                    .copied()
                    .filter(|&id| {
                        let created_secs = id / 1000;
                        (now_secs - created_secs).abs() <= MAX_NOTE_AGE_SECS
                    })
                    .collect();

                if !new_ids.is_empty() {
                    debug!(
                        "Polling fallback detected {} new Anki notes: {:?}",
                        new_ids.len(),
                        new_ids
                    );

                    match client.notes_info(&new_ids).await {
                        Ok(notes) => {
                            for note in notes {
                                if !emit_note_info(note, &anki_config, tx, None).await {
                                    return PollStep::Stop;
                                }
                            }
                        }
                        Err(error) => {
                            warn!("Failed to fetch note info: {}", error);
                            return PollStep::Continue(base_interval);
                        }
                    }
                }

                *known_ids = current_set;
            }

            PollStep::Continue(base_interval)
        }
        Err(error) => {
            *consecutive_errors += 1;
            if *consecutive_errors >= DISCONNECT_AFTER_ERRORS {
                set_disconnected_anki_status(
                    status,
                    format!(
                        "Cannot reach AnkiConnect at {}. Card enrichment will stay unavailable until Anki is running. Last error: {}",
                        anki_config.url, error
                    ),
                )
                .await;
            }

            if !*final_warning_shown {
                if *consecutive_errors >= DISCONNECT_AFTER_ERRORS {
                    warn!(
                        "AnkiConnect unreachable after {} attempts - suppressing further \
                         warnings until reconnected. Make sure Anki is running and \
                         AnkiConnect is installed.",
                        *consecutive_errors
                    );
                    *final_warning_shown = true;
                } else {
                    debug!(
                        "AnkiConnect transient poll failure ({}/{}): {}",
                        *consecutive_errors, DISCONNECT_AFTER_ERRORS, error
                    );
                }
            }

            let backoff = std::cmp::min(
                base_interval * 2u32.pow(consecutive_errors.saturating_sub(1).min(10)),
                max_interval,
            );
            PollStep::Continue(backoff)
        }
    }
}

async fn handle_ankibeacon_event(
    payload: AnkiBeaconEvent,
    anki_client: &Arc<RwLock<Arc<AnkiClient>>>,
    config: &Arc<RwLock<Config>>,
    status: &Arc<RwLock<AnkiStatus>>,
    tx: &mpsc::Sender<NewCardNotification>,
    heartbeat: &mut Option<HeartbeatState>,
    processed_push_notes: &mut HashSet<i64>,
    consecutive_errors: &mut u32,
    final_warning_shown: &mut bool,
) -> bool {
    match payload.event {
        AnkiBeaconEventKind::Heartbeat => {
            let was_fresh = heartbeat_is_fresh(heartbeat.as_ref());
            *heartbeat = Some(HeartbeatState {
                received_at: Instant::now(),
                session_id: payload.session_id.clone(),
                interval: payload.heartbeat_interval(),
            });
            *consecutive_errors = 0;
            *final_warning_shown = false;

            {
                let mut current_status = status.write().await;
                *current_status = AnkiStatus {
                    state: AnkiConnectionState::Connected,
                    message: None,
                };
            }

            if !was_fresh {
                if let Some(session_id) = heartbeat
                    .as_ref()
                    .and_then(|state| state.session_id.as_deref())
                {
                    info!(
                        "AnkiBeacon heartbeat received for session {}; using push events while heartbeat is fresh",
                        session_id
                    );
                } else {
                    info!(
                        "AnkiBeacon heartbeat received; using push events while heartbeat is fresh"
                    );
                }
            }

            true
        }
        AnkiBeaconEventKind::NoteAdded => {
            let Some(note_id) = payload.note_id else {
                warn!("Ignoring AnkiBeacon note_added event without note_id");
                return true;
            };

            if processed_push_notes.contains(&note_id) {
                debug!(
                    "Ignoring duplicate AnkiBeacon note_added event for note {}",
                    note_id
                );
                return true;
            }

            {
                let mut current_status = status.write().await;
                *current_status = AnkiStatus {
                    state: AnkiConnectionState::Connected,
                    message: None,
                };
            }

            let anki_config = config.read().await.anki.clone();
            let card_ids = payload.provided_card_ids();

            if let Some(note) = payload.note_info() {
                let handled = emit_note_info(note, &anki_config, tx, card_ids).await;
                if handled {
                    processed_push_notes.insert(note_id);
                }
                return handled;
            }

            let client = anki_client.read().await.clone();
            match client.notes_info(&[note_id]).await {
                Ok(notes) => {
                    for note in notes {
                        if !emit_note_info(note, &anki_config, tx, card_ids.clone()).await {
                            return false;
                        }
                    }
                    processed_push_notes.insert(note_id);
                }
                Err(error) => {
                    warn!(
                        "Failed to fetch note info for AnkiBeacon note {}: {}",
                        note_id, error
                    );
                    set_disconnected_anki_status(
                        status,
                        format!(
                            "Cannot reach AnkiConnect at {}. Card enrichment will stay unavailable until Anki is running. Last error: {}",
                            anki_config.url, error
                        ),
                    )
                    .await;
                }
            }

            true
        }
    }
}

/// Receives AnkiBeacon push events and falls back to AnkiConnect polling only
/// when no fresh heartbeat is available.
///
/// Reads config and client from shared RwLock so that URL/settings changes
/// take effect without restarting the producer.
pub async fn run_anki_poller(
    anki_client: Arc<RwLock<Arc<AnkiClient>>>,
    config: Arc<RwLock<Config>>,
    status: Arc<RwLock<AnkiStatus>>,
    tx: mpsc::Sender<NewCardNotification>,
    mut event_rx: mpsc::Receiver<AnkiBeaconEvent>,
    session_rx: watch::Receiver<SessionState>,
) {
    let mut known_ids = HashSet::new();
    let mut initialized = false;
    let mut heartbeat = None;
    let mut processed_push_notes = HashSet::new();
    let mut consecutive_errors = 0;
    let mut final_warning_shown = false;
    let mut last_live_session_at = None;
    let mut next_poll = Instant::now();
    let mut metadata_jobs = tokio::task::JoinSet::new();
    // ID-only beacons need an HTTP lookup; full beacons must never wait for it.
    let mut metadata_pending = std::collections::VecDeque::new();
    let mut metadata_inflight = HashSet::new();

    loop {
        let live = has_live_session(&session_rx.borrow());
        if live {
            last_live_session_at = Some(Instant::now());
        }
        let fallback_allowed = live || grace_remaining_since(last_live_session_at).is_some();
        let push_active = heartbeat_is_fresh(heartbeat.as_ref());
        let wait = next_poll.saturating_duration_since(Instant::now());
        let client = anki_client.read().await.clone();
        let anki_config = config.read().await.anki.clone();

        while metadata_jobs.len() < 4 {
            let Some((note_id, card_ids)) = metadata_pending.pop_front() else {
                break;
            };
            let client = client.clone();
            metadata_jobs
                .spawn(async move { (note_id, card_ids, client.notes_info(&[note_id]).await) });
        }

        tokio::select! {
            biased;
            payload = event_rx.recv() => {
                let Some(payload) = payload else { return; };
                if payload.event == AnkiBeaconEventKind::NoteAdded && payload.note_info().is_none() {
                    if let Some(note_id) = payload.note_id {
                        if !processed_push_notes.contains(&note_id) && metadata_inflight.insert(note_id) {
                            if metadata_pending.len() < 128 {
                                metadata_pending.push_back((note_id, payload.provided_card_ids()));
                            } else {
                                metadata_inflight.remove(&note_id);
                                warn!(note_id, "Anki metadata queue is full; polling will retry");
                            }
                        }
                    }
                } else if !handle_ankibeacon_event(
                    payload, &anki_client, &config, &status, &tx, &mut heartbeat,
                    &mut processed_push_notes, &mut consecutive_errors, &mut final_warning_shown,
                ).await { return; }
                // Share deduplication with fallback, including when push starts
                // halfway through a poll. A fresh heartbeat cancels that poll.
                known_ids.extend(processed_push_notes.iter().copied());
            }
            Some(result) = metadata_jobs.join_next(), if !metadata_jobs.is_empty() => {
                if let Ok((note_id, card_ids, result)) = result {
                    metadata_inflight.remove(&note_id);
                    match result {
                        Ok(notes) => {
                            for note in notes {
                                if processed_push_notes.insert(note.note_id) {
                                    known_ids.insert(note.note_id);
                                    if !emit_note_info(note, &anki_config, &tx, card_ids.clone()).await { return; }
                                }
                            }
                        }
                        Err(error) => warn!(note_id, "Anki note metadata lookup failed: {}", error),
                    }
                }
            }
            step = async {
                tokio::time::sleep(wait).await;
                poll_ankiconnect_once(client, anki_config.clone(), &status, &tx, &mut known_ids,
                    &mut initialized, &mut consecutive_errors, &mut final_warning_shown).await
            }, if fallback_allowed && !push_active => {
                match step {
                    PollStep::Continue(delay) => {
                        processed_push_notes.extend(known_ids.iter().copied());
                        next_poll = Instant::now() + delay;
                    }
                    PollStep::Stop => return,
                }
            }
            // Re-evaluate idle/heartbeat state without cancelling in-flight
            // HTTP polls for every playback position update.
            _ = tokio::time::sleep(Duration::from_secs(1)), if !fallback_allowed || push_active => {
                if !fallback_allowed && !push_active { set_idle_anki_status(&status).await; }
            }
        }
    }
}

#[cfg(test)]
mod note_filter_tests {
    use super::{NoteField, NoteInfo, should_skip_note};
    use crate::config::AnkiConfig;
    use std::collections::HashMap;

    fn note_with_sentence(sentence: Option<&str>) -> NoteInfo {
        let cfg = AnkiConfig::default();
        let mut fields = HashMap::new();
        if let Some(sentence) = sentence {
            fields.insert(
                cfg.fields.sentence,
                NoteField {
                    value: sentence.to_string(),
                    order: 0,
                },
            );
        }
        NoteInfo {
            note_id: 1,
            model_name: "Mining".to_string(),
            tags: Vec::new(),
            fields,
        }
    }

    #[test]
    fn notes_with_blank_or_missing_sentence_fields_are_ignored() {
        let cfg = AnkiConfig::default();

        assert!(should_skip_note(&note_with_sentence(None), &cfg));
        assert!(should_skip_note(&note_with_sentence(Some("   ")), &cfg));
    }
}

#[cfg(test)]
mod notification_latency_tests {
    use super::*;
    use crate::config::MediaServerKind;
    use crate::session::SessionSummary;
    use axum::{Json, Router, routing::post};

    #[tokio::test]
    async fn full_push_bypasses_slow_poll_and_id_only_lookup() {
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/",
                    post(move || {
                        let signal = signal.clone();
                        async move {
                            signal.notify_one();
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            Json(json!({"result": [], "error": null}))
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let config = Config::default();
        let sentence_field = config.anki.fields.sentence.clone();
        let (event_tx, event_rx) = mpsc::channel(16);
        let (card_tx, mut card_rx) = mpsc::channel(16);
        let (_session_tx, session_rx) = watch::channel(SessionState {
            sessions: vec![SessionSummary {
                id: "test".into(),
                server_kind: MediaServerKind::Plex,
                client: "test".into(),
                device_name: "test".into(),
                user_name: None,
                title: None,
                is_target_language: true,
            }],
            now_playing: None,
            active_session_id: Some("test".into()),
        });
        let poller = tokio::spawn(run_anki_poller(
            Arc::new(RwLock::new(Arc::new(AnkiClient::new(&format!(
                "http://{addr}"
            ))))),
            Arc::new(RwLock::new(config)),
            Arc::new(RwLock::new(AnkiStatus::default())),
            card_tx,
            event_rx,
            session_rx,
        ));
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        let full = |id| {
            serde_json::from_value::<AnkiBeaconEvent>(json!({
                "event": "note_added", "note_id": id, "note_type_name": "Mining",
                "fields": { sentence_field.clone(): "対象の文" }, "card_ids": [123],
            }))
            .unwrap()
        };
        let start = Instant::now();
        event_tx.send(full(1)).await.unwrap();
        let first = tokio::time::timeout(Duration::from_millis(500), card_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.event.note_id, 1);
        assert_eq!(first.card_ids, Some(vec![123]));
        eprintln!("Full push during a 10-second poll: {:?}", start.elapsed());

        event_tx
            .send(serde_json::from_value(json!({"event":"heartbeat"})).unwrap())
            .await
            .unwrap();
        event_tx
            .send(serde_json::from_value(json!({"event":"note_added","note_id":2})).unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        event_tx.send(full(3)).await.unwrap();
        let next = tokio::time::timeout(Duration::from_millis(500), card_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.event.note_id, 3);
        event_tx.send(full(3)).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), card_rx.recv())
                .await
                .is_err()
        );
        poller.abort();
        server.abort();
    }

    #[tokio::test]
    async fn cards_with_existing_media_are_still_captured_for_review() {
        let cfg = AnkiConfig::default();
        let mut note = NoteInfo {
            note_id: 9,
            model_name: "Mining".into(),
            tags: vec![],
            fields: HashMap::from([(
                cfg.fields.sentence.clone(),
                NoteField {
                    value: "対象の文".into(),
                    order: 0,
                },
            )]),
        };
        note.fields.insert(
            cfg.fields.sentence_audio.clone(),
            NoteField {
                value: "[sound:existing.mp3]".into(),
                order: 1,
            },
        );
        note.fields.insert(
            cfg.fields.picture.clone(),
            NoteField {
                value: "<img src='existing.jpg'>".into(),
                order: 2,
            },
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        assert!(emit_note_info(note, &cfg, &tx, None).await);
        let notification = rx.recv().await.unwrap();
        assert!(!notification.needs_enhancement);
        assert_eq!(notification.event.sentence, "対象の文");
    }
}
