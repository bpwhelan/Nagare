use crate::anki::{AnkiClient, AnkiStatus, NewCardEvent};
use crate::config::{Config, MediaServerKind};
use crate::media;
use crate::media_server::{MediaServer, ServerMap};
use crate::mining::{
    AppDatabase, EnrichmentDialogState, EnrichmentSource, MiningHistoryEntry, MiningHistorySummary,
};
use crate::session::{
    HistoryEntry, SessionManager, SessionState, scoped_history_id, split_scoped_id,
};
use crate::subtitle::{SubtitleTrack, find_all_matching_lines, find_matching_line};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
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
    pub enhancement_status: Arc<RwLock<Option<String>>>,
    pub session_rx: watch::Receiver<SessionState>,
    pub new_card_tx: broadcast::Sender<EnrichmentDialogState>,
    pub subtitles: Arc<RwLock<Option<SubtitleTrack>>>,
    pub subtitle_history: Arc<RwLock<HashMap<String, SubtitleTrack>>>,
    pub history: Arc<RwLock<HashMap<String, HistoryEntry>>>,
    /// Queue of pending enrichment requests from the frontend.
    pub pending_enrichments: Arc<RwLock<Vec<EnrichmentDialogState>>>,
}

pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/sessions", get(get_sessions))
        .route("/api/sessions/select", post(select_session))
        .route("/api/subtitles", get(get_subtitles))
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

async fn get_subtitles(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let subs = state.subtitles.read().await;
    match subs.as_ref() {
        Some(track) => Json(serde_json::json!({
            "lines": track.lines,
            "count": track.lines.len(),
        })),
        None => Json(serde_json::json!({
            "lines": [],
            "count": 0,
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
) -> Json<serde_json::Value> {
    let sh = state.subtitle_history.read().await;
    match sh.get(&history_id) {
        Some(track) => Json(serde_json::json!({
            "lines": track.lines,
            "count": track.lines.len(),
        })),
        None => Json(serde_json::json!({
            "lines": [],
            "count": 0,
        })),
    }
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

#[derive(Deserialize)]
struct EnrichRequest {
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
        let hist = state.history.read().await;
        return hist
            .get(history_id)
            .map(history_entry_to_media_context)
            .ok_or_else(|| "Item not found in history".to_string());
    }

    let session_state = state.session_rx.borrow().clone();
    if let Some(np) = session_state.now_playing {
        return Ok(MediaContext {
            server_kind: np.server_kind,
            item_id: np.item_id,
            media_source_id: np.media_source_id,
            file_path: np.file_path,
            title: np.title,
        });
    }

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

async fn prepare_enrichment_candidate(
    state: &Arc<AppState>,
    event: NewCardEvent,
) -> Option<EnrichmentDialogState> {
    let subtitles = state.subtitles.read().await;
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

    let matched_line_index = subtitles
        .as_ref()
        .and_then(|track| find_matching_line(track, &event.sentence, position_ms, 30_000));
    drop(subtitles);

    let matched_line_index = matched_line_index?;
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

fn default_request_from_candidate(
    candidate: &EnrichmentDialogState,
    config: &Config,
    track: &SubtitleTrack,
) -> Option<EnrichRequest> {
    let matched_line_index = candidate.matched_line_index?;
    let line = track
        .lines
        .iter()
        .find(|line| line.index == matched_line_index)?;
    Some(EnrichRequest {
        note_id: candidate.event.note_id,
        sentence: Some(candidate.event.sentence.clone()),
        start_ms: (line.start_ms - config.mining.audio_start_offset_ms).max(0),
        end_ms: line.end_ms + config.mining.audio_end_offset_ms,
        generate_avif: config.mining.generate_avif,
        matched_line_index: Some(matched_line_index),
        included_line_first: Some(matched_line_index),
        included_line_last: Some(matched_line_index),
        item_id: candidate.history_id.clone(),
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

async fn set_enhancement_status(state: &Arc<AppState>, message: Option<String>) {
    let mut status = state.enhancement_status.write().await;
    *status = message;
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

async fn perform_enrichment(
    state: &Arc<AppState>,
    req: &EnrichRequest,
    fallback_event: Option<NewCardEvent>,
    fallback_card_ids: Vec<i64>,
) -> Result<(), String> {
    set_enhancement_status(state, Some(format!("Enhancing note #{}...", req.note_id))).await;

    let result = async {
        let media_ctx = resolve_media_context(state, req.item_id.as_deref()).await?;
        let config = state.config.read().await.clone();
        let server_opt = get_server(state, media_ctx.server_kind).await;
        let anki_client = state.anki_client.read().await.clone();

        let source = media::resolve_media_source(
            &config,
            server_opt.as_deref(),
            &media_ctx.item_id,
            &media_ctx.media_source_id,
            media_ctx.file_path.as_deref(),
        )
        .map_err(|error| format!("Failed to resolve media source: {}", error))?;

        let (audio_path, audio_data) = media::extract_audio(&source, req.start_ms, req.end_ms)
            .await
            .map_err(|error| format!("Audio extraction failed: {}", error))?;
        let audio_filename = format!("nagare_{}_{}.opus", media_ctx.item_id, req.start_ms);
        let audio_b64 = media::to_base64(&audio_data);

        if let Err(error) = anki_client
            .store_media_file(&audio_filename, &audio_b64)
            .await
        {
            media::cleanup_temp_file(&audio_path).await;
            return Err(format!("Failed to store audio: {}", error));
        }

        let mut picture_html = String::new();
        if req.generate_avif {
            match media::generate_avif(&source, req.start_ms, req.end_ms).await {
                Ok((avif_path, avif_data)) => {
                    let avif_filename =
                        format!("nagare_{}_{}.avif", media_ctx.item_id, req.start_ms);
                    let avif_b64 = media::to_base64(&avif_data);
                    if let Err(error) = anki_client
                        .store_media_file(&avif_filename, &avif_b64)
                        .await
                    {
                        warn!("Failed to store AVIF in Anki: {}", error);
                    } else {
                        picture_html = format!("<img src=\"{}\">", avif_filename);
                    }
                    media::cleanup_temp_file(&avif_path).await;
                }
                Err(error) => {
                    warn!("AVIF generation failed (continuing without): {}", error);
                    let mid_ms = (req.start_ms + req.end_ms) / 2;
                    if let Ok((ss_path, ss_data)) =
                        media::generate_screenshot(&source, mid_ms).await
                    {
                        let ss_filename =
                            format!("nagare_{}_{}_ss.webp", media_ctx.item_id, mid_ms);
                        let ss_b64 = media::to_base64(&ss_data);
                        if anki_client
                            .store_media_file(&ss_filename, &ss_b64)
                            .await
                            .is_ok()
                        {
                            picture_html = format!("<img src=\"{}\">", ss_filename);
                        }
                        media::cleanup_temp_file(&ss_path).await;
                    }
                }
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

        if let Err(error) = anki_client
            .update_note_fields(req.note_id, fields, None, None)
            .await
        {
            media::cleanup_temp_file(&audio_path).await;
            return Err(format!("Failed to update note: {}", error));
        }

        if !config.anki.add_tags.is_empty() {
            let tags_str = config.anki.add_tags.join(" ");
            if let Err(error) = anki_client.add_tags(&[req.note_id], &tags_str).await {
                warn!("Failed to add tags to note {}: {}", req.note_id, error);
            }
        }

        media::cleanup_temp_file(&audio_path).await;
        save_mining_history_entry(state, req, &media_ctx, fallback_event, fallback_card_ids).await;
        Ok(())
    }
    .await;

    set_enhancement_status(state, None).await;
    result
}

pub async fn run_new_card_processor(state: Arc<AppState>, mut rx: mpsc::Receiver<NewCardEvent>) {
    while let Some(event) = rx.recv().await {
        let Some(candidate) = prepare_enrichment_candidate(&state, event).await else {
            continue;
        };

        let auto_approve = state.config.read().await.mining.auto_approve;
        if auto_approve {
            let track = if let Some(history_id) = candidate.history_id.as_deref() {
                state.subtitle_history.read().await.get(history_id).cloned()
            } else {
                state.subtitles.read().await.clone()
            };

            if let Some(track) = track {
                let config = state.config.read().await.clone();
                if let Some(req) = default_request_from_candidate(&candidate, &config, &track) {
                    match perform_enrichment(
                        &state,
                        &req,
                        Some(candidate.event.clone()),
                        candidate.card_ids.clone(),
                    )
                    .await
                    {
                        Ok(()) => {
                            info!("Auto-approved note {}", candidate.event.note_id);
                            continue;
                        }
                        Err(error) => {
                            warn!(
                                "Auto-approve failed for note {}: {}. Falling back to manual review.",
                                candidate.event.note_id, error
                            );
                        }
                    }
                }
            }
        }

        queue_pending_enrichment(&state, candidate).await;
    }
}

async fn enrich_card(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrichRequest>,
) -> Json<EnrichResponse> {
    let fallback = {
        let pending = state.pending_enrichments.read().await;
        pending
            .iter()
            .find(|entry| entry.event.note_id == req.note_id)
            .cloned()
    };

    match perform_enrichment(
        &state,
        &req,
        fallback.as_ref().map(|entry| entry.event.clone()),
        fallback
            .as_ref()
            .map(|entry| entry.card_ids.clone())
            .unwrap_or_default(),
    )
    .await
    {
        Ok(()) => {
            remove_pending_enrichment(&state, req.note_id).await;
            info!("Successfully enriched note {}", req.note_id);
            Json(EnrichResponse {
                success: true,
                error: None,
            })
        }
        Err(error) => {
            error!("Enrichment failed for note {}: {}", req.note_id, error);
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

    let ticks = req.position_ms * 10_000;
    let Some(server) = get_server(&state, server_kind).await else {
        return Json(
            serde_json::json!({"ok": false, "error": format!("{} is not enabled", server_kind.display_name())}),
        );
    };

    if let Err(e) = server.seek_session(session_id, ticks).await {
        return Json(serde_json::json!({"ok": false, "error": e.to_string()}));
    }

    let session_manager = state.session_manager.clone();
    let paused = now_playing.is_paused;
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        session_manager
            .force_refresh_after_remote_command(session_id, Some(req.position_ms), Some(paused))
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

    let result = if should_pause {
        srv.pause_session(session_id).await
    } else {
        srv.unpause_session(session_id).await
    };

    match result {
        Ok(_) => {
            let session_manager = state.session_manager.clone();
            let session_id = session_id.to_string();
            tokio::spawn(async move {
                session_manager
                    .force_refresh_after_remote_command(session_id, None, Some(should_pause))
                    .await;
            });

            Json(serde_json::json!({"ok": true, "paused": should_pause}))
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})),
    }
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

    match media::extract_audio(&source, req.start_ms, req.end_ms).await {
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
    enhancement_status: Option<Option<String>>,
}

#[derive(Serialize)]
struct SubtitleData {
    lines: Vec<crate::subtitle::SubtitleLine>,
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    let mut session_rx = state.session_rx.clone();
    let mut card_rx = state.new_card_tx.subscribe();
    let subtitles = state.subtitles.clone();
    let anki_status = state.anki_status.clone();
    let enhancement_status = state.enhancement_status.clone();

    // Send initial state
    {
        let subs = subtitles.read().await;
        let session_state = session_rx.borrow().clone();
        let current_anki_status = anki_status.read().await.clone();
        let current_enhancement_status = enhancement_status.read().await.clone();
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
            subtitles: subs.as_ref().map(|t| SubtitleData {
                lines: t.lines.clone(),
            }),
            active_line_index: active_line,
            new_card: None,
            anki_status: Some(current_anki_status),
            enhancement_status: Some(current_enhancement_status),
        };

        if let Ok(json) = serde_json::to_string(&init_msg) {
            let _ = sender.send(Message::Text(json.into())).await;
        }
    }

    let send_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        let mut last_item_id: Option<String> = None;
        let mut last_anki_status: Option<AnkiStatus> = None;
        let mut last_enhancement_status: Option<String> = None;
        let mut sent_card_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let session_state = session_rx.borrow_and_update().clone();
                    let subs = subtitles.read().await;

                    let current_item_id = session_state
                        .now_playing
                        .as_ref()
                        .map(|np| np.history_id.clone());
                    let current_anki_status = anki_status.read().await.clone();
                    let current_enhancement_status = enhancement_status.read().await.clone();

                    // If item changed, send full subtitles
                    let send_subs = current_item_id != last_item_id;
                    last_item_id = current_item_id;
                    let send_anki_status = last_anki_status.as_ref() != Some(&current_anki_status);
                    if send_anki_status {
                        last_anki_status = Some(current_anki_status.clone());
                    }
                    let send_enhancement_status = last_enhancement_status != current_enhancement_status;
                    if send_enhancement_status {
                        last_enhancement_status = current_enhancement_status.clone();
                    }

                    let active_line = if let (Some(track), Some(np)) = (subs.as_ref(), &session_state.now_playing) {
                        track.line_at_time(np.position_ms).or_else(|| track.nearest_line(np.position_ms))
                    } else {
                        None
                    };

                    let msg = WsMessage {
                        msg_type: if send_subs { "full_update".to_string() } else { "position".to_string() },
                        state: Some(session_state),
                        subtitles: if send_subs {
                            subs.as_ref().map(|t| SubtitleData { lines: t.lines.clone() })
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
                        enhancement_status: if send_enhancement_status {
                            Some(current_enhancement_status)
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

                    if sent_card_ids.contains(&candidate.event.note_id) {
                        continue;
                    }
                    sent_card_ids.insert(candidate.event.note_id);

                    let msg = WsMessage {
                        msg_type: "new_card".to_string(),
                        state: None,
                        subtitles: None,
                        active_line_index: None,
                        new_card: Some(candidate),
                        anki_status: None,
                        enhancement_status: None,
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
