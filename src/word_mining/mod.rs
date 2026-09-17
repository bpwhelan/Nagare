mod analysis;
pub(crate) mod db;
mod dictionary;
pub(crate) mod jobs;
mod tokenizer;

use crate::{
    api::AppState,
    config::Config,
    media,
    session::HistoryEntry,
    subtitle::{SubtitleTrack, parse_subtitle},
};
use anyhow::{Context, bail};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub term: String,
    pub reading: String,
    pub definitions: Vec<dictionary::Definition>,
    pub common: bool,
    pub part_of_speech: String,
    pub occurrences: Vec<usize>,
    pub surfaces: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub revision: String,
    pub history: HistoryEntry,
    pub track: SubtitleTrack,
    pub subtitle_name: String,
    pub candidates: Vec<Candidate>,
    pub line_words: Vec<Vec<String>>,
    pub dictionary_date: Option<String>,
    pub split_mode: String,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/history/{id}/word-mining", get(workspace))
        .route("/api/history/{id}/word-mining/analyze", post(analyze))
        .route("/api/history/{id}/word-mining/term", post(add_term))
        .route("/api/history/{id}/word-mining/media", get(media_info))
        .route("/api/history/{id}/word-mining/preview", post(preview))
        .route("/api/history/{id}/word-mining/jobs", post(create_job))
        .route("/api/word-mining/anki", get(anki_options))
        .route("/api/word-mining/known", put(set_known))
        .route("/api/word-mining/sync-known", post(sync_known))
        .route("/api/word-mining/dictionary", post(update_dictionary))
        .route("/api/word-mining/jobs/{id}", get(get_job))
        .route("/api/word-mining/jobs/{id}/pause", post(pause_job))
        .route("/api/word-mining/jobs/{id}/resume", post(resume_job))
}

fn response(result: anyhow::Result<Value>) -> Json<Value> {
    Json(match result {
        Ok(value) => value,
        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
    })
}

async fn history(state: &AppState, id: &str) -> anyhow::Result<HistoryEntry> {
    state
        .history
        .read()
        .await
        .get(id)
        .cloned()
        .context("This file is no longer in watch history")
}

async fn workspace(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    response(
        async {
            let item = history(&state, &id).await?;
            let workspace = db::workspace(state.db.db_path.clone(), id.clone()).await?;
            let known = db::known(state.db.db_path.clone()).await?;
            let jobs = db::jobs(state.db.db_path.clone(), id).await?;
            Ok(json!({"ok":true,"history":item,"workspace":workspace,"known":known,"jobs":jobs}))
        }
        .await,
    )
}

#[derive(Deserialize, Default)]
struct AnalyzeRequest {
    subtitle_text: Option<String>,
    subtitle_name: Option<String>,
    split_mode: Option<String>,
}
static ANALYSIS_SLOTS: Semaphore = Semaphore::const_new(1);

async fn analyze(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<AnalyzeRequest>,
) -> Json<Value> {
    response(async {
        let _permit=ANALYSIS_SLOTS.try_acquire().context("Another subtitle is being analyzed; try again when it finishes")?;
        let item=history(&state,&id).await?;
        let split_mode=req.split_mode.unwrap_or_else(||"B".into());
        if !matches!(split_mode.as_str(),"A"|"B"|"C") { bail!("Choose Sudachi mode A, B, or C"); }
        let (track,name)=if let Some(text)=req.subtitle_text {
            if text.len()>2_000_000 { bail!("Subtitle files must be under 2 MB"); }
            let name=req.subtitle_name.unwrap_or_else(|| "Uploaded subtitle".into());
            (parse_subtitle(text.trim_start_matches('\u{feff}'),Some(&name)),name)
        } else if let Some(saved)=db::workspace(state.db.db_path.clone(),id.clone()).await? {
            (saved.track,saved.subtitle_name)
        } else {
            let track=state.subtitle_history.read().await.get(&id).cloned().context("No subtitles are saved for this file. Choose an SRT, VTT, or ASS file.")?;
            (track,"Saved subtitles".into())
        };
        let (lexicon,warning)=match dictionary::load(state.db.db_path.clone(),false).await {
            Ok(l)=>(Some(l),None),Err(error)=>(None,Some(format!("Definitions unavailable: {error}. You can still select words and enter meanings; use Update dictionary to retry."))),
        };
        let dictionary=tokenizer::load(state.db.db_path.clone()).await.context("Could not load Sudachi core dictionary")?;
        let workspace=tokio::task::spawn_blocking(move || analysis::analyze(item,track,name,lexicon,dictionary,split_mode)).await??;
        db::save_workspace(state.db.db_path.clone(),workspace.clone()).await?;
        Ok(json!({"ok":true,"workspace":workspace,"warning":warning,"known":db::known(state.db.db_path.clone()).await?}))
    }.await)
}

#[derive(Deserialize)]
struct TermRequest {
    term: String,
    revision: String,
}
async fn add_term(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<TermRequest>,
) -> Json<Value> {
    response(
        async {
            let _permit = ANALYSIS_SLOTS
                .try_acquire()
                .context("Wait for subtitle analysis to finish")?;
            let term = req.term.trim().to_string();
            if term.is_empty() || term.len() > 200 {
                bail!("Enter a word or short phrase");
            }
            let mut workspace = db::workspace(state.db.db_path.clone(), id)
                .await?
                .context("Analyze this subtitle first")?;
            if workspace.revision != req.revision {
                bail!("This subtitle changed in another tab; reload the mining page");
            }
            if !workspace.candidates.iter().any(|c| c.term == term) {
                let occurrences = workspace
                    .track
                    .lines
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| l.text.contains(&term).then_some(i))
                    .collect::<Vec<_>>();
                if occurrences.is_empty() {
                    bail!("This word or phrase does not appear in the subtitles");
                }
                let definitions = dictionary::load(state.db.db_path.clone(), false)
                    .await
                    .ok()
                    .map(|l| l.lookup(&term, ""))
                    .unwrap_or_default();
                workspace.candidates.push(Candidate {
                    term: term.clone(),
                    reading: definitions
                        .first()
                        .map(|d| d.reading.clone())
                        .unwrap_or_default(),
                    common: definitions.iter().any(|d| d.common),
                    definitions,
                    occurrences,
                    surfaces: vec![term],
                    part_of_speech: "custom phrase".into(),
                });
                db::save_workspace(state.db.db_path.clone(), workspace.clone()).await?;
            }
            Ok(json!({"ok":true,"workspace":workspace}))
        }
        .await,
    )
}

async fn update_dictionary(State(state): State<Arc<AppState>>) -> Json<Value> {
    response(
        dictionary::load(state.db.db_path.clone(), true)
            .await
            .map(|l| json!({"ok":true,"date":l.date})),
    )
}

#[derive(Deserialize)]
struct KnownRequest {
    terms: Vec<String>,
    status: String,
}
async fn set_known(
    State(state): State<Arc<AppState>>,
    Json(req): Json<KnownRequest>,
) -> Json<Value> {
    response(
        async {
            if !matches!(req.status.as_str(), "known" | "ignored" | "new")
                || req.terms.len() > 20_000
                || req
                    .terms
                    .iter()
                    .any(|t| t.trim().is_empty() || t.len() > 200)
            {
                bail!("Invalid word list");
            }
            db::set_known(
                state.db.db_path.clone(),
                req.terms
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .collect(),
                req.status,
            )
            .await?;
            Ok(json!({"ok":true,"known":db::known(state.db.db_path.clone()).await?}))
        }
        .await,
    )
}

#[derive(Deserialize)]
struct AnkiQuery {
    model: Option<String>,
}
async fn anki_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AnkiQuery>,
) -> Json<Value> {
    response(async {
        let client=state.anki_client.read().await.clone();
        let (decks,models)=tokio::try_join!(client.invoke("deckNames",json!({})),client.invoke("modelNames",json!({})))?;
        let model=query.model.unwrap_or_else(||jobs::MODEL.into());
        let exists=models.as_array().is_some_and(|models|models.iter().any(|m|m.as_str()==Some(&model)));
        let fields=if !exists && model==jobs::MODEL { json!(jobs::Fields::default().names()) }
            else { client.invoke("modelFieldNames",json!({"modelName":model})).await? };
        Ok(json!({"ok":true,"decks":decks,"models":models,"fields":fields,"default_model":jobs::MODEL,"default_fields":jobs::Fields::default()}))
    }.await)
}

#[derive(Deserialize)]
struct SyncRequest {
    model: String,
    field: String,
}
async fn sync_known(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SyncRequest>,
) -> Json<Value> {
    response(
        async {
            let client = state.anki_client.read().await.clone();
            let fields = client
                .invoke("modelFieldNames", json!({"modelName":req.model}))
                .await?;
            if !fields
                .as_array()
                .is_some_and(|f| f.iter().any(|f| f.as_str() == Some(&req.field)))
            {
                bail!("Choose the field that contains the word in Anki");
            }
            let words = jobs::words_in_anki(&client, &req.model, &req.field).await?;
            let count = words.len();
            let url = state.config.read().await.anki.url.clone();
            db::sync_known(
                state.db.db_path.clone(),
                format!("{url}|{}|{}", req.model, req.field),
                words.into_iter().collect(),
            )
            .await?;
            Ok(json!({"ok":true,"count":count,"known":db::known(state.db.db_path.clone()).await?}))
        }
        .await,
    )
}

async fn media_source(
    state: &AppState,
    item: &HistoryEntry,
    config: &Config,
) -> anyhow::Result<String> {
    let server = state.servers.read().await.get(&item.server_kind).cloned();
    media::resolve_media_source(
        config,
        server.as_deref(),
        &item.item_id,
        &item.media_source_id,
        item.file_path.as_deref(),
    )
}

#[derive(Serialize)]
struct AudioStream {
    ordinal: usize,
    index: u32,
    language: Option<String>,
    title: String,
}
async fn probe_audio(source: &str) -> anyhow::Result<Vec<AudioStream>> {
    let mut command = tokio::process::Command::new("ffprobe");
    command.kill_on_drop(true).args([
        "-v",
        "error",
        "-select_streams",
        "a",
        "-show_entries",
        "stream=index:stream_tags=language,title",
        "-of",
        "json",
        source,
    ]);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await??;
    if !output.status.success() {
        bail!("Could not read this file's audio tracks. Check media access in Settings.");
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(value["streams"]
        .as_array()
        .context("No audio streams found")?
        .iter()
        .enumerate()
        .map(|(ordinal, s)| AudioStream {
            ordinal,
            index: s["index"].as_u64().unwrap_or(ordinal as u64) as u32,
            language: s["tags"]["language"].as_str().map(String::from),
            title: s["tags"]["title"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("Audio {}", ordinal + 1)),
        })
        .collect())
}

async fn media_info(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    response(
        async {
            let item = history(&state, &id).await?;
            let config = state.config.read().await.clone();
            let source = media_source(&state, &item, &config).await?;
            let tracks = probe_audio(&source).await?;
            let selected = tracks
                .iter()
                .find(|t| {
                    crate::session::SessionManager::language_matches_target(
                        t.language.as_deref(),
                        &config.target_language,
                    )
                })
                .or_else(|| (tracks.len() == 1).then(|| &tracks[0]))
                .map(|t| t.ordinal);
            Ok(json!({"ok":true,"tracks":tracks,"selected":selected}))
        }
        .await,
    )
}

pub fn validate_range(start: i64, end: i64, duration: Option<i64>) -> anyhow::Result<()> {
    if start < 0
        || end <= start
        || end.saturating_sub(start) > 90_000
        || duration.is_some_and(|d| d > 0 && end > d)
    {
        bail!("Choose a clip between 0 and 90 seconds long, within the media's duration");
    }
    Ok(())
}

#[derive(Deserialize)]
struct PreviewRequest {
    kind: String,
    start_ms: i64,
    end_ms: i64,
    audio_ordinal: Option<usize>,
}
async fn preview(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<PreviewRequest>,
) -> Json<Value> {
    response(async {
        let item=history(&state,&id).await?;
        validate_range(req.start_ms,req.end_ms,item.duration_ms)?;
        let config=state.config.read().await.clone();
        let source=media_source(&state,&item,&config).await?;
        let result=tokio::time::timeout(Duration::from_secs(45),async {
            if req.kind=="audio" {
                let tracks=probe_audio(&source).await?;
                let ordinal=req.audio_ordinal.or_else(||tracks.iter().find(|t|crate::session::SessionManager::language_matches_target(t.language.as_deref(),&config.target_language)).map(|t|t.ordinal))
                    .or_else(||(tracks.len()==1).then_some(0)).context("Choose an audio track first")?;
                if ordinal>=tracks.len() { bail!("The chosen audio track no longer exists"); }
                let (path,data)=media::extract_audio(&source,req.start_ms,req.end_ms,None,Some(ordinal),config.mining.audio_codec).await?;
                media::cleanup_temp_file(&path).await;
                Ok(json!({"ok":true,"data":media::to_base64(&data),"mime":config.mining.audio_codec.mime_type()}))
            } else if req.kind=="image" {
                if !media::probe_media(&source,req.start_ms).await?.has_visual_video { bail!("This is an audio-only file"); }
                let image=media::generate_screenshot(&source,req.start_ms+(req.end_ms-req.start_ms)/2,config.mining.static_screenshot_format).await?;
                media::cleanup_temp_file(&image.path).await;
                Ok(json!({"ok":true,"data":media::to_base64(&image.data),"mime":image.format.mime_type()}))
            } else { bail!("Unknown preview type") }
        }).await.context("Preview timed out")?;
        result
    }.await)
}

#[derive(Deserialize)]
struct CreateRequest {
    request_id: String,
    revision: String,
    settings: jobs::Settings,
    cards: Vec<jobs::Draft>,
}
async fn create_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<CreateRequest>,
) -> Json<Value> {
    response(
        async {
            uuid::Uuid::parse_str(&req.request_id).context("Invalid batch request ID")?;
            if let Some(job) = db::job(state.db.db_path.clone(), req.request_id.clone()).await? {
                // The worker may have resolved an automatic audio choice since the request was saved.
                let mut saved_settings = job.settings.clone();
                if req.settings.audio_ordinal.is_none() {
                    saved_settings.audio_ordinal = None;
                }
                if job.history.history_id != id
                    || saved_settings != req.settings
                    || job.cards.iter().map(|c| &c.draft).ne(req.cards.iter())
                {
                    bail!("This request ID belongs to a different batch");
                }
                return Ok(json!({"ok":true,"job":job.view()}));
            }
            let guard = jobs::RUN_LOCK
                .clone()
                .try_lock_owned()
                .context("Another mining batch is running. Pause it or wait for it to finish.")?;
            let workspace = db::workspace(state.db.db_path.clone(), id.clone())
                .await?
                .context("Analyze this file first")?;
            if workspace.revision != req.revision {
                bail!("The subtitle changed. Reload the mining page before creating cards.");
            }
            jobs::validate_drafts(&workspace, &req.cards)?;
            if req.settings.audio_ordinal.is_some_and(|i| i > 63) {
                bail!("Invalid audio track");
            }
            let client = state.anki_client.read().await.clone();
            jobs::validate_settings(&client, &req.settings, false).await?;
            let config = state.config.read().await.clone();
            let mut settings = req.settings;
            // Reject ambiguous audio before persisting a job, so the user can fix
            // the selection in the confirmation flow instead of stranding a batch.
            jobs::resolve_audio(&state, &workspace.history, &config, &mut settings).await?;
            let job = jobs::Job {
                id: req.request_id,
                status: "running".into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                history: workspace.history,
                track: workspace.track,
                settings,
                mining_config: config.mining,
                anki_url: config.anki.url,
                cards: req
                    .cards
                    .into_iter()
                    .map(|draft| jobs::JobCard {
                        draft,
                        status: "pending".into(),
                        note_id: None,
                        error: None,
                        recorded: false,
                        event: None,
                    })
                    .collect(),
            };
            db::save_job(state.db.db_path.clone(), job.clone()).await?;
            let view = job.view();
            jobs::start(state, job, guard);
            Ok(json!({"ok":true,"job":view}))
        }
        .await,
    )
}

async fn get_job(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    response(async { Ok(json!({"ok":true,"job":db::job(state.db.db_path.clone(),id).await?.context("Batch not found")?.view()})) }.await)
}
async fn pause_job(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    response(
        async {
            db::cancel(state.db.db_path.clone(), id, true).await?;
            Ok(json!({"ok":true}))
        }
        .await,
    )
}
async fn resume_job(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    response(
        async {
            let guard = jobs::RUN_LOCK
                .clone()
                .try_lock_owned()
                .context("A batch is already running")?;
            let mut job = db::job(state.db.db_path.clone(), id.clone())
                .await?
                .context("Batch not found")?;
            if job.status == "complete" {
                return Ok(json!({"ok":true,"job":job.view()}));
            }
            job.status = "running".into();
            db::cancel(state.db.db_path.clone(), id, false).await?;
            db::save_job(state.db.db_path.clone(), job.clone()).await?;
            let view = job.view();
            jobs::start(state, job, guard);
            Ok(json!({"ok":true,"job":view}))
        }
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clip_bounds_reject_reversed_negative_oversized_and_past_end_ranges() {
        for (start, end) in [
            (-1, 1000),
            (1000, 1000),
            (2000, 1000),
            (0, 90001),
            (9990, 10001),
        ] {
            assert!(validate_range(start, end, Some(10000)).is_err());
        }
        assert!(validate_range(0, 10000, Some(10000)).is_ok());
        assert!(validate_range(0, i64::MAX, None).is_err());
    }
}
