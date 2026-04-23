use crate::anki::{AnkiClient, AnkiStatus, NewCardEvent};
use crate::config::{Config, MediaServerKind};
use crate::media;
use crate::media_server::{MediaServer, ServerMap};
use crate::mining::{
    AppDatabase, EnrichmentDialogState, EnrichmentSource, MiningHistoryEntry, MiningHistorySummary,
};
use crate::session::{
    HistoryEntry, SessionManager, SessionState, SubtitleCandidate, SubtitleSelectionMode,
    scoped_history_id, split_scoped_id,
};
use crate::subtitle::{SubtitleTrack, find_all_matching_lines, find_matching_line};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, broadcast, mpsc, watch};
use tracing::{error, info, warn};

#[allow(unused_imports)]
use crate::anki::AnkiMedia;

/// Shared application state passed to handlers.
pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub db: Arc<AppDatabase>,
    pub session_manager: Arc<SessionManager>,
    pub servers: Arc<RwLock<ServerMap>>,
    pub anki_client: Arc<RwLock<Arc<AnkiClient>>>,
    pub anki_status: Arc<RwLock<AnkiStatus>>,
    pub enhancement_queue: Arc<RwLock<Vec<EnhancementQueueItem>>>,
    pub enhancement_tx: mpsc::Sender<EnhancementJob>,
    pub session_rx: watch::Receiver<SessionState>,
    pub new_card_tx: broadcast::Sender<EnrichmentDialogState>,
    pub subtitles: Arc<RwLock<Option<SubtitleTrack>>>,
    pub subtitle_candidates: Arc<RwLock<Vec<SubtitleCandidate>>>,
    pub subtitle_history: Arc<RwLock<HashMap<String, SubtitleTrack>>>,
    pub history: Arc<RwLock<HashMap<String, HistoryEntry>>>,
    /// Queue of pending enrichment requests from the frontend.
    pub pending_enrichments: Arc<RwLock<Vec<EnrichmentDialogState>>>,
    /// Broadcast channel for enhancement job results (success/failure).
    pub enhancement_result_tx: broadcast::Sender<EnhancementResult>,
    /// Broadcast channel for remote control results (seek/play/pause).
    pub remote_result_tx: broadcast::Sender<RemoteControlResult>,
    /// Audio tracks for the current media item.
    pub audio_tracks: Arc<RwLock<Vec<crate::session::AudioTrack>>>,
    /// The currently selected audio stream index (absolute).
    pub selected_audio_track: Arc<RwLock<Option<u32>>>,
    /// How the audio track was resolved.
    pub audio_track_resolution: Arc<RwLock<crate::session::AudioTrackResolution>>,
}

pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/sessions", get(get_sessions))
        .route("/api/sessions/select", post(select_session))
        .route("/api/subtitles", get(get_subtitles))
        .route("/api/subtitles/select", post(select_subtitle_track))
        .route("/api/history", get(get_history))
        .route(
            "/api/history/{item_id}/subtitles",
            get(get_history_subtitles),
        )
        .route(
            "/api/history/{item_id}/activate",
            post(activate_history_item),
        )
        .route("/api/mined", get(get_mined_history))
        .route("/api/dialog/note/{note_id}", get(get_dialog_by_note_id))
        .route("/api/dialog/card/{card_id}", get(get_dialog_by_card_id))
        .route("/api/enrich", post(enrich_card))
        .route("/api/enrich/skip", post(skip_enrichment))
        .route("/api/enrich/pending", get(get_pending_enrichments))
        .route("/api/config", get(get_config))
        .route("/api/config", put(update_config))
        .route("/api/seek", post(seek_to_line))
        .route("/api/play-pause", post(play_pause))
        .route("/api/subtitle/matches", post(subtitle_matches))
        .route("/api/preview-audio", post(preview_audio_url))
        .route("/api/preview-screenshot", post(preview_screenshot))
        .route("/api/audio-tracks", get(get_audio_tracks))
        .route("/api/audio-tracks/select", post(select_audio_track))
        .route("/api/audio-tracks/preview", post(preview_audio_track))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

// === REST Handlers ===

async fn get_state(State(state): State<Arc<AppState>>) -> Json<SessionState> {
    let session_state = state.session_rx.borrow().clone();
    Json(session_state)
}

async fn get_sessions(State(state): State<Arc<AppState>>) -> Json<SessionState> {
    let session_state = state.session_rx.borrow().clone();
    Json(session_state)
}

#[derive(Deserialize)]
struct SelectSession {
    session_id: Option<String>,
}

async fn select_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectSession>,
) -> Json<serde_json::Value> {
    state.session_manager.select_session(body.session_id).await;
    Json(serde_json::json!({"ok": true}))
}

async fn active_subtitle_data(state: &Arc<AppState>) -> SubtitleData {
    let track = state.subtitles.read().await.clone();
    let candidates = state.subtitle_candidates.read().await.clone();
    let session_state = state.session_rx.borrow().clone();
    let selected_candidate_id = session_state
        .now_playing
        .as_ref()
        .and_then(|now_playing| now_playing.subtitle_candidate_id.clone());
    let selection_mode = session_state
        .now_playing
        .as_ref()
        .map(|now_playing| now_playing.subtitle_selection_mode)
        .unwrap_or(SubtitleSelectionMode::Auto);
    let lines = track.map(|track| track.lines).unwrap_or_default();
    let count = lines.len();

    SubtitleData {
        lines,
        count,
        candidates,
        selected_candidate_id,
        selection_mode,
    }
}

fn history_subtitle_data(track: Option<SubtitleTrack>) -> SubtitleData {
    let lines = track.map(|track| track.lines).unwrap_or_default();
    let count = lines.len();

    SubtitleData {
        lines,
        count,
        candidates: Vec::new(),
        selected_candidate_id: None,
        selection_mode: SubtitleSelectionMode::Auto,
    }
}

async fn get_subtitles(State(state): State<Arc<AppState>>) -> Json<SubtitleData> {
    Json(active_subtitle_data(&state).await)
}

#[derive(Deserialize)]
struct SelectSubtitleTrack {
    candidate_id: Option<String>,
}

async fn select_subtitle_track(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectSubtitleTrack>,
) -> Json<serde_json::Value> {
    match state
        .session_manager
        .select_subtitle_candidate(body.candidate_id)
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "subtitles": active_subtitle_data(&state).await,
        })),
        Err(error) => Json(serde_json::json!({
            "ok": false,
            "error": error.to_string(),
        })),
    }
}

async fn get_pending_enrichments(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<EnrichmentDialogState>> {
    let pending = state.pending_enrichments.read().await;
    Json(pending.clone())
}

// === History endpoints ===

async fn get_history(State(state): State<Arc<AppState>>) -> Json<Vec<HistoryEntry>> {
    let hist = state.history.read().await;
    let mut entries: Vec<HistoryEntry> = hist.values().cloned().collect();
    entries.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    Json(entries)
}

async fn get_history_subtitles(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(history_id): axum::extract::Path<String>,
) -> Json<SubtitleData> {
    let sh = state.subtitle_history.read().await;
    Json(history_subtitle_data(sh.get(&history_id).cloned()))
}

/// Load a history item as the active item for mining (load its subtitles into the main view).
async fn activate_history_item(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(history_id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let hist = state.history.read().await;
    let entry = match hist.get(&history_id) {
        Some(e) => e.clone(),
        None => return Json(serde_json::json!({"ok": false, "error": "Item not in history"})),
    };
    drop(hist);

    // Load the history item's subtitles into the active subtitle track
    let sh = state.subtitle_history.read().await;
    if let Some(track) = sh.get(&history_id) {
        let mut subs = state.subtitles.write().await;
        *subs = Some(track.clone());
    }
    drop(sh);

    info!("Activated history item: {} ({})", entry.title, history_id);
    Json(serde_json::json!({"ok": true, "title": entry.title}))
}

async fn get_mined_history(State(state): State<Arc<AppState>>) -> Json<Vec<MiningHistorySummary>> {
    match state.db.list_mined_notes().await {
        Ok(entries) => Json(entries),
        Err(error) => {
            error!("Failed to load mining history: {}", error);
            Json(Vec::new())
        }
    }
}

async fn get_dialog_by_note_id(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(note_id): axum::extract::Path<i64>,
) -> Json<serde_json::Value> {
    match lookup_dialog_state(&state, DialogLookup::NoteId(note_id)).await {
        Ok(Some(dialog)) => Json(serde_json::json!({ "ok": true, "dialog": dialog })),
        Ok(None) => Json(serde_json::json!({
            "ok": false,
            "error": format!("No pending or mined note found for note {}", note_id),
        })),
        Err(error) => Json(serde_json::json!({ "ok": false, "error": error })),
    }
}

async fn get_dialog_by_card_id(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(card_id): axum::extract::Path<i64>,
) -> Json<serde_json::Value> {
    match lookup_dialog_state(&state, DialogLookup::CardId(card_id)).await {
        Ok(Some(dialog)) => Json(serde_json::json!({ "ok": true, "dialog": dialog })),
        Ok(None) => Json(serde_json::json!({
            "ok": false,
            "error": format!("No pending or mined note found for card {}", card_id),
        })),
        Err(error) => Json(serde_json::json!({ "ok": false, "error": error })),
    }
}

#[derive(Deserialize, Clone)]
pub(crate) struct EnrichRequest {
    note_id: i64,
    sentence: Option<String>,
    start_ms: i64,
    end_ms: i64,
    generate_avif: bool,
    matched_line_index: Option<usize>,
    included_line_first: Option<usize>,
    included_line_last: Option<usize>,
    /// Optional: enrich from a history item instead of the live session.
    /// Despite the legacy field name, this is a scoped history id.
    item_id: Option<String>,
}

#[derive(Serialize)]
struct EnrichResponse {
    success: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EnhancementQueueState {
    Queued,
    Running,
}

#[derive(Clone, Serialize, PartialEq, Eq)]
pub(crate) struct EnhancementQueueItem {
    note_id: i64,
    state: EnhancementQueueState,
    message: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct EnhancementResult {
    pub note_id: i64,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct RemoteControlResult {
    pub action: String,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
struct MediaContext {
    server_kind: MediaServerKind,
    item_id: String,
    media_source_id: String,
    file_path: Option<String>,
    title: String,
}

fn history_entry_to_media_context(entry: &HistoryEntry) -> MediaContext {
    MediaContext {
        server_kind: entry.server_kind,
        item_id: entry.item_id.clone(),
        media_source_id: entry.media_source_id.clone(),
        file_path: entry.file_path.clone(),
        title: entry.title.clone(),
    }
}

async fn get_server(state: &Arc<AppState>, kind: MediaServerKind) -> Option<Arc<dyn MediaServer>> {
    state.servers.read().await.get(&kind).cloned()
}

async fn resolve_media_context(
    state: &Arc<AppState>,
    requested_history_id: Option<&str>,
) -> Result<MediaContext, String> {
    if let Some(history_id) = requested_history_id {
        info!("[media_ctx] Looking up history_id={}", history_id);
        let hist = state.history.read().await;
        let result = hist
            .get(history_id)
            .map(history_entry_to_media_context)
            .ok_or_else(|| format!("Item not found in history: {}", history_id));
        if result.is_err() {
            warn!(
                "[media_ctx] history_id={} NOT FOUND (history has {} entries)",
                history_id,
                hist.len()
            );
        }
        return result;
    }

    info!("[media_ctx] No history_id, checking now_playing...");
    let session_state = state.session_rx.borrow().clone();
    if let Some(np) = session_state.now_playing {
        info!(
            "[media_ctx] Using now_playing: {} ({})",
            np.title, np.item_id
        );
        return Ok(MediaContext {
            server_kind: np.server_kind,
            item_id: np.item_id,
            media_source_id: np.media_source_id,
            file_path: np.file_path,
            title: np.title,
        });
    }

    info!("[media_ctx] No now_playing, falling back to most recent history...");
    let hist = state.history.read().await;
    hist.values()
        .max_by_key(|entry| entry.last_seen)
        .map(|entry| {
            info!(
                "No active playback session; falling back to most recent history item: {} ({})",
                entry.title, entry.history_id
            );
            history_entry_to_media_context(entry)
        })
        .ok_or_else(|| "No active playback session or history item available".to_string())
}

#[derive(Clone, Copy)]
enum DialogLookup {
    NoteId(i64),
    CardId(i64),
}

fn build_note_event(note: crate::anki::NoteInfo, sentence_field: &str) -> NewCardEvent {
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

async fn fetch_note_event(
    anki_client: Arc<AnkiClient>,
    config: &Config,
    note_id: i64,
) -> Option<NewCardEvent> {
    let notes = anki_client.notes_info(&[note_id]).await.ok()?;
    notes
        .into_iter()
        .next()
        .map(|note| build_note_event(note, &config.anki.fields.sentence))
}

async fn lookup_dialog_state(
    state: &Arc<AppState>,
    lookup: DialogLookup,
) -> Result<Option<EnrichmentDialogState>, String> {
    {
        let pending = state.pending_enrichments.read().await;
        let candidate = match lookup {
            DialogLookup::NoteId(note_id) => {
                pending.iter().find(|item| item.event.note_id == note_id)
            }
            DialogLookup::CardId(card_id) => pending
                .iter()
                .find(|item| item.card_ids.iter().any(|value| *value == card_id)),
        };
        if let Some(candidate) = candidate {
            return Ok(Some(candidate.clone()));
        }
    }

    let mining = match lookup {
        DialogLookup::NoteId(note_id) => state.db.get_mined_note_by_note_id(note_id).await,
        DialogLookup::CardId(card_id) => state.db.get_mined_note_by_card_id(card_id).await,
    }
    .map_err(|error| error.to_string())?;

    Ok(mining.map(|entry| entry.dialog_state()))
}

async fn remove_pending_enrichment(state: &Arc<AppState>, note_id: i64) {
    let mut pending = state.pending_enrichments.write().await;
    pending.retain(|entry| entry.event.note_id != note_id);
}

async fn resolve_audio_mapping(
    state: &Arc<AppState>,
    stream_index: Option<u32>,
) -> (Option<u32>, Option<usize>) {
    let audio_ordinal = if let Some(index) = stream_index {
        let tracks = state.audio_tracks.read().await;
        tracks.iter().position(|track| track.index == index)
    } else {
        None
    };

    (stream_index, audio_ordinal)
}

async fn prepare_enrichment_candidate(
    state: &Arc<AppState>,
    event: NewCardEvent,
) -> Option<EnrichmentDialogState> {
    let subtitles = state.subtitles.read().await;
    let track = subtitles.as_ref()?;

    let session_state = state.session_rx.borrow().clone();
    let position_ms = session_state
        .now_playing
        .as_ref()
        .map(|now_playing| now_playing.position_ms)
        .unwrap_or(0);
    let history_id = session_state
        .now_playing
        .as_ref()
        .map(|now_playing| now_playing.history_id.clone());

    // Try time-windowed match first (±30s around playback position)
    let mut matched_line_index = find_matching_line(track, &event.sentence, position_ms, 30_000);

    // Fall back to a global match across the entire subtitle track.
    // Cards should be accepted whenever we have subs loaded and a match exists,
    // regardless of playback state or reported position.
    if matched_line_index.is_none() {
        let global_matches = find_all_matching_lines(track, &event.sentence);
        if let Some(&(best_idx, score)) = global_matches.first() {
            if score > 0.6 {
                info!(
                    "No time-windowed match for note {} at position {}ms; \
                     global fallback matched line {} (score {:.2})",
                    event.note_id, position_ms, best_idx, score
                );
                matched_line_index = Some(best_idx);
            }
        }
    }

    let matched_line_index = matched_line_index?;
    drop(subtitles);

    let config = state.config.read().await.clone();
    let anki_client = state.anki_client.read().await.clone();
    let card_ids = anki_client
        .find_cards_for_note(event.note_id)
        .await
        .unwrap_or_else(|error| {
            warn!(
                "Failed to look up card ids for note {}: {}",
                event.note_id, error
            );
            Vec::new()
        });

    Some(EnrichmentDialogState {
        event,
        matched_line_index: Some(matched_line_index),
        history_id,
        start_ms: None,
        end_ms: None,
        generate_avif: Some(config.mining.generate_avif),
        included_line_first: None,
        included_line_last: None,
        card_ids,
        source: EnrichmentSource::Pending,
        updated_at: None,
    })
}

async fn queue_pending_enrichment(state: &Arc<AppState>, candidate: EnrichmentDialogState) {
    {
        let mut pending = state.pending_enrichments.write().await;
        if let Some(existing) = pending
            .iter_mut()
            .find(|entry| entry.event.note_id == candidate.event.note_id)
        {
            *existing = candidate.clone();
        } else {
            pending.push(candidate.clone());
        }
    }
    let _ = state.new_card_tx.send(candidate);
}

fn enhancement_queue_message(note_id: i64, queue_state: EnhancementQueueState) -> String {
    match queue_state {
        EnhancementQueueState::Queued => format!("Queued note #{}", note_id),
        EnhancementQueueState::Running => format!("Enhancing note #{}...", note_id),
    }
}

async fn update_enhancement_queue_item(
    state: &Arc<AppState>,
    note_id: i64,
    queue_state: EnhancementQueueState,
) {
    let mut queue = state.enhancement_queue.write().await;
    let message = enhancement_queue_message(note_id, queue_state);

    if let Some(item) = queue.iter_mut().find(|item| item.note_id == note_id) {
        item.state = queue_state;
        item.message = message;
        return;
    }

    queue.push(EnhancementQueueItem {
        note_id,
        state: queue_state,
        message,
    });
}

async fn remove_enhancement_queue_item(state: &Arc<AppState>, note_id: i64) {
    let mut queue = state.enhancement_queue.write().await;
    queue.retain(|item| item.note_id != note_id);
}

async fn enqueue_enhancement_job(
    state: &Arc<AppState>,
    req: EnrichRequest,
    fallback: Option<EnrichmentDialogState>,
) -> Result<(), String> {
    let note_id = req.note_id;
    info!(
        "[enqueue] note {} — item_id={:?}, start={}ms, end={}ms, avif={}",
        note_id, req.item_id, req.start_ms, req.end_ms, req.generate_avif
    );

    {
        let mut queue = state.enhancement_queue.write().await;
        if queue.iter().any(|item| item.note_id == note_id) {
            warn!("[enqueue] note {} — REJECTED, already in queue", note_id);
            return Err(format!(
                "Note #{} is already queued for enhancement",
                note_id
            ));
        }

        queue.push(EnhancementQueueItem {
            note_id,
            state: EnhancementQueueState::Queued,
            message: enhancement_queue_message(note_id, EnhancementQueueState::Queued),
        });
        info!(
            "[enqueue] note {} — added to queue (queue size: {})",
            note_id,
            queue.len()
        );
    }

    state
        .enhancement_tx
        .send(EnhancementJob { req, fallback })
        .await
        .map_err(|_| "Enhancement worker is not running".to_string())?;

    Ok(())
}

pub struct EnhancementJob {
    pub req: EnrichRequest,
    pub fallback: Option<EnrichmentDialogState>,
}

const ENHANCEMENT_TIMEOUT: Duration = Duration::from_secs(120);
const ENHANCEMENT_WORKERS: usize = 3;

/// Spawns a pool of workers that process enhancement jobs concurrently.
/// Each worker pulls from the shared channel independently.
pub async fn run_enhancement_worker_pool(state: Arc<AppState>, rx: mpsc::Receiver<EnhancementJob>) {
    let shared_rx = Arc::new(tokio::sync::Mutex::new(rx));
    info!(
        "[pool] Starting {} enhancement workers",
        ENHANCEMENT_WORKERS
    );

    let mut handles = Vec::new();
    for worker_id in 0..ENHANCEMENT_WORKERS {
        let state = state.clone();
        let rx = shared_rx.clone();
        handles.push(tokio::spawn(async move {
            enhancement_worker_loop(worker_id, state, rx).await;
        }));
    }

    // If any worker exits, they all should (channel closed)
    for handle in handles {
        let _ = handle.await;
    }
    warn!("[pool] All enhancement workers stopped");
}

async fn enhancement_worker_loop(
    worker_id: usize,
    state: Arc<AppState>,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<EnhancementJob>>>,
) {
    info!("[worker-{}] Started", worker_id);
    loop {
        // Hold the Mutex only long enough to pull the next job
        let job = {
            let mut rx = rx.lock().await;
            rx.recv().await
        };
        let Some(job) = job else {
            info!("[worker-{}] Channel closed, shutting down", worker_id);
            break;
        };

        let note_id = job.req.note_id;
        info!("[worker-{}] Processing note {}", worker_id, note_id);
        update_enhancement_queue_item(&state, note_id, EnhancementQueueState::Running).await;
        info!("Starting enhancement for note {}", note_id);

        let fallback_event = job.fallback.as_ref().map(|entry| entry.event.clone());
        let fallback_card_ids = job
            .fallback
            .as_ref()
            .map(|entry| entry.card_ids.clone())
            .unwrap_or_default();

        let result = AssertUnwindSafe(tokio::time::timeout(
            ENHANCEMENT_TIMEOUT,
            perform_enrichment(&state, &job.req, fallback_event, fallback_card_ids),
        ))
        .catch_unwind()
        .await;

        remove_enhancement_queue_item(&state, note_id).await;

        let enhancement_result = match result {
            Ok(Ok(Ok(()))) => {
                info!("Successfully enriched note {}", note_id);
                EnhancementResult {
                    note_id,
                    success: true,
                    message: format!("Note #{} enhanced successfully", note_id),
                }
            }
            Ok(Ok(Err(error))) => {
                error!("Enrichment failed for note {}: {}", note_id, error);
                if let Some(candidate) = job.fallback {
                    queue_pending_enrichment(&state, candidate).await;
                }
                EnhancementResult {
                    note_id,
                    success: false,
                    message: format!("Enhancement failed for note #{}: {}", note_id, error),
                }
            }
            Ok(Err(_)) => {
                error!(
                    "Enrichment timed out for note {} after {}s",
                    note_id,
                    ENHANCEMENT_TIMEOUT.as_secs()
                );
                if let Some(candidate) = job.fallback {
                    queue_pending_enrichment(&state, candidate).await;
                }
                EnhancementResult {
                    note_id,
                    success: false,
                    message: format!("Enhancement timed out for note #{}", note_id),
                }
            }
            Err(_) => {
                error!("Enhancement task panicked for note {}", note_id);
                if let Some(candidate) = job.fallback {
                    queue_pending_enrichment(&state, candidate).await;
                }
                EnhancementResult {
                    note_id,
                    success: false,
                    message: format!("Enhancement panicked for note #{}", note_id),
                }
            }
        };

        let _ = state.enhancement_result_tx.send(enhancement_result);
        info!(
            "[worker-{}] Finished note {}, ready for next job",
            worker_id, note_id
        );
    }
}

async fn save_mining_history_entry(
    state: &Arc<AppState>,
    req: &EnrichRequest,
    media_ctx: &MediaContext,
    fallback_event: Option<NewCardEvent>,
    fallback_card_ids: Vec<i64>,
) {
    let config = state.config.read().await.clone();
    let anki_client = state.anki_client.read().await.clone();
    let event = match fetch_note_event(anki_client.clone(), &config, req.note_id).await {
        Some(event) => event,
        None => match fallback_event {
            Some(event) => event,
            None => {
                warn!(
                    "Skipping mining history save for note {} because no note snapshot was available",
                    req.note_id
                );
                return;
            }
        },
    };

    let card_ids = match anki_client.find_cards_for_note(req.note_id).await {
        Ok(ids) if !ids.is_empty() => ids,
        Ok(_) => fallback_card_ids,
        Err(error) => {
            warn!(
                "Failed to refresh card ids for note {}: {}",
                req.note_id, error
            );
            fallback_card_ids
        }
    };

    let now = Utc::now();
    let created_at = state
        .db
        .get_mined_note_by_note_id(req.note_id)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.created_at)
        .unwrap_or(now);

    let entry = MiningHistoryEntry {
        note_id: req.note_id,
        card_ids,
        history_id: req
            .item_id
            .clone()
            .unwrap_or_else(|| scoped_history_id(media_ctx.server_kind, &media_ctx.item_id)),
        server_kind: media_ctx.server_kind,
        item_id: media_ctx.item_id.clone(),
        media_source_id: media_ctx.media_source_id.clone(),
        file_path: media_ctx.file_path.clone(),
        title: media_ctx.title.clone(),
        event,
        start_ms: req.start_ms,
        end_ms: req.end_ms,
        generate_avif: req.generate_avif,
        matched_line_index: req.matched_line_index,
        included_line_first: req.included_line_first,
        included_line_last: req.included_line_last,
        created_at,
        updated_at: now,
    };

    if let Err(error) = state.db.upsert_mined_note(entry).await {
        warn!(
            "Failed to persist mining history for note {}: {}",
            req.note_id, error
        );
    }
}

async fn generate_and_store_screenshot(
    anki_client: &Arc<AnkiClient>,
    source: &str,
    item_id: &str,
    time_ms: i64,
    note_id: i64,
) -> Option<String> {
    info!(
        "[enhance {}] Capturing screenshot fallback at {}ms...",
        note_id, time_ms
    );
    match media::generate_screenshot(source, time_ms).await {
        Ok((ss_path, ss_data)) => {
            let ss_filename = format!("nagare_{}_{}_ss.webp", item_id, time_ms);
            let ss_b64 = media::to_base64(&ss_data);
            let picture_html = match anki_client.store_media_file(&ss_filename, &ss_b64).await {
                Ok(_) => {
                    info!("[enhance {}] Screenshot stored in Anki", note_id);
                    Some(format!("<img src=\"{}\">", ss_filename))
                }
                Err(error) => {
                    warn!(
                        "[enhance {}] Failed to store screenshot in Anki: {}",
                        note_id, error
                    );
                    None
                }
            };
            media::cleanup_temp_file(&ss_path).await;
            picture_html
        }
        Err(error) => {
            warn!(
                "[enhance {}] Screenshot generation failed (continuing without): {}",
                note_id, error
            );
            None
        }
    }
}

async fn perform_enrichment(
    state: &Arc<AppState>,
    req: &EnrichRequest,
    fallback_event: Option<NewCardEvent>,
    fallback_card_ids: Vec<i64>,
) -> Result<(), String> {
    let note_id = req.note_id;
    let result = async {
        info!(
            "[enhance {}] Resolving media context (item_id={:?})...",
            note_id, req.item_id
        );
        let media_ctx = resolve_media_context(state, req.item_id.as_deref()).await?;
        info!(
            "[enhance {}] Media context resolved: {} (server={:?}, item={}, source={})",
            note_id,
            media_ctx.title,
            media_ctx.server_kind,
            media_ctx.item_id,
            media_ctx.media_source_id
        );

        info!("[enhance {}] Reading config...", note_id);
        let config = state.config.read().await.clone();
        info!("[enhance {}] Getting server...", note_id);
        let server_opt = get_server(state, media_ctx.server_kind).await;
        info!("[enhance {}] Reading anki client...", note_id);
        let anki_client = state.anki_client.read().await.clone();

        info!("[enhance {}] Resolving media source...", note_id);
        let source = media::resolve_media_source(
            &config,
            server_opt.as_deref(),
            &media_ctx.item_id,
            &media_ctx.media_source_id,
            media_ctx.file_path.as_deref(),
        )
        .map_err(|error| format!("Failed to resolve media source: {}", error))?;
        info!("[enhance {}] Media source resolved", note_id);

        info!(
            "[enhance {}] Extracting audio ({}ms - {}ms)...",
            note_id, req.start_ms, req.end_ms
        );
        let selected_audio_track = *state.selected_audio_track.read().await;
        let (audio_track_index, audio_track_ordinal) =
            resolve_audio_mapping(&state, selected_audio_track).await;
        let (audio_path, audio_data) = media::extract_audio(
            &source,
            req.start_ms,
            req.end_ms,
            audio_track_index,
            audio_track_ordinal,
        )
        .await
        .map_err(|error| format!("Audio extraction failed: {}", error))?;
        let audio_filename = format!("nagare_{}_{}.opus", media_ctx.item_id, req.start_ms);
        let audio_b64 = media::to_base64(&audio_data);
        info!(
            "[enhance {}] Audio extracted ({} bytes), storing in Anki as {}...",
            note_id,
            audio_data.len(),
            audio_filename
        );

        if let Err(error) = anki_client
            .store_media_file(&audio_filename, &audio_b64)
            .await
        {
            media::cleanup_temp_file(&audio_path).await;
            return Err(format!("Failed to store audio: {}", error));
        }
        info!("[enhance {}] Audio stored in Anki", note_id);

        let mut picture_html = String::new();
        let mid_ms = (req.start_ms + req.end_ms) / 2;
        if req.generate_avif {
            info!(
                "[enhance {}] Generating AVIF ({}ms - {}ms)...",
                note_id, req.start_ms, req.end_ms
            );
            match media::generate_avif(&source, req.start_ms, req.end_ms).await {
                Ok((avif_path, avif_data)) => {
                    let avif_filename =
                        format!("nagare_{}_{}.avif", media_ctx.item_id, req.start_ms);
                    let avif_b64 = media::to_base64(&avif_data);
                    info!(
                        "[enhance {}] AVIF generated ({} bytes), storing as {}...",
                        note_id,
                        avif_data.len(),
                        avif_filename
                    );
                    if let Err(error) = anki_client
                        .store_media_file(&avif_filename, &avif_b64)
                        .await
                    {
                        warn!("Failed to store AVIF in Anki: {}", error);
                    } else {
                        picture_html = format!("<img src=\"{}\">", avif_filename);
                        info!("[enhance {}] AVIF stored in Anki", note_id);
                    }
                    media::cleanup_temp_file(&avif_path).await;
                }
                Err(error) => {
                    warn!(
                        "[enhance {}] AVIF generation failed (continuing without): {}",
                        note_id, error
                    );
                }
            }
        } else {
            info!("[enhance {}] AVIF generation skipped (disabled)", note_id);
        }

        if picture_html.is_empty() {
            if req.generate_avif {
                info!(
                    "[enhance {}] No AVIF stored, falling back to screenshot...",
                    note_id
                );
            }
            if let Some(screenshot_html) = generate_and_store_screenshot(
                &anki_client,
                &source,
                &media_ctx.item_id,
                mid_ms,
                note_id,
            )
            .await
            {
                picture_html = screenshot_html;
            }
        }

        let mut fields = HashMap::new();
        fields.insert(
            config.anki.fields.sentence_audio.clone(),
            format!("[sound:{}]", audio_filename),
        );
        if !picture_html.is_empty() {
            fields.insert(config.anki.fields.picture.clone(), picture_html);
        }
        if let Some(sentence) = req.sentence.clone() {
            fields.insert(config.anki.fields.sentence.clone(), sentence);
        }
        if let Some(source_field) = config.anki.fields.source_name.as_ref() {
            if !source_field.is_empty() {
                fields.insert(source_field.clone(), media_ctx.title.clone());
            }
        }

        info!(
            "[enhance {}] Updating note fields in Anki ({} fields)...",
            note_id,
            fields.len()
        );
        if let Err(error) = anki_client
            .update_note_fields(req.note_id, fields, None, None)
            .await
        {
            media::cleanup_temp_file(&audio_path).await;
            return Err(format!("Failed to update note: {}", error));
        }
        info!("[enhance {}] Note fields updated", note_id);

        if !config.anki.add_tags.is_empty() {
            let tags_str = config.anki.add_tags.join(" ");
            info!("[enhance {}] Adding tags: {}", note_id, tags_str);
            if let Err(error) = anki_client.add_tags(&[req.note_id], &tags_str).await {
                warn!("Failed to add tags to note {}: {}", req.note_id, error);
            }
        }

        media::cleanup_temp_file(&audio_path).await;
        info!("[enhance {}] Saving mining history entry...", note_id);
        save_mining_history_entry(state, req, &media_ctx, fallback_event, fallback_card_ids).await;
        info!("[enhance {}] Enhancement complete", note_id);
        Ok(())
    }
    .await;

    result
}

pub async fn run_new_card_processor(state: Arc<AppState>, mut rx: mpsc::Receiver<NewCardEvent>) {
    while let Some(event) = rx.recv().await {
        let Some(candidate) = prepare_enrichment_candidate(&state, event).await else {
            continue;
        };

        queue_pending_enrichment(&state, candidate).await;
    }
}

async fn enrich_card(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrichRequest>,
) -> Json<EnrichResponse> {
    let note_id = req.note_id;
    info!(
        "[REST] enrich_card called: note={}, item_id={:?}, start={}ms, end={}ms",
        note_id, req.item_id, req.start_ms, req.end_ms
    );
    let fallback = {
        let pending = state.pending_enrichments.read().await;
        let found = pending
            .iter()
            .find(|entry| entry.event.note_id == note_id)
            .cloned();
        info!(
            "[REST] note {} — pending fallback: {}",
            note_id,
            found.is_some()
        );
        found
    };

    match enqueue_enhancement_job(&state, req, fallback).await {
        Ok(()) => {
            remove_pending_enrichment(&state, note_id).await;
            info!("Queued enhancement for note {}", note_id);
            Json(EnrichResponse {
                success: true,
                error: None,
            })
        }
        Err(error) => {
            error!(
                "Failed to queue enhancement for note {}: {}",
                note_id, error
            );
            Json(EnrichResponse {
                success: false,
                error: Some(error),
            })
        }
    }
}

// === Subtitle match search ===

#[derive(Deserialize)]
struct SubtitleMatchRequest {
    sentence: String,
    /// If set, search history track; otherwise use active subtitle track.
    item_id: Option<String>,
}

#[derive(Serialize)]
struct SubtitleMatchResult {
    line_index: usize,
    score: f64,
    start_ms: i64,
    end_ms: i64,
    text: String,
}

async fn subtitle_matches(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SubtitleMatchRequest>,
) -> Json<Vec<SubtitleMatchResult>> {
    let track_opt: Option<SubtitleTrack> = if let Some(ref item_id) = req.item_id {
        let sh = state.subtitle_history.read().await;
        sh.get(item_id).cloned()
    } else {
        let subs = state.subtitles.read().await;
        subs.clone()
    };

    let track = match track_opt {
        Some(t) => t,
        None => return Json(Vec::new()),
    };

    let candidates = find_all_matching_lines(&track, &req.sentence);
    let results = candidates
        .into_iter()
        .filter_map(|(idx, score)| {
            track
                .lines
                .iter()
                .find(|l| l.index == idx)
                .map(|line| SubtitleMatchResult {
                    line_index: idx,
                    score,
                    start_ms: line.start_ms,
                    end_ms: line.end_ms,
                    text: line.text.clone(),
                })
        })
        .collect();

    Json(results)
}

async fn skip_enrichment(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Some(note_id) = body.get("note_id").and_then(|v| v.as_i64()) {
        remove_pending_enrichment(&state, note_id).await;
    }
    Json(serde_json::json!({"ok": true}))
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<Config> {
    let config = state.config.read().await;
    Json(config.clone())
}

async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(new_config): Json<Config>,
) -> Json<serde_json::Value> {
    // Detect if server config changed
    let (server_changed, anki_url_changed) = {
        let old = state.config.read().await;
        let server_changed = old.emby != new_config.emby
            || old.jellyfin != new_config.jellyfin
            || old.plex != new_config.plex;
        let anki_url_changed = old.anki.url != new_config.anki.url;
        (server_changed, anki_url_changed)
    };

    if let Err(e) = state.db.save_config(new_config.clone()).await {
        error!("Failed to save config: {}", e);
        return Json(serde_json::json!({"ok": false, "error": format!("Failed to save: {}", e)}));
    }

    // Update in-memory config
    {
        let mut config = state.config.write().await;
        *config = new_config.clone();
    }

    // Recreate media server client if server config changed
    if server_changed {
        let new_servers = crate::build_media_servers(&new_config);
        if new_servers.is_empty() {
            warn!("All media server configurations are disabled");
        } else {
            info!(
                "Media server configuration updated — {} service(s) enabled",
                new_servers.len()
            );
        }
        let mut servers = state.servers.write().await;
        *servers = new_servers;
    }

    // Recreate AnkiConnect client if URL changed
    if anki_url_changed {
        info!("AnkiConnect URL updated to {}", new_config.anki.url);
        let new_client = Arc::new(AnkiClient::new(&new_config.anki.url));
        let mut client = state.anki_client.write().await;
        *client = new_client;
        drop(client);

        let mut anki_status = state.anki_status.write().await;
        *anki_status = AnkiStatus::default();
    }

    info!("Configuration updated via web UI");
    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
struct SeekRequest {
    position_ms: i64,
}

fn remote_control_error(session_state: &SessionState, server_kind: MediaServerKind) -> String {
    if server_kind == MediaServerKind::Plex
        && session_state
            .now_playing
            .as_ref()
            .map(|np| !np.supports_remote_control)
            .unwrap_or(false)
    {
        "Plex Web is not exposing companion control through Plex Media Server, so pause/play/seek cannot be driven from here."
            .to_string()
    } else {
        "Remote control is unavailable for the active session".to_string()
    }
}

async fn seek_to_line(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SeekRequest>,
) -> Json<serde_json::Value> {
    let session_state = state.session_rx.borrow().clone();
    let scoped_session_id = match session_state.active_session_id.as_ref() {
        Some(id) => id.clone(),
        None => return Json(serde_json::json!({"ok": false, "error": "No active session"})),
    };
    let now_playing = match session_state.now_playing.as_ref() {
        Some(np) => np,
        None => return Json(serde_json::json!({"ok": false, "error": "No active playback item"})),
    };

    let server_kind = now_playing.server_kind;

    if !now_playing.supports_remote_control {
        return Json(serde_json::json!({
            "ok": false,
            "error": remote_control_error(&session_state, server_kind),
        }));
    }

    let (_, session_id) = match split_scoped_id(&scoped_session_id) {
        Some(parts) => parts,
        None => {
            return Json(serde_json::json!({"ok": false, "error": "Malformed session identifier"}));
        }
    };

    let Some(server) = get_server(&state, server_kind).await else {
        return Json(
            serde_json::json!({"ok": false, "error": format!("{} is not enabled", server_kind.display_name())}),
        );
    };

    let ticks = req.position_ms * 10_000;
    let paused = now_playing.is_paused;
    let session_id_owned = session_id.to_string();
    let scoped_session_id_owned = scoped_session_id.clone();
    let session_manager = state.session_manager.clone();
    let remote_tx = state.remote_result_tx.clone();
    let position_ms = req.position_ms;

    // Fire-and-forget: spawn the actual seek command so the handler returns immediately
    tokio::spawn(async move {
        if let Err(e) = server.seek_session(&session_id_owned, ticks).await {
            let synced = session_manager
                .force_refresh_after_remote_command(
                    scoped_session_id_owned.clone(),
                    Some(position_ms),
                    Some(paused),
                )
                .await;
            if synced {
                warn!(
                    "Seek returned an error for session {}, but playback reached the target state: {}",
                    session_id_owned, e
                );
            } else {
                warn!("Seek failed for session {}: {}", session_id_owned, e);
                let _ = remote_tx.send(RemoteControlResult {
                    action: "seek".to_string(),
                    success: false,
                    error: Some(e.to_string()),
                });
            }
            return;
        }
        let _ = session_manager
            .force_refresh_after_remote_command(
                scoped_session_id_owned,
                Some(position_ms),
                Some(paused),
            )
            .await;
    });

    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
struct PlayPauseRequest {
    /// If set, force this state; if omitted, toggle.
    paused: Option<bool>,
}

async fn play_pause(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayPauseRequest>,
) -> Json<serde_json::Value> {
    let session_state = state.session_rx.borrow().clone();
    let scoped_session_id = match session_state.active_session_id.as_ref() {
        Some(id) => id.clone(),
        None => return Json(serde_json::json!({"ok": false, "error": "No active session"})),
    };
    let currently_paused = session_state
        .now_playing
        .as_ref()
        .map(|np| np.is_paused)
        .unwrap_or(false);

    let should_pause = req.paused.unwrap_or(!currently_paused);
    let server_kind = session_state
        .now_playing
        .as_ref()
        .map(|np| np.server_kind)
        .unwrap_or(MediaServerKind::Emby);

    if session_state
        .now_playing
        .as_ref()
        .map(|np| !np.supports_remote_control)
        .unwrap_or(false)
    {
        return Json(serde_json::json!({
            "ok": false,
            "error": remote_control_error(&session_state, server_kind),
        }));
    }

    let (_, session_id) = match split_scoped_id(&scoped_session_id) {
        Some(parts) => parts,
        None => {
            return Json(serde_json::json!({"ok": false, "error": "Malformed session identifier"}));
        }
    };
    let Some(srv) = get_server(&state, server_kind).await else {
        return Json(
            serde_json::json!({"ok": false, "error": format!("{} is not enabled", server_kind.display_name())}),
        );
    };

    let session_id_owned = session_id.to_string();
    let scoped_session_id_owned = scoped_session_id.clone();
    let session_manager = state.session_manager.clone();
    let remote_tx = state.remote_result_tx.clone();

    // Fire-and-forget: spawn the actual play/pause command
    tokio::spawn(async move {
        let result = if should_pause {
            srv.pause_session(&session_id_owned).await
        } else {
            srv.unpause_session(&session_id_owned).await
        };

        match result {
            Ok(_) => {
                let _ = session_manager
                    .force_refresh_after_remote_command(
                        scoped_session_id_owned,
                        None,
                        Some(should_pause),
                    )
                    .await;
            }
            Err(e) => {
                let synced = session_manager
                    .force_refresh_after_remote_command(
                        scoped_session_id_owned.clone(),
                        None,
                        Some(should_pause),
                    )
                    .await;
                if synced {
                    warn!(
                        "Play/pause returned an error for session {}, but playback reached the target state: {}",
                        session_id_owned, e
                    );
                } else {
                    warn!("Play/pause failed for session {}: {}", session_id_owned, e);
                    let _ = remote_tx.send(RemoteControlResult {
                        action: "play_pause".to_string(),
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
    });

    Json(serde_json::json!({"ok": true, "paused": should_pause}))
}

#[derive(Deserialize)]
struct PreviewAudioRequest {
    start_ms: i64,
    end_ms: i64,
    item_id: Option<String>,
}

async fn preview_audio_url(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PreviewAudioRequest>,
) -> Json<serde_json::Value> {
    let media_ctx = match resolve_media_context(&state, req.item_id.as_deref()).await {
        Ok(ctx) => ctx,
        Err(error) => return Json(serde_json::json!({"error": error})),
    };
    let config = state.config.read().await.clone();
    let server_opt = get_server(&state, media_ctx.server_kind).await;

    let source = match media::resolve_media_source(
        &config,
        server_opt.as_deref(),
        &media_ctx.item_id,
        &media_ctx.media_source_id,
        media_ctx.file_path.as_deref(),
    ) {
        Ok(source) => source,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Failed to resolve media source: {}", e)
            }));
        }
    };

    let selected_audio_track = *state.selected_audio_track.read().await;
    let (audio_track_index, audio_track_ordinal) =
        resolve_audio_mapping(&state, selected_audio_track).await;
    match media::extract_audio(
        &source,
        req.start_ms,
        req.end_ms,
        audio_track_index,
        audio_track_ordinal,
    )
    .await
    {
        Ok((path, data)) => {
            let b64 = media::to_base64(&data);
            media::cleanup_temp_file(&path).await;
            Json(serde_json::json!({
                "audio_base64": b64,
                "format": "opus",
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

// === Screenshot preview ===

#[derive(Deserialize)]
struct PreviewScreenshotRequest {
    time_ms: i64,
    item_id: Option<String>,
}

async fn preview_screenshot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PreviewScreenshotRequest>,
) -> Json<serde_json::Value> {
    let media_ctx = match resolve_media_context(&state, req.item_id.as_deref()).await {
        Ok(ctx) => ctx,
        Err(error) => return Json(serde_json::json!({"error": error})),
    };
    let config = state.config.read().await.clone();
    let server_opt = get_server(&state, media_ctx.server_kind).await;

    let source = match media::resolve_media_source(
        &config,
        server_opt.as_deref(),
        &media_ctx.item_id,
        &media_ctx.media_source_id,
        media_ctx.file_path.as_deref(),
    ) {
        Ok(source) => source,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Failed to resolve media source: {}", e)
            }));
        }
    };

    match media::generate_screenshot(&source, req.time_ms).await {
        Ok((path, data)) => {
            let b64 = media::to_base64(&data);
            media::cleanup_temp_file(&path).await;
            Json(serde_json::json!({
                "image_base64": b64,
                "format": "webp",
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

// === Audio Track Management ===

async fn get_audio_tracks(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let data = audio_tracks_data(&state).await;
    Json(serde_json::json!(data))
}

#[derive(Deserialize)]
struct SelectAudioTrackRequest {
    stream_index: u32,
}

async fn select_audio_track(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SelectAudioTrackRequest>,
) -> Json<serde_json::Value> {
    state
        .session_manager
        .select_audio_track(req.stream_index)
        .await;
    let data = audio_tracks_data(&state).await;
    Json(serde_json::json!({"ok": true, "audio_tracks": data}))
}

/// Extract a short audio snippet (~5 s) from a specific track for preview purposes.
/// The snippet is taken from approximately 15 minutes (900 000 ms) into the media,
/// or from the mid-point of the duration if the media is shorter than 15 minutes.
#[derive(Deserialize)]
struct PreviewAudioTrackRequest {
    stream_index: u32,
    item_id: Option<String>,
}

async fn preview_audio_track(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PreviewAudioTrackRequest>,
) -> Json<serde_json::Value> {
    let media_ctx = match resolve_media_context(&state, req.item_id.as_deref()).await {
        Ok(ctx) => ctx,
        Err(error) => return Json(serde_json::json!({"error": error})),
    };
    let config = state.config.read().await.clone();
    let server_opt = get_server(&state, media_ctx.server_kind).await;

    let source = match media::resolve_media_source(
        &config,
        server_opt.as_deref(),
        &media_ctx.item_id,
        &media_ctx.media_source_id,
        media_ctx.file_path.as_deref(),
    ) {
        Ok(source) => source,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Failed to resolve media source: {}", e)
            }));
        }
    };

    // Pick a sample point: prefer ~15 min in, else mid-point of known duration, else 60 s.
    let session_state = state.session_rx.borrow().clone();
    let duration_ms = session_state
        .now_playing
        .as_ref()
        .and_then(|np| np.duration_ms)
        .unwrap_or(0);
    let sample_start_ms = if duration_ms > 900_000 {
        900_000i64
    } else if duration_ms > 10_000 {
        duration_ms / 2
    } else {
        0
    };
    let snippet_duration_ms = 5_000i64;
    let end_ms = (sample_start_ms + snippet_duration_ms).min(duration_ms.max(snippet_duration_ms));

    let (audio_track_index, audio_track_ordinal) =
        resolve_audio_mapping(&state, Some(req.stream_index)).await;
    match media::extract_audio(
        &source,
        sample_start_ms,
        end_ms,
        audio_track_index,
        audio_track_ordinal,
    )
    .await
    {
        Ok((path, data)) => {
            let b64 = media::to_base64(&data);
            media::cleanup_temp_file(&path).await;
            Json(serde_json::json!({
                "audio_base64": b64,
                "format": "opus",
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

// === WebSocket Handler ===

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

#[derive(Serialize)]
struct WsMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<SessionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitles: Option<SubtitleData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_line_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_card: Option<EnrichmentDialogState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anki_status: Option<AnkiStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enhancement_queue: Option<Vec<EnhancementQueueItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enhancement_result: Option<EnhancementResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_result: Option<RemoteControlResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_tracks: Option<AudioTracksData>,
}

#[derive(Serialize, Clone, PartialEq)]
struct AudioTracksData {
    tracks: Vec<crate::session::AudioTrack>,
    selected_index: Option<u32>,
    resolution: crate::session::AudioTrackResolution,
}

#[derive(Serialize)]
struct SubtitleData {
    lines: Vec<crate::subtitle::SubtitleLine>,
    count: usize,
    candidates: Vec<SubtitleCandidate>,
    selected_candidate_id: Option<String>,
    selection_mode: SubtitleSelectionMode,
}

async fn audio_tracks_data(state: &Arc<AppState>) -> AudioTracksData {
    let tracks = state.audio_tracks.read().await.clone();
    let selected_index = *state.selected_audio_track.read().await;
    let resolution = *state.audio_track_resolution.read().await;
    AudioTracksData {
        tracks,
        selected_index,
        resolution,
    }
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    let mut session_rx = state.session_rx.clone();
    let mut card_rx = state.new_card_tx.subscribe();
    let mut result_rx = state.enhancement_result_tx.subscribe();
    let mut remote_rx = state.remote_result_tx.subscribe();
    let subtitles = state.subtitles.clone();
    let subtitle_candidates = state.subtitle_candidates.clone();
    let anki_status = state.anki_status.clone();
    let enhancement_queue = state.enhancement_queue.clone();

    // Send initial state
    {
        let subs = subtitles.read().await;
        let session_state = session_rx.borrow().clone();
        let current_anki_status = anki_status.read().await.clone();
        let current_enhancement_queue = enhancement_queue.read().await.clone();
        let subtitle_data = active_subtitle_data(&state).await;
        let active_line =
            if let (Some(track), Some(np)) = (subs.as_ref(), &session_state.now_playing) {
                track
                    .line_at_time(np.position_ms)
                    .or_else(|| track.nearest_line(np.position_ms))
            } else {
                None
            };

        let init_msg = WsMessage {
            msg_type: "init".to_string(),
            state: Some(session_state),
            subtitles: Some(subtitle_data),
            active_line_index: active_line,
            new_card: None,
            anki_status: Some(current_anki_status),
            enhancement_queue: Some(current_enhancement_queue),
            enhancement_result: None,
            remote_result: None,
            audio_tracks: Some(audio_tracks_data(&state).await),
        };

        if let Ok(json) = serde_json::to_string(&init_msg) {
            let _ = sender.send(Message::Text(json.into())).await;
        }
    }

    let send_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        let mut last_item_id: Option<String> = None;
        let mut last_subtitle_signature: Option<(
            Option<String>,
            Option<String>,
            SubtitleSelectionMode,
            usize,
            Vec<SubtitleCandidate>,
        )> = None;
        let mut last_anki_status: Option<AnkiStatus> = None;
        let mut last_enhancement_queue: Vec<EnhancementQueueItem> = Vec::new();
        let mut last_audio_tracks: Option<AudioTracksData> = None;
        let audio_tracks_arc = state.audio_tracks.clone();
        let selected_audio_arc = state.selected_audio_track.clone();
        let audio_resolution_arc = state.audio_track_resolution.clone();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let session_state = session_rx.borrow_and_update().clone();
                    let subs = subtitles.read().await;
                    let current_candidates = subtitle_candidates.read().await.clone();

                    let current_item_id = session_state
                        .now_playing
                        .as_ref()
                        .map(|np| np.history_id.clone());
                    let current_anki_status = anki_status.read().await.clone();
                    let current_enhancement_queue = enhancement_queue.read().await.clone();
                    let selected_candidate_id = session_state
                        .now_playing
                        .as_ref()
                        .and_then(|np| np.subtitle_candidate_id.clone());
                    let selection_mode = session_state
                        .now_playing
                        .as_ref()
                        .map(|np| np.subtitle_selection_mode)
                        .unwrap_or(SubtitleSelectionMode::Auto);
                    let subtitle_lines = subs.as_ref().map(|track| track.lines.clone()).unwrap_or_default();
                    let subtitle_data = SubtitleData {
                        count: subtitle_lines.len(),
                        lines: subtitle_lines,
                        candidates: current_candidates.clone(),
                        selected_candidate_id,
                        selection_mode,
                    };
                    let current_subtitle_signature = (
                        current_item_id.clone(),
                        subtitle_data.selected_candidate_id.clone(),
                        subtitle_data.selection_mode,
                        subtitle_data.count,
                        subtitle_data.candidates.clone(),
                    );

                    let send_subs = current_item_id != last_item_id
                        || last_subtitle_signature.as_ref() != Some(&current_subtitle_signature);
                    last_item_id = current_item_id;
                    if send_subs {
                        last_subtitle_signature = Some(current_subtitle_signature);
                    }
                    let send_anki_status = last_anki_status.as_ref() != Some(&current_anki_status);
                    if send_anki_status {
                        last_anki_status = Some(current_anki_status.clone());
                    }
                    let send_enhancement_queue = last_enhancement_queue != current_enhancement_queue;
                    if send_enhancement_queue {
                        last_enhancement_queue = current_enhancement_queue.clone();
                    }

                    // Audio tracks change detection
                    let current_audio_data = AudioTracksData {
                        tracks: audio_tracks_arc.read().await.clone(),
                        selected_index: *selected_audio_arc.read().await,
                        resolution: *audio_resolution_arc.read().await,
                    };
                    let send_audio = last_audio_tracks.as_ref() != Some(&current_audio_data);
                    if send_audio {
                        last_audio_tracks = Some(current_audio_data.clone());
                    }

                    let active_line = if let (Some(track), Some(np)) = (subs.as_ref(), &session_state.now_playing) {
                        track.line_at_time(np.position_ms).or_else(|| track.nearest_line(np.position_ms))
                    } else {
                        None
                    };

                    let is_full_update = send_subs || send_audio;

                    let msg = WsMessage {
                        msg_type: if is_full_update { "full_update".to_string() } else { "position".to_string() },
                        state: Some(session_state),
                        subtitles: if send_subs {
                            Some(subtitle_data)
                        } else {
                            None
                        },
                        active_line_index: active_line,
                        new_card: None,
                        anki_status: if send_anki_status {
                            Some(current_anki_status)
                        } else {
                            None
                        },
                        enhancement_queue: if send_enhancement_queue {
                            Some(current_enhancement_queue)
                        } else {
                            None
                        },
                        enhancement_result: None,
                        remote_result: None,
                        audio_tracks: if send_audio {
                            Some(current_audio_data)
                        } else {
                            None
                        },
                    };

                    if let Ok(json) = serde_json::to_string(&msg) {
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                card_result = card_rx.recv() => {
                    let candidate = match card_result {
                        Ok(e) => e,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("WS new_card receiver lagged, {} message(s) lost", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };

                    let msg = WsMessage {
                        msg_type: "new_card".to_string(),
                        state: None,
                        subtitles: None,
                        active_line_index: None,
                        new_card: Some(candidate),
                        anki_status: None,
                        enhancement_queue: None,
                        enhancement_result: None,
                        remote_result: None,
                        audio_tracks: None,
                    };

                    if let Ok(json) = serde_json::to_string(&msg) {
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                result = result_rx.recv() => {
                    let enhancement_result = match result {
                        Ok(r) => r,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("WS enhancement_result receiver lagged, {} message(s) lost", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };

                    let msg = WsMessage {
                        msg_type: "enhancement_result".to_string(),
                        state: None,
                        subtitles: None,
                        active_line_index: None,
                        new_card: None,
                        anki_status: None,
                        enhancement_queue: None,
                        enhancement_result: Some(enhancement_result),
                        remote_result: None,
                        audio_tracks: None,
                    };

                    if let Ok(json) = serde_json::to_string(&msg) {
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                remote = remote_rx.recv() => {
                    let remote_result = match remote {
                        Ok(r) => r,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("WS remote_result receiver lagged, {} message(s) lost", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };

                    let msg = WsMessage {
                        msg_type: "remote_result".to_string(),
                        state: None,
                        subtitles: None,
                        active_line_index: None,
                        new_card: None,
                        anki_status: None,
                        enhancement_queue: None,
                        enhancement_result: None,
                        remote_result: Some(remote_result),
                        audio_tracks: None,
                    };

                    if let Ok(json) = serde_json::to_string(&msg) {
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Handle incoming messages (e.g., client commands)
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(_msg)) = receiver.next().await {
            // Currently no client-to-server messages needed
            // Could add seek commands, etc.
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
}
