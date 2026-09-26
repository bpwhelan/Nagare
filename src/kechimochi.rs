//! A one-way, restart-safe mirror of Nagare history into Kechimochi.
//!
//! Identity lives on the remote objects as well as in Nagare's database. Always
//! discover remote objects before writing: a POST can commit even when its
//! response is lost. Retrying that POST blindly would duplicate immersion time.
use crate::api::AppState;
use crate::config::{Config, KechimochiConfig, KechimochiSyncMode as SyncMode, MediaServerKind};
use crate::mining::{AppDatabase, load_watch_history_map, open_connection};
use crate::session::HistoryEntry;
use anyhow::{Context, bail};
use axum::{Json, extract::State, http::StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as TimeDelta, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::{Client, Method};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, RwLock};
use tracing::{info, warn};

pub fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS kechimochi_sync_state (
            destination TEXT PRIMARY KEY, state_json TEXT NOT NULL
        );",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO app_metadata(key, value) VALUES ('kechimochi_instance_id', ?1)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncReport {
    pub history_items: usize,
    pub media_only_items: usize,
    pub media_created: usize,
    pub media_updated: usize,
    pub media_deleted: usize,
    pub logs_created: usize,
    pub logs_updated: usize,
    pub logs_deleted: usize,
    pub unchanged_logs: usize,
    pub retained_media: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    pub last_started_at: Option<DateTime<Utc>>,
    pub last_finished_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub report: SyncReport,
}

#[derive(Clone, Serialize)]
struct SourceItem {
    #[serde(flatten)]
    history: HistoryEntry,
    mined_notes: u64,
}

impl AppDatabase {
    async fn kechimochi_query<T: Send + 'static>(
        &self,
        f: impl FnOnce(Connection) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let path = self.db_path.clone();
        tokio::task::spawn_blocking(move || f(open_connection(&path)?))
            .await
            .context("Kechimochi database task failed")?
    }

    async fn kechimochi_state(&self, destination: String) -> anyhow::Result<SyncState> {
        self.kechimochi_query(move |conn| {
            let saved: Option<String> = conn
                .query_row(
                    "SELECT state_json FROM kechimochi_sync_state WHERE destination = ?1",
                    [destination],
                    |row| row.get(0),
                )
                .optional()?;
            saved
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map(Option::unwrap_or_default)
                .map_err(Into::into)
        })
        .await
    }

    async fn save_kechimochi_state(
        &self,
        destination: String,
        state: SyncState,
    ) -> anyhow::Result<()> {
        self.kechimochi_query(move |conn| {
            conn.execute(
                "INSERT INTO kechimochi_sync_state(destination, state_json) VALUES (?1, ?2)
                 ON CONFLICT(destination) DO UPDATE SET state_json = excluded.state_json",
                params![destination, serde_json::to_string(&state)?],
            )?;
            Ok(())
        })
        .await
    }

    async fn kechimochi_source(&self) -> anyhow::Result<(String, Vec<SourceItem>)> {
        self.kechimochi_query(|mut conn| {
            // History and mining counts must describe the same local snapshot.
            let tx = conn.transaction()?;
            let instance = tx.query_row(
                "SELECT value FROM app_metadata WHERE key = 'kechimochi_instance_id'",
                [],
                |row| row.get(0),
            )?;
            let history = load_watch_history_map(&tx)?;
            let mut stmt =
                tx.prepare("SELECT history_id, COUNT(*) FROM mined_notes GROUP BY history_id")?;
            let counts: HashMap<String, u64> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let mut items: Vec<_> = history
                .into_values()
                .map(|history| SourceItem {
                    mined_notes: counts.get(&history.history_id).copied().unwrap_or_default(),
                    history,
                })
                .collect();
            items.sort_by(|a, b| a.history.history_id.cmp(&b.history.history_id));
            Ok((instance, items))
        })
        .await
    }
}

// Media and logs are compared to fresh remote data on every pass. No cached
// numeric ID is trusted after a restore, deletion, or destination change.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct RemoteMedia {
    id: Option<i64>,
    uid: Option<String>,
    title: String,
    variant: String,
    #[serde(alias = "media_type")]
    default_activity_type: String,
    status: String,
    language: String,
    description: String,
    cover_image: String,
    extra_data: String,
    content_type: String,
    tracking_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RemoteLog {
    id: Option<i64>,
    media_id: i64,
    duration_minutes: i64,
    characters: i64,
    date: String,
    activity_type: String,
    #[serde(default)]
    notes: String,
}

struct KechimochiClient {
    http: Client,
    base: String,
}

impl KechimochiClient {
    fn new(config: &KechimochiConfig) -> anyhow::Result<Self> {
        let mut config = config.clone();
        config.normalize_and_validate()?;
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("Nagare/", env!("CARGO_PKG_VERSION")))
                .build()?,
            base: config.api_url,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<reqwest::Response> {
        let mut request = self
            .http
            .request(method.clone(), format!("{}/api/{path}", self.base))
            .header("X-Kechimochi-API", "1");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.with_context(|| format!("Kechimochi {method} /api/{path} could not connect; check that the API is running and reachable from the Nagare server"))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!(
                "Kechimochi {method} /api/{path} returned {status}: {}",
                detail.chars().take(400).collect::<String>()
            );
        }
        Ok(response)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        self.request(Method::GET, path, None)
            .await?
            .json()
            .await
            .with_context(|| format!("Unexpected Kechimochi response from /api/{path}"))
    }

    async fn create(&self, path: &str, body: &impl Serialize) -> anyhow::Result<i64> {
        let id: i64 = self
            .request(Method::POST, path, Some(serde_json::to_value(body)?))
            .await?
            .json()
            .await
            .context(
                "Kechimochi did not return a numeric ID; the next sync will rediscover the object",
            )?;
        if id <= 0 {
            bail!("Kechimochi returned an invalid object ID");
        }
        Ok(id)
    }

    async fn update(&self, path: &str, body: &impl Serialize) -> anyhow::Result<()> {
        self.request(Method::PUT, path, Some(serde_json::to_value(body)?))
            .await?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.request(Method::DELETE, path, None).await?;
        Ok(())
    }
}

fn ownership(media: &RemoteMedia, instance: &str) -> Option<String> {
    let extra: Value = serde_json::from_str(&media.extra_data).ok()?;
    let owner = extra.get("nagare")?;
    (owner.get("instance_id")?.as_str()? == instance)
        .then(|| owner.get("group_key")?.as_str().map(str::to_string))
        .flatten()
}

fn log_prefix(instance: &str) -> String {
    format!("[nagare:{instance}:")
}

fn log_key(log: &RemoteLog, instance: &str) -> Option<String> {
    let first = log.notes.lines().next()?;
    let encoded = first
        .strip_prefix(&log_prefix(instance))?
        .strip_suffix(']')?;
    String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()
}

fn language_name(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "jpn" | "ja" | "japanese" => "Japanese".into(),
        "eng" | "en" | "english" => "English".into(),
        "kor" | "ko" | "korean" => "Korean".into(),
        "zho" | "chi" | "zh" | "chinese" => "Chinese".into(),
        "spa" | "es" | "spanish" => "Spanish".into(),
        "fra" | "fre" | "fr" | "french" => "French".into(),
        "deu" | "ger" | "de" | "german" => "German".into(),
        "" | "und" => "Unknown".into(),
        _ => language.trim().to_string(),
    }
}

struct Group {
    key: String,
    title: String,
    variant: String,
    activity: String,
    language: String,
    content_type: String,
    items: Vec<SourceItem>,
}

fn groups(items: Vec<SourceItem>, target_language: &str) -> BTreeMap<String, Group> {
    let mut result = BTreeMap::<String, Group>::new();
    for item in items {
        let history = &item.history;
        let title = history
            .series_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&history.title)
            .trim();
        let title = if title.is_empty() {
            &history.history_id
        } else {
            title
        };
        let target = language_name(target_language);
        let language = history
            .audio_languages
            .iter()
            .map(|s| language_name(s))
            .find(|s| s == &target)
            .or_else(|| history.audio_languages.first().map(|s| language_name(s)))
            .unwrap_or(target);
        let (activity, content_type) = if history.server_kind == MediaServerKind::Audiobookshelf {
            ("Listening", "Audiobook")
        } else if history
            .file_path
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains("anime")
        {
            ("Watching", "Anime")
        } else if history
            .series_name
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            ("Watching", "TV Series")
        } else {
            ("Watching", "Movie")
        };
        let key = json!([history.server_kind, title, activity, content_type, language]).to_string();
        result
            .entry(key.clone())
            .or_insert_with(|| Group {
                key,
                title: title.to_string(),
                variant: format!(
                    "Nagare · {} · {content_type} · {language}",
                    history.server_kind.display_name()
                ),
                activity: activity.into(),
                language,
                content_type: content_type.into(),
                items: Vec::new(),
            })
            .items
            .push(item);
    }
    result
}

fn desired_media(
    group: &Group,
    instance: &str,
    existing: Option<&RemoteMedia>,
) -> anyhow::Result<RemoteMedia> {
    let mut media = existing.cloned().unwrap_or_else(|| RemoteMedia {
        status: "Active".into(),
        tracking_status: "Ongoing".into(),
        ..RemoteMedia::default()
    });
    let mut extra = if media.extra_data.is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(&media.extra_data)
            .context("Media extra_data is not valid JSON")?
    };
    let object = extra
        .as_object_mut()
        .context("Media extra_data must be an object")?;
    object.insert(
        "nagare".into(),
        json!({
            "instance_id": instance, "group_key": group.key, "history": group.items,
        }),
    );
    media.title.clone_from(&group.title);
    media.variant.clone_from(&group.variant);
    media.default_activity_type.clone_from(&group.activity);
    media.language.clone_from(&group.language);
    media.content_type.clone_from(&group.content_type);
    media.extra_data = extra.to_string();
    Ok(media)
}

fn progress_ms(history: &HistoryEntry) -> i64 {
    history
        .last_position_ms
        .max(0)
        .min(history.duration_ms.filter(|d| *d > 0).unwrap_or(i64::MAX))
}

fn desired_log(
    item: &SourceItem,
    group: &Group,
    media_id: i64,
    instance: &str,
    timezone: Tz,
    existing: Option<&RemoteLog>,
) -> RemoteLog {
    let history = &item.history;
    let marker = format!(
        "{}{}]",
        log_prefix(instance),
        URL_SAFE_NO_PAD.encode(&history.history_id)
    );
    let end_marker = "\n[/nagare]";
    // Users can append their own notes after the managed block.
    let suffix = existing
        .and_then(|log| log.notes.split_once(end_marker).map(|(_, tail)| tail))
        .unwrap_or("");
    RemoteLog {
        id: existing.and_then(|log| log.id),
        media_id,
        duration_minutes: (progress_ms(history).saturating_add(30_000) / 60_000).max(1),
        characters: 0,
        date: history
            .last_seen
            .with_timezone(&timezone)
            .date_naive()
            .to_string(),
        activity_type: group.activity.clone(),
        notes: format!(
            "{marker}\n{}\nRecorded progress: {} seconds\nMined notes: {}\nSource: {}{end_marker}{suffix}",
            history.title,
            progress_ms(history) / 1000,
            item.mined_notes,
            history.server_kind.display_name()
        ),
    }
}

async fn reconcile_group(
    client: &KechimochiClient,
    group: &Group,
    source_groups: &HashMap<&str, &str>,
    instance: &str,
    timezone: Tz,
    media: &mut Vec<RemoteMedia>,
    logs: &mut Vec<RemoteLog>,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    let matches: Vec<_> = media
        .iter()
        .filter(|m| ownership(m, instance).as_deref() == Some(&group.key))
        .collect();
    if matches.len() > 1 {
        bail!("Multiple Kechimochi media entries claim this Nagare group");
    }
    let mut existing = matches.first().copied().cloned();
    if existing.is_none() {
        // A renamed series keeps its cover, UID, description, and milestones.
        // Reuse an old group only if every remaining source belongs here. A
        // split group must keep its identity for the entries that remain.
        let renamed: Vec<_> = media
            .iter()
            .filter(|m| {
                if ownership(m, instance).is_none() {
                    return false;
                }
                let Ok(extra) = serde_json::from_str::<Value>(&m.extra_data) else {
                    return false;
                };
                let Some(previous) = extra["nagare"]["history"].as_array() else {
                    return false;
                };
                let remaining: Vec<_> = previous
                    .iter()
                    .filter_map(|h| h["history_id"].as_str())
                    .filter_map(|id| source_groups.get(id))
                    .collect();
                !remaining.is_empty() && remaining.iter().all(|key| **key == group.key)
            })
            .collect();
        if renamed.len() == 1 {
            existing = Some(renamed[0].clone());
        }
    }
    let mut desired = desired_media(group, instance, existing.as_ref())?;
    let media_id = if let Some(existing) = existing.as_ref() {
        let id = existing.id.context("Kechimochi media is missing its ID")?;
        // Compare the JSON semantically so whitespace/key-order changes do not
        // make every scheduled check dirty Kechimochi's cloud-sync profile.
        let mut comparable = existing.clone();
        comparable.extra_data = serde_json::from_str::<Value>(&existing.extra_data)?.to_string();
        if comparable != desired {
            client.update(&format!("media/{id}"), &desired).await?;
            *media.iter_mut().find(|media| media.id == Some(id)).unwrap() = desired;
            report.media_updated += 1;
        }
        id
    } else {
        let id = client.create("media", &desired).await?;
        desired.id = Some(id);
        media.push(desired);
        report.media_created += 1;
        id
    };
    for item in &group.items {
        if let Err(error) = reconcile_log(
            client, item, group, media_id, instance, timezone, logs, report,
        )
        .await
        {
            if network_unavailable(&error) {
                return Err(error);
            }
            report
                .errors
                .push(format!("{}: {error:#}", item.history.title));
        }
    }
    Ok(())
}

fn network_unavailable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_timeout() || e.is_connect())
    })
}

async fn reconcile_log(
    client: &KechimochiClient,
    item: &SourceItem,
    group: &Group,
    media_id: i64,
    instance: &str,
    timezone: Tz,
    logs: &mut Vec<RemoteLog>,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    let matches: Vec<_> = logs
        .iter()
        .filter(|log| log_key(log, instance).as_deref() == Some(&item.history.history_id))
        .cloned()
        .collect();
    // Kechimochi rejects activities with both zero duration and characters.
    // Keep zero-progress history in the media metadata without inventing time.
    // If the source was reset to zero, remove only its formerly managed logs.
    if progress_ms(&item.history) == 0 {
        for log in matches {
            let id = log.id.context("Kechimochi log is missing its ID")?;
            client.delete(&format!("logs/{id}")).await?;
            logs.retain(|log| log.id != Some(id));
            report.logs_deleted += 1;
        }
        report.media_only_items += 1;
        return Ok(());
    }
    let existing = matches.first();
    let mut desired = desired_log(item, group, media_id, instance, timezone, existing);
    if let Some(existing) = existing {
        let id = existing.id.context("Kechimochi log is missing its ID")?;
        if &desired != existing {
            client.update(&format!("logs/{id}"), &desired).await?;
            *logs.iter_mut().find(|log| log.id == Some(id)).unwrap() = desired;
            report.logs_updated += 1;
        } else {
            report.unchanged_logs += 1;
        }
        // Recover duplicates carrying our exact ownership marker only.
        for duplicate in matches.iter().skip(1) {
            let id = duplicate.id.context("Duplicate log is missing its ID")?;
            client.delete(&format!("logs/{id}")).await?;
            logs.retain(|log| log.id != Some(id));
            report.logs_deleted += 1;
        }
    } else {
        desired.id = Some(client.create("logs", &desired).await?);
        logs.push(desired);
        report.logs_created += 1;
    }
    Ok(())
}

async fn cleanup(
    client: &KechimochiClient,
    groups: &BTreeMap<String, Group>,
    instance: &str,
    media: &[RemoteMedia],
    logs: &mut Vec<RemoteLog>,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    let source_ids: HashSet<_> = groups
        .values()
        .flat_map(|group| {
            group
                .items
                .iter()
                .map(|item| item.history.history_id.as_str())
        })
        .collect();
    let stale: Vec<_> = logs
        .iter()
        .filter(|log| log_key(log, instance).is_some_and(|id| !source_ids.contains(id.as_str())))
        .cloned()
        .collect();
    for log in stale {
        let id = log.id.context("Kechimochi log is missing its ID")?;
        client.delete(&format!("logs/{id}")).await?;
        logs.retain(|log| log.id != Some(id));
        report.logs_deleted += 1;
    }
    for item in media {
        let Some(key) = ownership(item, instance) else {
            continue;
        };
        if groups.contains_key(&key) {
            continue;
        }
        let id = item.id.context("Kechimochi media is missing its ID")?;
        // Deleting media cascades to logs/milestones. Keep an orphaned media
        // entry if the user has added their own activity or milestones to it.
        if logs.iter().any(|log| log.media_id == id) {
            report.retained_media += 1;
            continue;
        }
        // Someone may have added activity while this sync was writing other
        // groups. Refresh before a cascading delete rather than trusting the
        // snapshot taken at the beginning of a potentially long backfill.
        let current_logs: Vec<RemoteLog> = client.get(&format!("logs/media/{id}")).await?;
        if !current_logs.is_empty() {
            report.retained_media += 1;
            continue;
        }
        let Some(uid) = item.uid.as_deref() else {
            report.retained_media += 1;
            continue;
        };
        let mut encoded = reqwest::Url::parse("http://localhost")?;
        encoded.path_segments_mut().unwrap().push(uid);
        let uid = encoded.path().trim_start_matches('/');
        let milestones: Vec<Value> = client.get(&format!("media/{uid}/milestones")).await?;
        if !milestones.is_empty() {
            report.retained_media += 1;
            continue;
        }
        client.delete(&format!("media/{id}")).await?;
        report.media_deleted += 1;
    }
    Ok(())
}

// Choosing the earliest valid local instant handles both repeated and skipped
// daylight-saving hours. A missed run stays overdue across process restarts.
fn daily_instant(date: NaiveDate, config: &KechimochiConfig, timezone: Tz) -> DateTime<Utc> {
    let nominal = date
        .and_hms_opt(config.daily_hour, config.daily_minute, 0)
        .unwrap();
    for minute in 0..=1500 {
        if let Some(time) = timezone
            .from_local_datetime(&(nominal + TimeDelta::minutes(minute)))
            .earliest()
        {
            return time.with_timezone(&Utc);
        }
    }
    unreachable!("IANA time-zone gaps are at most one day")
}

fn next_run(
    config: &KechimochiConfig,
    state: &SyncState,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    if !config.enabled {
        return Ok(None);
    }
    let timezone = config.timezone.parse::<Tz>()?;
    let interrupted = state
        .last_started_at
        .is_some_and(|start| state.last_finished_at.is_none_or(|finish| start > finish));
    if interrupted {
        return Ok(Some(now));
    }
    if state.consecutive_failures > 0 {
        let seconds =
            (30_i64 * (1_i64 << state.consecutive_failures.saturating_sub(1).min(7))).min(3600);
        return Ok(Some(
            state.last_finished_at.unwrap_or(now) + TimeDelta::seconds(seconds),
        ));
    }
    let Some(last) = state.last_success_at else {
        return Ok(Some(now));
    };
    if config.sync_mode == SyncMode::Automatic {
        return Ok(Some(
            last + TimeDelta::minutes(config.interval_minutes.into()),
        ));
    }
    let date = last.with_timezone(&timezone).date_naive();
    let scheduled = daily_instant(date, config, timezone);
    Ok(Some(if scheduled > last {
        scheduled
    } else {
        daily_instant(
            date.succ_opt().context("Invalid schedule date")?,
            config,
            timezone,
        )
    }))
}

pub struct SyncService {
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
    gate: Arc<Mutex<()>>,
    notify: Notify,
}

impl SyncService {
    pub fn new(config: Arc<RwLock<Config>>, db: Arc<AppDatabase>) -> Self {
        Self {
            config,
            db,
            gate: Arc::new(Mutex::new(())),
            notify: Notify::new(),
        }
    }

    pub fn wake(&self) {
        self.notify.notify_one();
    }

    pub fn start(self: &Arc<Self>) -> bool {
        let Ok(guard) = self.gate.clone().try_lock_owned() else {
            return false;
        };
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(error) = service.execute(false).await {
                warn!("Kechimochi sync failed: {error:#}");
            }
        });
        true
    }

    async fn execute(&self, automatic: bool) -> anyhow::Result<SyncState> {
        let (mut settings, language) = {
            let config = self.config.read().await;
            (config.kechimochi.clone(), config.target_language.clone())
        };
        settings.normalize_and_validate()?;
        if automatic && !settings.enabled {
            bail!("Automatic Kechimochi sync is disabled");
        }
        let mut state = self.db.kechimochi_state(settings.api_url.clone()).await?;
        state.last_started_at = Some(Utc::now());
        self.db
            .save_kechimochi_state(settings.api_url.clone(), state.clone())
            .await?;
        let mut report = SyncReport::default();
        let result = self
            .reconcile(&settings, &language, automatic, &mut report)
            .await;
        if let Err(error) = &result {
            report.errors.push(format!("{error:#}"));
        }
        state.last_finished_at = Some(Utc::now());
        state.last_error = if report.errors.is_empty() {
            None
        } else {
            Some(report.errors.join("; "))
        };
        if state.last_error.is_none() {
            state.last_success_at = state.last_finished_at;
            state.consecutive_failures = 0;
        } else {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        }
        state.report = report;
        self.db
            .save_kechimochi_state(settings.api_url, state.clone())
            .await?;
        if let Some(error) = &state.last_error {
            warn!("Kechimochi sync will retry: {error}");
        } else {
            info!(
                "Kechimochi sync reconciled {} history items",
                state.report.history_items
            );
        }
        Ok(state)
    }

    async fn reconcile(
        &self,
        settings: &KechimochiConfig,
        language: &str,
        automatic: bool,
        report: &mut SyncReport,
    ) -> anyhow::Result<()> {
        let client = KechimochiClient::new(settings)?;
        let (instance, items) = self.db.kechimochi_source().await?;
        report.history_items = items.len();
        let groups = groups(items, language);
        let source_groups: HashMap<_, _> = groups
            .values()
            .flat_map(|group| {
                group
                    .items
                    .iter()
                    .map(|item| (item.history.history_id.as_str(), group.key.as_str()))
            })
            .collect();
        let timezone = settings.timezone.parse::<Tz>()?;
        let mut media: Vec<RemoteMedia> = client.get("media").await?;
        let mut logs: Vec<RemoteLog> = client.get("logs").await?;
        for group in groups.values() {
            self.check_settings(settings, automatic).await?;
            if let Err(error) = reconcile_group(
                &client,
                group,
                &source_groups,
                &instance,
                timezone,
                &mut media,
                &mut logs,
                report,
            )
            .await
            {
                report.errors.push(format!("{}: {error:#}", group.title));
                // A network outage should not spend 30 seconds on every item.
                if network_unavailable(&error) {
                    break;
                }
            }
        }
        // Never clean up after an incomplete source read or failed write pass.
        if report.errors.is_empty() {
            self.check_settings(settings, automatic).await?;
            cleanup(&client, &groups, &instance, &media, &mut logs, report).await?;
        }
        Ok(())
    }

    async fn check_settings(
        &self,
        expected: &KechimochiConfig,
        automatic: bool,
    ) -> anyhow::Result<()> {
        let mut current = self.config.read().await.kechimochi.clone();
        current.normalize_and_validate()?;
        if current != *expected || (automatic && !current.enabled) {
            bail!("Kechimochi settings changed during sync; retry with the saved settings");
        }
        Ok(())
    }

    pub async fn run_scheduler(self: Arc<Self>) {
        loop {
            if let Err(error) = self.scheduled_check().await {
                warn!("Kechimochi scheduler: {error:#}");
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(10)) => {},
                _ = self.notify.notified() => {},
            }
        }
    }

    async fn scheduled_check(&self) -> anyhow::Result<()> {
        let Ok(_guard) = self.gate.try_lock() else {
            return Ok(());
        };
        let mut settings = self.config.read().await.kechimochi.clone();
        if !settings.enabled {
            return Ok(());
        }
        settings.normalize_and_validate()?;
        let state = self.db.kechimochi_state(settings.api_url.clone()).await?;
        let now = Utc::now();
        if next_run(&settings, &state, now)?.is_some_and(|due| due <= now) {
            self.execute(true).await?;
        }
        Ok(())
    }
}

pub async fn get_status(State(app): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let service = &app.kechimochi_sync;
    let result = async {
        let mut config = app.config.read().await.kechimochi.clone();
        config.normalize_and_validate()?;
        let state = app.db.kechimochi_state(config.api_url.clone()).await?;
        let running = service.gate.try_lock().is_err();
        let due = if running { None } else { next_run(&config, &state, Utc::now())? };
        anyhow::Ok(json!({"ok": true, "enabled": config.enabled, "running": running, "next_run_at": due, "state": state}))
    }.await;
    api_result(result)
}

pub async fn test_connection(State(app): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = app.config.read().await.kechimochi.clone();
    let result = async {
        let client = KechimochiClient::new(&config)?;
        let version: String = client.get("version").await?;
        let media: Vec<RemoteMedia> = client.get("media").await?;
        let logs: Vec<RemoteLog> = client.get("logs").await?;
        anyhow::Ok(json!({"ok": true, "version": version, "media_count": media.len(), "log_count": logs.len()}))
    }.await;
    api_result(result)
}

pub async fn sync_now(State(app): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // Flush first so a manual request sees the last few seconds of playback.
    app.session_manager.flush_history().await;
    let started = app.kechimochi_sync.start();
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "started": started, "running": true})),
    )
}

fn api_result(result: anyhow::Result<Value>) -> (StatusCode, Json<Value>) {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)),
        Err(error) => (
            StatusCode::OK,
            Json(json!({"ok": false, "error": format!("{error:#}")})),
        ),
    }
}

#[cfg(test)]
mod tests;
