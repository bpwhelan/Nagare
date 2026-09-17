use super::{Workspace, db, media_source, probe_audio};
use crate::{
    anki::{AnkiClient, NewCardEvent, NoteField},
    api::AppState,
    config::MiningConfig,
    media,
    mining::{EnrichmentDialogState, EnrichmentSource, MiningHistoryEntry},
    session::HistoryEntry,
    subtitle::SubtitleTrack,
};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

pub const MODEL: &str = "Nagare Vocabulary";
pub static RUN_LOCK: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fields {
    pub word: String,
    pub reading: String,
    pub meaning: String,
    pub sentence: String,
    pub audio: String,
    pub picture: String,
    pub source: String,
}
impl Default for Fields {
    fn default() -> Self {
        Self {
            word: "Word".into(),
            reading: "Reading".into(),
            meaning: "Meaning".into(),
            sentence: "Sentence".into(),
            audio: "SentenceAudio".into(),
            picture: "Picture".into(),
            source: "Source".into(),
        }
    }
}
impl Fields {
    pub fn names(&self) -> [&str; 7] {
        [
            &self.word,
            &self.reading,
            &self.meaning,
            &self.sentence,
            &self.audio,
            &self.picture,
            &self.source,
        ]
    }
    fn validate(&self, available: &[String]) -> anyhow::Result<()> {
        if self.word.is_empty() || self.sentence.is_empty() {
            bail!("Map the word and sentence fields");
        }
        let mut used = HashSet::new();
        for name in self.names().into_iter().filter(|n| !n.is_empty()) {
            if !available.iter().any(|s| s == name) {
                bail!("Note type has no field named {name}");
            }
            if !used.insert(name) {
                bail!("Map each value to a different Anki field ({name} is used twice)");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub deck: String,
    pub model: String,
    pub fields: Fields,
    #[serde(default)]
    pub tags: Vec<String>,
    pub audio_ordinal: Option<usize>,
    #[serde(default)]
    pub screenshot: bool,
    #[serde(default)]
    pub animated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub term: String,
    pub reading: String,
    pub meaning: String,
    pub sentence: String,
    pub first: usize,
    pub last: usize,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JobCard {
    pub draft: Draft,
    pub status: String,
    pub note_id: Option<i64>,
    pub error: Option<String>,
    pub recorded: bool,
    pub event: Option<NewCardEvent>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub created_at: String,
    pub history: HistoryEntry,
    pub track: SubtitleTrack,
    pub settings: Settings,
    pub mining_config: MiningConfig,
    pub anki_url: String,
    pub cards: Vec<JobCard>,
}

impl Job {
    pub fn view(&self) -> Value {
        json!({"id":self.id,"status":self.status,"created_at":self.created_at,"title":self.history.title,
            "history_id":self.history.history_id,"settings":self.settings,
            "cards":self.cards.iter().map(|c| json!({"term":c.draft.term,"status":c.status,"note_id":c.note_id,"error":c.error})).collect::<Vec<_>>()})
    }
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('\n', "<br>")
}

pub fn plain_word(text: &str) -> String {
    static RUBY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<rt\b[^>]*>.*?</rt>").unwrap());
    static TAGS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"<[^>]*>|\[[^\]]*\]").unwrap());
    TAGS.replace_all(&RUBY.replace_all(text, ""), "")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

pub fn quote_search(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

pub async fn words_in_anki(
    client: &AnkiClient,
    model: &str,
    field: &str,
) -> anyhow::Result<HashSet<String>> {
    let ids = client
        .find_notes(&format!("note:{}", quote_search(model)))
        .await?;
    let mut words = HashSet::new();
    for chunk in ids.chunks(250) {
        for note in client.notes_info(chunk).await? {
            if let Some(value) = note.fields.get(field) {
                let word = plain_word(&value.value);
                if !word.is_empty() {
                    words.insert(word);
                }
            }
        }
    }
    Ok(words)
}

pub fn validate_drafts(workspace: &Workspace, drafts: &[Draft]) -> anyhow::Result<()> {
    if drafts.is_empty() || drafts.len() > 500 {
        bail!("Select between 1 and 500 words per batch");
    }
    let mut seen = HashSet::new();
    for draft in drafts {
        let term = draft.term.trim();
        if term.is_empty() || term.len() > 200 || !seen.insert(term) {
            bail!("Each selected word must be unique and nonempty");
        }
        let candidate = workspace
            .candidates
            .iter()
            .find(|c| c.term == term)
            .context("A selected word no longer exists; analyze the subtitles again")?;
        if draft.first > draft.last
            || draft.last >= workspace.track.lines.len()
            || !candidate
                .occurrences
                .iter()
                .any(|i| *i >= draft.first && *i <= draft.last)
        {
            bail!("Choose subtitle context containing {term}");
        }
        super::validate_range(draft.start_ms, draft.end_ms, workspace.history.duration_ms)?;
        if draft.sentence.trim().is_empty()
            || draft.sentence.len() > 20_000
            || draft.meaning.len() > 20_000
            || draft.reading.len() > 500
        {
            bail!("The sentence must be nonempty and card text must be under 20,000 bytes");
        }
    }
    Ok(())
}

pub async fn validate_settings(
    client: &AnkiClient,
    settings: &Settings,
    create_model: bool,
) -> anyhow::Result<()> {
    let decks: Vec<String> = serde_json::from_value(client.invoke("deckNames", json!({})).await?)?;
    if !decks.contains(&settings.deck) {
        bail!("Choose an existing Anki deck");
    }
    let models: Vec<String> =
        serde_json::from_value(client.invoke("modelNames", json!({})).await?)?;
    if !models.contains(&settings.model) {
        if settings.model != MODEL {
            bail!("Choose an existing Anki note type");
        }
        if create_model {
            client.invoke("createModel", json!({"modelName":MODEL,"inOrderFields":Fields::default().names(),
                "css":".card{font-family:Arial,sans-serif;font-size:22px;text-align:center;line-height:1.7;padding:20px;color:#222;background:#fff}.nightMode.card{color:#eee;background:#222}.word{font-size:42px}.reading{color:#888}.meaning{font-size:18px;white-space:normal}.source{font-size:12px;color:#888}img{max-width:100%;max-height:360px}",
                "cardTemplates":[{"Name":"Vocabulary","Front":"<div class=\"word\">{{Word}}</div><div>{{Sentence}}</div>",
                "Back":"{{FrontSide}}<hr id=answer><div class=\"reading\">{{Reading}}</div><div class=\"meaning\">{{Meaning}}</div><div>{{SentenceAudio}}</div><div>{{Picture}}</div><div class=\"source\">{{Source}}</div>"}]})).await?;
        }
    }
    let available: Vec<String> =
        if settings.model == MODEL && !models.contains(&settings.model) && !create_model {
            Fields::default().names().map(String::from).to_vec()
        } else {
            serde_json::from_value(
                client
                    .invoke("modelFieldNames", json!({"modelName":settings.model}))
                    .await?,
            )?
        };
    settings.fields.validate(&available)?;
    if settings.tags.len() > 30
        || settings
            .tags
            .iter()
            .any(|tag| tag.len() > 150 || tag.chars().any(char::is_whitespace))
    {
        bail!("Use up to 30 tags, separated by spaces");
    }
    Ok(())
}

async fn record_card(state: &Arc<AppState>, job: &Job, index: usize) -> anyhow::Result<()> {
    let card = &job.cards[index];
    let event = card.event.clone().context("Missing saved note fields")?;
    let note_id = card.note_id.context("Missing note ID")?;
    let draft = &card.draft;
    let dialog = EnrichmentDialogState {
        event: event.clone(),
        matched_text: Some(draft.sentence.clone()),
        history_id: Some(job.history.history_id.clone()),
        matched_line_index: Some(draft.first),
        start_ms: Some(draft.start_ms),
        end_ms: Some(draft.end_ms),
        generate_avif: Some(job.settings.animated),
        included_line_first: Some(draft.first),
        included_line_last: Some(draft.last),
        card_ids: vec![],
        source: EnrichmentSource::MiningHistory,
        updated_at: Some(chrono::Utc::now()),
    };
    db::save_note_fields(
        state.db.db_path.clone(),
        note_id,
        job.settings.fields.clone(),
    )
    .await?;
    state
        .db
        .record_review_card(
            job.id.clone(),
            job.history.title.clone(),
            job.track.clone(),
            dialog,
            (None, job.settings.audio_ordinal),
        )
        .await?;
    let now = chrono::Utc::now();
    state
        .db
        .upsert_mined_note(MiningHistoryEntry {
            note_id,
            card_ids: vec![],
            history_id: job.history.history_id.clone(),
            server_kind: job.history.server_kind,
            item_id: job.history.item_id.clone(),
            media_source_id: job.history.media_source_id.clone(),
            file_path: job.history.file_path.clone(),
            title: job.history.title.clone(),
            event,
            start_ms: draft.start_ms,
            end_ms: draft.end_ms,
            generate_avif: job.settings.animated,
            matched_line_index: Some(draft.first),
            included_line_first: Some(draft.first),
            included_line_last: Some(draft.last),
            created_at: now,
            updated_at: now,
        })
        .await?;
    state
        .db
        .update_review_card(note_id, "enhanced", None, None)
        .await?;
    db::set_known(
        state.db.db_path.clone(),
        vec![draft.term.clone()],
        "mined".into(),
    )
    .await?;
    Ok(())
}

async fn create_card(
    state: &Arc<AppState>,
    client: &AnkiClient,
    job: &Job,
    index: usize,
    existing: &mut HashSet<String>,
) -> anyhow::Result<(Option<NewCardEvent>, bool)> {
    let draft = &job.cards[index].draft;
    let marker = format!("nagare::word_miner::{}::{index}", job.id);
    // Recover a note that committed before a lost response or process exit. Never retry addNote blindly.
    let found = client.find_notes(&format!("tag:{marker}")).await?;
    if let Some(id) = found.first() {
        let note = client
            .notes_info(&[*id])
            .await?
            .into_iter()
            .next()
            .context("The recovered Anki note is missing")?;
        return Ok((
            Some(crate::anki::note_info_to_event(
                note,
                &job.settings.fields.sentence,
            )),
            true,
        ));
    }
    if existing.contains(&draft.term) {
        return Ok((None, false));
    }
    let config = state.config.read().await.clone();
    let source = media_source(state, &job.history, &config).await?;
    let mapping = &job.settings.fields;
    let mut fields = HashMap::new();
    for (field, value) in [
        (&mapping.word, &draft.term),
        (&mapping.reading, &draft.reading),
        (&mapping.meaning, &draft.meaning),
        (&mapping.sentence, &draft.sentence),
        (&mapping.source, &job.history.title),
    ] {
        if !field.is_empty() {
            fields.insert(field.clone(), escape(value));
        }
    }
    if !mapping.meaning.is_empty() && !draft.meaning.trim().is_empty() {
        fields.entry(mapping.meaning.clone()).and_modify(|value| value.push_str(
            "<br><small><a href=\"https://www.edrdg.org/wiki/index.php/JMdict-EDICT_Dictionary_Project\">JMdict</a> · © EDRDG · <a href=\"https://creativecommons.org/licenses/by-sa/4.0/\">CC BY-SA 4.0</a></small>"));
    }
    let prefix = format!("nagare_miner_{}_{index}", job.id);
    if !mapping.audio.is_empty() {
        let codec = job.mining_config.audio_codec;
        let (path, data) = media::extract_audio(
            &source,
            draft.start_ms,
            draft.end_ms,
            None,
            job.settings.audio_ordinal,
            codec,
        )
        .await?;
        media::cleanup_temp_file(&path).await;
        let filename = format!("{prefix}.{}", codec.extension());
        client
            .store_media_file(&filename, &media::to_base64(&data))
            .await?;
        fields.insert(mapping.audio.clone(), format!("[sound:{filename}]"));
    }
    if job.settings.screenshot
        && !mapping.picture.is_empty()
        && media::probe_media(&source, draft.start_ms)
            .await?
            .has_visual_video
    {
        let (path, data, ext) = if job.settings.animated {
            let (path, data) =
                media::generate_avif(&source, draft.start_ms, draft.end_ms, &job.mining_config)
                    .await?;
            (path, data, "avif".to_string())
        } else {
            let image = media::generate_screenshot(
                &source,
                draft.start_ms + (draft.end_ms - draft.start_ms) / 2,
                job.mining_config.static_screenshot_format,
            )
            .await?;
            (image.path, image.data, image.format.extension().to_string())
        };
        media::cleanup_temp_file(&path).await;
        let filename = format!("{prefix}_picture.{ext}");
        client
            .store_media_file(&filename, &media::to_base64(&data))
            .await?;
        fields.insert(mapping.picture.clone(), format!("<img src=\"{filename}\">"));
    }
    let mut tags = job.settings.tags.clone();
    tags.push("nagare::word_miner".into());
    tags.push(marker);
    let result=client.invoke("addNote",json!({"note":{"deckName":job.settings.deck,"modelName":job.settings.model,
        "fields":fields,"tags":tags,"options":{"allowDuplicate":false,"duplicateScope":"collection"}}})).await?;
    let id = result.as_i64().context(
        "Anki did not return a note ID; resume this batch to check whether the card was saved",
    )?;
    existing.insert(draft.term.clone());
    Ok((
        Some(NewCardEvent {
            note_id: id,
            sentence: fields.get(&mapping.sentence).cloned().unwrap_or_default(),
            model_name: job.settings.model.clone(),
            tags,
            fields: fields
                .into_iter()
                .enumerate()
                .map(|(order, (name, value))| {
                    (
                        name,
                        NoteField {
                            value,
                            order: order as i32,
                        },
                    )
                })
                .collect(),
        }),
        false,
    ))
}

pub fn start(state: Arc<AppState>, mut job: Job, guard: OwnedMutexGuard<()>) {
    tokio::spawn(async move {
        let _guard = guard;
        let result = run(&state, &mut job).await;
        if let Err(error) = result {
            tracing::error!("Word mining batch {} paused: {error:#}", job.id);
            job.status = "needs_attention".into();
            if let Some(card) = job
                .cards
                .iter_mut()
                .find(|c| !matches!(c.status.as_str(), "created" | "skipped"))
            {
                card.status = "failed".into();
                card.error = Some(format!("{error:#}"));
            }
        }
        if let Err(error) = db::save_job(state.db.db_path.clone(), job).await {
            tracing::error!("Could not save mining progress: {error}");
        }
    });
}

pub async fn resolve_audio(
    state: &AppState,
    history: &HistoryEntry,
    config: &crate::config::Config,
    settings: &mut Settings,
) -> anyhow::Result<()> {
    if settings.fields.audio.is_empty() {
        return Ok(());
    }
    let source = media_source(state, history, config).await?;
    let tracks = probe_audio(&source).await?;
    if tracks.is_empty() {
        bail!("This file has no audio track. Omit the audio field to create text cards.");
    }
    let preferred = tracks.iter().find(|t| {
        crate::session::SessionManager::language_matches_target(
            t.language.as_deref(),
            &config.target_language,
        )
    });
    settings.audio_ordinal = settings.audio_ordinal.or_else(|| {
        preferred
            .or_else(|| (tracks.len() == 1).then(|| &tracks[0]))
            .map(|t| t.ordinal)
    });
    if settings
        .audio_ordinal
        .is_some_and(|ordinal| ordinal >= tracks.len())
    {
        bail!("The selected audio track is no longer available");
    }
    if settings.audio_ordinal.is_none() {
        bail!(
            "This file has multiple audio tracks without a target-language match. Choose an audio track in Anki & audio settings."
        );
    }
    Ok(())
}

async fn run(state: &Arc<AppState>, job: &mut Job) -> anyhow::Result<()> {
    let config = state.config.read().await.clone();
    if config.anki.url != job.anki_url {
        bail!(
            "AnkiConnect address changed. Restore the original address before resuming this batch."
        );
    }
    let client = state.anki_client.read().await.clone();
    validate_settings(&client, &job.settings, true).await?;
    // Recheck the saved choice on resume; it remains attached to the source snapshot.
    resolve_audio(state, &job.history, &config, &mut job.settings).await?;
    db::save_job(state.db.db_path.clone(), job.clone()).await?;
    let mut existing =
        words_in_anki(&client, &job.settings.model, &job.settings.fields.word).await?;
    for index in 0..job.cards.len() {
        if db::cancelled(state.db.db_path.clone(), job.id.clone()).await? {
            job.status = "paused".into();
            return Ok(());
        }
        if job.cards[index].status == "skipped"
            || (job.cards[index].status == "created" && job.cards[index].recorded)
        {
            continue;
        }
        if job.cards[index].status != "created" {
            job.cards[index].status = "creating".into();
            job.cards[index].error = None;
            db::save_job(state.db.db_path.clone(), job.clone()).await?;
            let result = tokio::time::timeout(
                Duration::from_secs(180),
                create_card(state, &client, job, index, &mut existing),
            )
            .await;
            match result {
                Ok(Ok((Some(event), _))) => {
                    job.cards[index].note_id = Some(event.note_id);
                    job.cards[index].event = Some(event);
                    job.cards[index].status = "created".into();
                }
                Ok(Ok((None, _))) => {
                    job.cards[index].status = "skipped".into();
                    job.cards[index].error =
                        Some("This word is already in this Anki note type.".into());
                }
                result => {
                    let error = match result {
                        Ok(Err(e)) => format!("{e:#}"),
                        Err(_) => {
                            "Timed out. Resume to check Anki before retrying this word.".into()
                        }
                        _ => unreachable!(),
                    };
                    job.cards[index].status = "failed".into();
                    job.cards[index].error = Some(error);
                }
            }
            // Persist the returned ID before bookkeeping. A failed save stops the batch.
            db::save_job(state.db.db_path.clone(), job.clone()).await?;
        }
        if job.cards[index].status == "created" {
            match record_card(state, job, index).await {
                Ok(()) => {
                    job.cards[index].recorded = true;
                    job.cards[index].error = None;
                }
                Err(error) => {
                    job.cards[index].error = Some(format!(
                        "Card saved in Anki. Resume to finish saving review history: {error}"
                    ));
                }
            }
            db::save_job(state.db.db_path.clone(), job.clone()).await?;
        }
    }
    job.status = if job
        .cards
        .iter()
        .any(|c| c.status == "failed" || (c.status == "created" && !c.recorded))
    {
        "needs_attention"
    } else {
        "complete"
    }
    .into();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_mapping_rejects_collisions_and_missing_fields() {
        let mut fields = Fields::default();
        let names = fields.names().map(String::from);
        assert!(fields.validate(&names).is_ok());
        fields.word = "Sentence".into();
        assert!(fields.validate(&names).is_err());
        fields.word = "Missing".into();
        assert!(fields.validate(&names).is_err());
    }
    #[test]
    fn html_and_anki_search_are_escaped() {
        assert_eq!(escape("<script> & \""), "&lt;script&gt; &amp; &quot;");
        assert_eq!(plain_word("<ruby>猫<rt>ねこ</rt></ruby>"), "猫");
        assert_eq!(plain_word("猫[ねこ]"), "猫");
        assert_eq!(quote_search("Mining \"deck\""), "\"Mining \\\"deck\\\"\"");
    }
}
