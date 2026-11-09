use crate::config::{Config, TadokuConfig, TadokuSyncMode};
use crate::mining::{AppDatabase, TadokuExportBatch};
use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use reqwest::cookie::{CookieStore, Jar};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

const TADOKU_AUTH_BASE_URL: &str = "https://tadoku.app/api/internal/identity";
const AUTOMATIC_EXPORT_INTERVAL: Duration = Duration::from_secs(5 * 60);
static EXPORT_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Serialize)]
pub struct TadokuConnectionInfo {
    pub user_id: String,
    pub display_name: Option<String>,
    pub listening_activity_id: i32,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    account: Option<SessionAccount>,
}

#[derive(Debug, Deserialize)]
struct SessionAccount {
    id: String,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigurationOptions {
    activities: Vec<Activity>,
    languages: Vec<Language>,
}

#[derive(Debug, Deserialize)]
struct Activity {
    id: i32,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Language {
    code: String,
}

#[derive(Debug)]
struct TadokuConnection {
    info: TadokuConnectionInfo,
}

#[derive(Debug, Default, Deserialize)]
struct RegistrationsResponse {
    #[serde(default)]
    registrations: Vec<Registration>,
}

#[derive(Debug, Deserialize)]
struct Registration {
    id: String,
    #[serde(default)]
    languages: Vec<Language>,
    contest: Option<RegistrationContest>,
}

#[derive(Debug, Deserialize)]
struct RegistrationContest {
    #[serde(default)]
    allowed_activities: Vec<Activity>,
}

#[derive(Debug, Default, Deserialize)]
struct LogsResponse {
    #[serde(default)]
    logs: Vec<TadokuLog>,
    #[serde(default)]
    total_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct TadokuLog {
    id: String,
    description: Option<String>,
    amount: Option<f64>,
    unit_key: Option<String>,
    duration_seconds: Option<i32>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CreateLogRequest<'a> {
    registration_ids: Vec<String>,
    language_code: &'a str,
    activity_id: i32,
    duration_seconds: i32,
    tags: Vec<String>,
    description: &'a str,
}

#[derive(Debug, Serialize)]
struct UpdateLogRequest<'a> {
    duration_seconds: i32,
    tags: Vec<String>,
    description: &'a str,
}

#[derive(Clone)]
struct TadokuPersistence {
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
}

impl TadokuPersistence {
    async fn save_cookie(&self, cookie: &str) -> anyhow::Result<()> {
        let mut config = self.config.write().await;
        let previous = config.tadoku.clone();
        // Save the endpoint migration with the new cookie, otherwise the next
        // client would mistake it for a legacy session and sign in again.
        config.tadoku.normalize();
        config.tadoku.session_cookie = cookie.to_string();
        if let Err(error) = self.db.save_config(config.clone()).await {
            config.tadoku = previous;
            return Err(error).context("Could not save the refreshed Tadoku session");
        }
        Ok(())
    }
}

struct TadokuClient {
    http: reqwest::Client,
    cookies: Arc<Jar>,
    config: TadokuConfig,
    auth_url: String,
    persistence: Option<TadokuPersistence>,
}

impl TadokuClient {
    fn new(
        config: TadokuConfig,
        persistence: Option<TadokuPersistence>,
        seed_saved_cookie: bool,
    ) -> anyhow::Result<Self> {
        Self::new_with_auth_url(config, persistence, seed_saved_cookie, TADOKU_AUTH_BASE_URL)
    }

    fn new_with_auth_url(
        mut config: TadokuConfig,
        persistence: Option<TadokuPersistence>,
        seed_saved_cookie: bool,
        auth_url: &str,
    ) -> anyhow::Result<Self> {
        config.normalize();
        if !config.has_credentials() {
            bail!("Tadoku email and password are not configured");
        }
        if config.api_url.trim().is_empty() {
            bail!("Tadoku API URL is empty");
        }
        if config.session_url.trim().is_empty() {
            bail!("Tadoku session URL is empty");
        }
        let cookies = Arc::new(Jar::default());
        if seed_saved_cookie && !config.session_cookie.trim().is_empty() {
            let cookie = extract_cookie_value(&config.session_cookie)?;
            add_session_cookie(&cookies, auth_url, &cookie)?;
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Nagare/", env!("CARGO_PKG_VERSION")))
            .cookie_provider(cookies.clone())
            .build()?;
        Ok(Self {
            http,
            cookies,
            config,
            auth_url: auth_url.trim_end_matches('/').to_string(),
            persistence,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.api_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn cookie_urls(&self) -> anyhow::Result<[reqwest::Url; 2]> {
        Ok([
            reqwest::Url::parse(&self.auth_url).context("Invalid Tadoku auth URL")?,
            reqwest::Url::parse(&self.config.api_url).context("Invalid Tadoku API URL")?,
        ])
    }

    fn session_cookie(&self) -> anyhow::Result<Option<String>> {
        for url in self.cookie_urls()? {
            let Some(header) = self.cookies.cookies(&url) else {
                continue;
            };
            let value = header
                .to_str()
                .context("Tadoku returned an invalid session cookie")?;
            if let Some(cookie) = cookie_from_header(value) {
                return Ok(Some(cookie));
            }
        }
        Ok(None)
    }

    fn clear_session_cookie(&self) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(&self.auth_url).context("Invalid Tadoku auth URL")?;
        let expired = if is_tadoku_production_host(&url) {
            "tadoku_session=; Max-Age=0; Path=/; Domain=.tadoku.app; Secure"
        } else {
            "tadoku_session=; Max-Age=0; Path=/"
        };
        self.cookies.add_cookie_str(expired, &url);
        Ok(())
    }

    async fn login(&mut self) -> anyhow::Result<()> {
        let origin = reqwest::Url::parse(&self.auth_url)
            .context("Invalid Tadoku auth URL")?
            .origin()
            .ascii_serialization();
        let login_response = self
            .http
            .post(format!("{}/login", self.auth_url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, origin)
            .json(&serde_json::json!({
                "email": self.config.username,
                "password": self.config.password,
            }))
            .send()
            .await
            .context("Could not complete Tadoku login")?;
        match login_response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::UNAUTHORIZED => {
                bail!(
                    "Tadoku login failed; check the saved email and password (Tadoku now requires your email address)"
                );
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                bail!("Tadoku sign-in is rate limited; wait 15 minutes before trying again");
            }
            status => bail!("Tadoku login failed (HTTP {status})"),
        }
        let cookie = self
            .session_cookie()?
            .context("Tadoku login did not return a tadoku_session cookie")?;
        if let Some(persistence) = &self.persistence {
            persistence.save_cookie(&cookie).await?;
        }
        self.config.session_cookie = cookie;
        Ok(())
    }

    async fn ensure_authenticated(&mut self) -> anyhow::Result<()> {
        if self.session_cookie()?.is_none() {
            self.login().await?;
        }
        Ok(())
    }

    async fn send_authenticated<F>(&mut self, request: F) -> anyhow::Result<reqwest::Response>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        self.ensure_authenticated().await?;
        let response = request(&self.http)
            .send()
            .await
            .context("Tadoku request failed")?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        self.clear_session_cookie()?;
        self.login().await?;
        request(&self.http)
            .send()
            .await
            .context("Tadoku request failed after refreshing the login")
    }

    async fn refresh_session(&mut self) -> anyhow::Result<()> {
        self.clear_session_cookie()?;
        self.login().await
    }

    async fn session_account(&self) -> anyhow::Result<Option<SessionAccount>> {
        let response = self
            .http
            .get(&self.config.session_url)
            .send()
            .await
            .context("Could not check Tadoku session")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        let session: SessionResponse = response_json(response, "Tadoku authentication").await?;
        Ok(session.account)
    }

    async fn connection_info(&mut self, language_code: &str) -> anyhow::Result<TadokuConnection> {
        self.ensure_authenticated().await?;
        // Unlike protected API calls, an expired session returns HTTP 200 with
        // account: null. Refresh once for either form of an unauthenticated reply.
        let account = match self.session_account().await? {
            Some(account) => account,
            None => {
                self.refresh_session().await?;
                self.session_account().await?.context(
                    "Tadoku did not return a signed-in account after refreshing the login",
                )?
            }
        };

        let options_url = self.api_url("logs/configuration-options");
        let options: ConfigurationOptions = response_json(
            self.send_authenticated(|http| http.get(&options_url))
                .await?,
            "Tadoku log configuration",
        )
        .await?;

        if !options
            .languages
            .iter()
            .any(|language| language.code.eq_ignore_ascii_case(language_code))
        {
            bail!("Tadoku does not offer language code '{language_code}'");
        }
        let activity = options
            .activities
            .iter()
            .find(|activity| activity.name.eq_ignore_ascii_case("listening"))
            .context("Tadoku did not return a Listening activity")?;
        Ok(TadokuConnection {
            info: TadokuConnectionInfo {
                user_id: account.id,
                display_name: account.display_name,
                listening_activity_id: activity.id,
            },
        })
    }

    async fn registrations(&mut self) -> anyhow::Result<Vec<Registration>> {
        let url = self.api_url("contests/ongoing-registrations");
        let response: RegistrationsResponse = response_json(
            self.send_authenticated(|http| http.get(&url)).await?,
            "Tadoku contest registrations",
        )
        .await?;
        Ok(response.registrations)
    }

    async fn existing_nagare_logs(
        &mut self,
        user_id: &str,
    ) -> anyhow::Result<HashMap<String, TadokuLog>> {
        let mut result = HashMap::new();
        let mut page = 0usize;
        loop {
            let url = self.api_url(&format!("users/{user_id}/logs"));
            let page_string = page.to_string();
            let response: LogsResponse = response_json(
                self.send_authenticated(|http| {
                    http.get(&url)
                        .query(&[("page_size", "50"), ("page", page_string.as_str())])
                })
                .await?,
                "Tadoku log history",
            )
            .await?;
            let returned = response.logs.len();
            for log in response.logs {
                let from_nagare = log
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case("nagare"));
                if from_nagare {
                    result.insert(log.id.clone(), log);
                }
            }
            page += 1;
            if returned == 0 || page.saturating_mul(50) >= response.total_size {
                break;
            }
        }
        Ok(result)
    }

    async fn create_log(
        &mut self,
        batch: &TadokuExportBatch,
        listening_activity_id: i32,
        registrations: &[Registration],
    ) -> anyhow::Result<String> {
        let registration_ids =
            eligible_registration_ids(registrations, &batch.language_code, listening_activity_id);
        let payload = CreateLogRequest {
            registration_ids,
            language_code: &batch.language_code,
            activity_id: listening_activity_id,
            duration_seconds: batch.duration_seconds,
            tags: tadoku_tags(&self.config, batch),
            description: &batch.description,
        };
        let url = self.api_url("logs");
        let created: TadokuLog = response_json(
            self.send_authenticated(|http| http.post(&url).json(&payload))
                .await?,
            "Tadoku log creation",
        )
        .await?;
        self.ensure_log_duration(created, batch).await
    }

    async fn ensure_log_duration(
        &mut self,
        log: TadokuLog,
        batch: &TadokuExportBatch,
    ) -> anyhow::Result<String> {
        // Save the remote identity before corrections can remove a legacy
        // marker or fail, so a retry still knows which log belongs to this batch.
        if let Some(persistence) = &self.persistence {
            persistence
                .db
                .remember_tadoku_batch_log(batch.batch_id.clone(), log.id.clone())
                .await?;
        }
        let desired_tags = tadoku_tags(&self.config, batch);
        let duration_matches = tadoku_log_duration_matches(&log, batch.duration_seconds);
        let description_matches = log.description.as_deref() == Some(batch.description.as_str());
        let tags_match = desired_tags.iter().all(|desired| {
            log.tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(desired))
        }) && !log.tags.iter().any(|tag| is_rewatch_batch_tag(tag));
        if duration_matches && description_matches && tags_match {
            return Ok(log.id);
        }

        warn!(
            "Tadoku log {} is missing the expected title, duration, or tags; updating it",
            log.id
        );
        let mut tags = log.tags.clone();
        tags.retain(|tag| !is_rewatch_batch_tag(tag));
        for desired in desired_tags {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&desired))
            {
                tags.push(desired);
            }
        }
        let payload = UpdateLogRequest {
            duration_seconds: batch.duration_seconds,
            tags,
            description: &batch.description,
        };
        let url = self.api_url(&format!("logs/{}", log.id));
        let updated: TadokuLog = response_json(
            self.send_authenticated(|http| http.put(&url).json(&payload))
                .await?,
            "Tadoku log duration correction",
        )
        .await?;
        let updated_tags_match = tadoku_tags(&self.config, batch).iter().all(|desired| {
            updated
                .tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(desired))
        }) && !updated.tags.iter().any(|tag| is_rewatch_batch_tag(tag));
        if !tadoku_log_duration_matches(&updated, batch.duration_seconds)
            || updated.description.as_deref() != Some(batch.description.as_str())
            || !updated_tags_match
        {
            bail!(
                "Tadoku returned an unexpected title, duration, or tags after updating log {}",
                updated.id
            );
        }
        Ok(updated.id)
    }
}

// Listening is time-only in Tadoku: an amount/unit would describe a different log.
fn tadoku_log_duration_matches(log: &TadokuLog, duration_seconds: i32) -> bool {
    log.duration_seconds == Some(duration_seconds)
        && log.amount.unwrap_or_default() == 0.0
        && log.unit_key.as_deref().is_none_or(str::is_empty)
}

fn tadoku_tags(config: &TadokuConfig, batch: &TadokuExportBatch) -> Vec<String> {
    let mut tags = vec!["nagare".to_string()];
    if batch.is_longform_checkpoint {
        tags.push("dense".to_string());
    }
    for rule in &config.path_tag_rules {
        let needle = rule.contains.to_lowercase();
        if !is_rewatch_batch_tag(&rule.tag)
            && batch
                .file_paths
                .iter()
                .any(|path| path.to_lowercase().contains(&needle))
            && !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&rule.tag))
        {
            tags.push(rule.tag.clone());
        }
    }
    tags
}

fn rewatch_batch_tag(batch: &TadokuExportBatch) -> String {
    format!("nagare-watch-{}", batch.batch_id)
}

fn is_rewatch_batch_tag(tag: &str) -> bool {
    tag.get(.."nagare-watch-".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("nagare-watch-"))
}

fn matching_existing_log<'a>(
    batch: &TadokuExportBatch,
    existing: &'a HashMap<String, TadokuLog>,
    prior_log_ids: Option<&HashSet<String>>,
) -> anyhow::Result<Option<&'a TadokuLog>> {
    let linked = batch
        .tadoku_log_id
        .as_deref()
        .and_then(|id| existing.get(id));
    if linked.is_some() {
        return Ok(linked);
    }
    if batch.has_rewatch {
        // Recognize pending exports from older versions without sending their
        // internal marker back to Tadoku.
        let marker = rewatch_batch_tag(batch);
        if let Some(log) = existing
            .values()
            .find(|log| log.tags.iter().any(|tag| tag.eq_ignore_ascii_case(&marker)))
        {
            return Ok(Some(log));
        }
        let Some(prior_log_ids) = prior_log_ids else {
            return Ok(None);
        };
        let mut candidates = existing.values().filter(|log| {
            !prior_log_ids.contains(&log.id)
                && log.description.as_deref() == Some(&batch.description)
                && !log.tags.iter().any(|tag| is_rewatch_batch_tag(tag))
        });
        let candidate = candidates.next();
        if candidates.next().is_some() {
            bail!(
                "Multiple Tadoku logs could belong to this rewatch; refusing to create or change a log until the match is resolved"
            );
        }
        Ok(candidate)
    } else if batch.is_longform_checkpoint {
        Ok(None)
    } else {
        Ok(existing.values().find(|log| {
            log.description.as_deref() == Some(&batch.description)
                && !log.tags.iter().any(|tag| is_rewatch_batch_tag(tag))
                && !prior_log_ids.is_some_and(|ids| ids.contains(&log.id))
        }))
    }
}

fn eligible_registration_ids(
    registrations: &[Registration],
    language_code: &str,
    listening_activity_id: i32,
) -> Vec<String> {
    registrations
        .iter()
        .filter(|registration| {
            let language_allowed = registration
                .languages
                .iter()
                .any(|language| language.code.eq_ignore_ascii_case(language_code));
            let activity_allowed = registration
                .contest
                .as_ref()
                .map(|contest| {
                    contest
                        .allowed_activities
                        .iter()
                        .any(|activity| activity.id == listening_activity_id)
                })
                .unwrap_or(false);
            language_allowed && activity_allowed
        })
        .map(|registration| registration.id.clone())
        .collect()
}

async fn response_json<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: &str,
) -> anyhow::Result<T> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let summary = body.trim().chars().take(300).collect::<String>();
        bail!("{operation} failed with HTTP {status}: {summary}");
    }
    serde_json::from_str(&body).with_context(|| format!("Invalid response from {operation}"))
}

fn extract_cookie_value(raw: &str) -> anyhow::Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Tadoku session cookie is empty");
    }
    if raw.contains(['\r', '\n']) {
        bail!("Tadoku session cookie contains an invalid newline");
    }
    Ok(cookie_from_header(raw).unwrap_or_else(|| raw.to_string()))
}

fn cookie_from_header(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == "tadoku_session" && !value.is_empty()).then(|| value.to_string())
    })
}

fn is_tadoku_production_host(url: &reqwest::Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "tadoku.app" || host.ends_with(".tadoku.app"))
}

fn add_session_cookie(cookies: &Jar, auth_url: &str, value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(auth_url).context("Invalid Tadoku auth URL")?;
    let cookie = if is_tadoku_production_host(&url) {
        format!("tadoku_session={value}; Path=/; Domain=.tadoku.app; Secure; HttpOnly")
    } else {
        format!("tadoku_session={value}; Path=/; HttpOnly")
    };
    cookies.add_cookie_str(&cookie, &url);
    Ok(())
}

fn persistence(config: Arc<RwLock<Config>>, db: Arc<AppDatabase>) -> TadokuPersistence {
    TadokuPersistence { config, db }
}

pub async fn test_connection(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<TadokuConnectionInfo> {
    let tadoku_config = config.read().await.tadoku.clone();
    let language_code = tadoku_config.language_code.trim().to_ascii_lowercase();
    let mut client = TadokuClient::new(tadoku_config, Some(persistence(config, db)), true)?;
    Ok(client.connection_info(&language_code).await?.info)
}

pub async fn refresh_authentication(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<()> {
    let tadoku_config = config.read().await.tadoku.clone();
    let mut client = TadokuClient::new(tadoku_config, Some(persistence(config, db)), false)?;
    client.refresh_session().await
}

pub async fn export_once(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<usize> {
    export(config, db, None, false).await
}

pub async fn export_selected(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
    history_ids: Vec<String>,
) -> anyhow::Result<usize> {
    if history_ids.is_empty() {
        bail!("Select at least one episode to sync");
    }
    export(config, db, Some(history_ids), false).await
}

async fn export(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
    selected_history_ids: Option<Vec<String>>,
    automatic: bool,
) -> anyhow::Result<usize> {
    // A five-minute check, an episode-finished event, and a manual request can
    // arrive together. Serialize them so one completed episode is never
    // prepared for two concurrent Tadoku requests.
    let _export_guard = EXPORT_LOCK.lock().await;
    let (tadoku_config, target_audio_language) = {
        let config = config.read().await;
        (config.tadoku.clone(), config.target_language.clone())
    };
    let language_code = tadoku_config.language_code.trim().to_ascii_lowercase();
    let mut client = TadokuClient::new(tadoku_config, Some(persistence(config, db.clone())), true)?;
    let connection = client.connection_info(&language_code).await?;
    let registrations = client.registrations().await?;
    let eastern_date = eastern_time(Utc::now()).date_naive().to_string();
    let batches = match selected_history_ids {
        Some(history_ids) => {
            db.prepare_selected_tadoku_batches(
                eastern_date,
                language_code,
                target_audio_language.clone(),
                history_ids,
            )
            .await?
        }
        None if automatic => {
            db.prepare_automatic_tadoku_batches(eastern_date, language_code, target_audio_language)
                .await?
        }
        None => {
            db.prepare_daily_tadoku_batches(eastern_date, language_code, target_audio_language)
                .await?
        }
    };
    if batches.is_empty() {
        info!("Tadoku export found no eligible episodes");
        return Ok(0);
    }

    let existing = client
        .existing_nagare_logs(&connection.info.user_id)
        .await?;
    let mut completed = 0usize;
    let mut failures = Vec::new();
    for batch in batches {
        let result = async {
            let prior_log_ids = db
                .tadoku_log_match_exclusions(
                    batch.batch_id.clone(),
                    batch
                        .has_rewatch
                        .then(|| existing.keys().cloned().collect()),
                )
                .await?;
            let existing_log = matching_existing_log(&batch, &existing, Some(&prior_log_ids))?;
            if let Some(log) = existing_log {
                info!(
                    "Tadoku batch {} already exists remotely as {}; verifying it locally",
                    batch.batch_id, log.id
                );
                client.ensure_log_duration(log.clone(), &batch).await
            } else {
                client
                    .create_log(
                        &batch,
                        connection.info.listening_activity_id,
                        &registrations,
                    )
                    .await
            }
        }
        .await;

        match result {
            Ok(log_id) => {
                db.mark_tadoku_batch_completed(batch.batch_id.clone(), log_id)
                    .await?;
                completed += 1;
                info!(
                    "Exported {} seconds of {} to Tadoku",
                    batch.duration_seconds, batch.series_name
                );
            }
            Err(error) => {
                let message = error.to_string();
                warn!(
                    "Failed to export Tadoku batch {}: {}",
                    batch.batch_id, message
                );
                db.mark_tadoku_batch_failed(batch.batch_id.clone(), message.clone())
                    .await?;
                failures.push(format!("{}: {}", batch.series_name, message));
            }
        }
    }

    if failures.is_empty() {
        Ok(completed)
    } else {
        bail!(
            "{} Tadoku export(s) failed: {}",
            failures.len(),
            failures.join("; ")
        )
    }
}

/// Export all eligible episodes only when the near-real-time automatic mode is
/// currently selected. The setting is checked at execution time so a queued
/// playback-ended check cannot run after the user switches back to daily sync.
pub async fn export_if_automatic(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<Option<usize>> {
    if !config.read().await.tadoku.automatic_sync_enabled() {
        return Ok(None);
    }

    export(config, db, None, true).await.map(Some)
}

pub async fn run_exporter(config: Arc<RwLock<Config>>, db: Arc<AppDatabase>) {
    let mut last_automatic_check: Option<Instant> = None;

    loop {
        let tadoku_config = config.read().await.tadoku.clone();
        if tadoku_config.automatic_sync_enabled() {
            let automatic_check_due = last_automatic_check
                .is_none_or(|last_check| last_check.elapsed() >= AUTOMATIC_EXPORT_INTERVAL);
            if automatic_check_due {
                last_automatic_check = Some(Instant::now());
                match export_if_automatic(config.clone(), db.clone()).await {
                    Ok(Some(count)) => {
                        info!("Automatic Tadoku check completed ({} show logs)", count)
                    }
                    Ok(None) => {}
                    Err(error) => error!("Automatic Tadoku check failed: {}", error),
                }
            }
        } else {
            last_automatic_check = None;
            if tadoku_config.enabled && tadoku_config.sync_mode == TadokuSyncMode::Daily {
                let now_eastern = eastern_time(Utc::now());
                let export_hour = tadoku_config.export_hour_eastern.min(23);
                if now_eastern.hour() >= export_hour {
                    let export_date = now_eastern.date_naive().to_string();
                    match db.tadoku_export_due(export_date.clone()).await {
                        Ok(true) => {
                            if let Err(error) =
                                db.mark_tadoku_run_started(export_date.clone()).await
                            {
                                error!("Could not record Tadoku export start: {}", error);
                            } else {
                                let result = export_once(config.clone(), db.clone()).await;
                                let error_message = result.as_ref().err().map(ToString::to_string);
                                if let Err(error) = db
                                    .mark_tadoku_run_finished(export_date, error_message)
                                    .await
                                {
                                    error!("Could not record Tadoku export result: {}", error);
                                }
                                match result {
                                    Ok(count) => {
                                        info!("Daily Tadoku export completed ({} show logs)", count)
                                    }
                                    Err(error) => error!("Daily Tadoku export failed: {}", error),
                                }
                            }
                        }
                        Ok(false) => {}
                        Err(error) => error!("Could not check Tadoku export schedule: {}", error),
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// Convert UTC to America/New_York without relying on the host's time-zone
/// database. Since 2007, US daylight time runs from the second Sunday in
/// March at 02:00 local to the first Sunday in November at 02:00 local.
fn eastern_time(utc: DateTime<Utc>) -> DateTime<FixedOffset> {
    let year = utc.year();
    let dst_start = nth_weekday(year, 3, Weekday::Sun, 2)
        .and_hms_opt(7, 0, 0)
        .expect("valid DST start");
    let dst_end = nth_weekday(year, 11, Weekday::Sun, 1)
        .and_hms_opt(6, 0, 0)
        .expect("valid DST end");
    let offset_seconds = if utc.naive_utc() >= dst_start && utc.naive_utc() < dst_end {
        -4 * 60 * 60
    } else {
        -5 * 60 * 60
    };
    utc.with_timezone(&FixedOffset::east_opt(offset_seconds).expect("valid Eastern offset"))
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, nth: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let days_until = (7 + weekday.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    first + chrono::Duration::days(days_until + 7 * (nth.saturating_sub(1) as i64))
}

#[cfg(test)]
mod tests {
    use super::{
        Activity, CreateLogRequest, Language, Registration, RegistrationContest, TadokuClient,
        TadokuLog, eastern_time, eligible_registration_ids, extract_cookie_value,
        matching_existing_log, tadoku_log_duration_matches, tadoku_tags,
    };
    use crate::config::TadokuConfig;
    use crate::mining::{AppDatabase, TadokuExportBatch};
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use chrono::{TimeZone, Timelike, Utc};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct MockTadoku {
        base_url: String,
        set_session_cookie: bool,
        reject_first_options_request: bool,
        reject_all_options: AtomicBool,
        guest_session: AtomicBool,
        login_status: AtomicUsize,
        login_count: AtomicUsize,
        session_count: AtomicUsize,
        options_count: AtomicUsize,
        login_requests: Mutex<Vec<(HeaderMap, serde_json::Value)>>,
        request_cookies: Mutex<Vec<String>>,
        logs: Mutex<Vec<serde_json::Value>>,
        update_requests: Mutex<Vec<serde_json::Value>>,
        incorrect_update_response: AtomicBool,
        lose_log_response: AtomicBool,
    }

    async fn submit_login(
        State(state): State<Arc<MockTadoku>>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        state.login_count.fetch_add(1, Ordering::SeqCst);
        state
            .login_requests
            .lock()
            .unwrap()
            .push((headers.clone(), body));
        if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(&state.base_url) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let status =
            StatusCode::from_u16(state.login_status.load(Ordering::SeqCst) as u16).unwrap();
        if status != StatusCode::OK {
            // A redirect must never forward the credential body to this URL.
            return (
                status,
                [(
                    header::LOCATION,
                    format!("{}/credential-leak", state.base_url),
                )],
            )
                .into_response();
        }
        let mut response = Json(
            json!({"account": {"id": "user-id", "display_name": "Reader"}, "account_app": "v2"}),
        )
        .into_response();
        if state.set_session_cookie {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("tadoku_session=fresh-session; Path=/; HttpOnly"),
            );
        }
        response
    }

    async fn session(State(state): State<Arc<MockTadoku>>, headers: HeaderMap) -> Response {
        state.session_count.fetch_add(1, Ordering::SeqCst);
        let mut authenticated = false;
        if let Some(cookie) = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
        {
            authenticated = cookie.contains("tadoku_session=fresh-session")
                || cookie.contains("tadoku_session=saved-session");
            state
                .request_cookies
                .lock()
                .unwrap()
                .push(cookie.to_string());
        }
        let account = if authenticated && !state.guest_session.load(Ordering::SeqCst) {
            json!({"id": "user-id", "display_name": "Reader"})
        } else {
            serde_json::Value::Null
        };
        Json(json!({"account": account, "account_app": "v2"})).into_response()
    }

    async fn configuration_options(
        State(state): State<Arc<MockTadoku>>,
        headers: HeaderMap,
    ) -> Response {
        if let Some(cookie) = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
        {
            state
                .request_cookies
                .lock()
                .unwrap()
                .push(cookie.to_string());
        }
        let call = state.options_count.fetch_add(1, Ordering::SeqCst);
        if (state.reject_first_options_request && call == 0)
            || state.reject_all_options.load(Ordering::SeqCst)
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Json(json!({
            "activities": [{"id": 2, "name": "Listening"}],
            "languages": [{"code": "jpn"}],
            "units": [{
                "unit_key": "pages", "log_activity_id": 1, "name": "Page", "modifier": 1
            }]
        }))
        .into_response()
    }

    async fn spawn_mock_tadoku(
        set_session_cookie: bool,
        reject_first_options_request: bool,
    ) -> (Arc<MockTadoku>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(MockTadoku {
            base_url,
            set_session_cookie,
            reject_first_options_request,
            reject_all_options: AtomicBool::new(false),
            guest_session: AtomicBool::new(false),
            login_status: AtomicUsize::new(200),
            login_count: AtomicUsize::new(0),
            session_count: AtomicUsize::new(0),
            options_count: AtomicUsize::new(0),
            login_requests: Mutex::new(Vec::new()),
            request_cookies: Mutex::new(Vec::new()),
            logs: Mutex::new(Vec::new()),
            update_requests: Mutex::new(Vec::new()),
            incorrect_update_response: AtomicBool::new(false),
            lose_log_response: AtomicBool::new(false),
        });
        let app = Router::new()
            .route("/login", post(submit_login))
            .route("/credential-leak", post(submit_login))
            .route("/session", get(session))
            .route("/logs/configuration-options", get(configuration_options))
            .route(
                "/users/user-id/logs",
                get(|State(state): State<Arc<MockTadoku>>| async move {
                    let logs = state.logs.lock().unwrap();
                    Json(json!({"logs": *logs, "total_size": logs.len()}))
                }),
            )
            .route(
                "/logs",
                post(
                    |State(state): State<Arc<MockTadoku>>,
                     Json(mut log): Json<serde_json::Value>| async move {
                        if ["amount", "unit_id", "unit_key"]
                            .iter()
                            .any(|key| log.get(key).is_some())
                            || log["duration_seconds"].as_i64().is_none()
                        {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        let mut logs = state.logs.lock().unwrap();
                        log["id"] = json!(format!("log-{}", logs.len() + 1));
                        logs.push(log.clone());
                        if state.lose_log_response.swap(false, Ordering::SeqCst) {
                            StatusCode::INTERNAL_SERVER_ERROR.into_response()
                        } else {
                            Json(log).into_response()
                        }
                    },
                ),
            )
            .route(
                "/logs/{id}",
                axum::routing::put(
                    |State(state): State<Arc<MockTadoku>>,
                     Path(id): Path<String>,
                     Json(body): Json<serde_json::Value>| async move {
                        if ["amount", "unit_id", "unit_key"]
                            .iter()
                            .any(|key| body.get(key).is_some())
                            || body["duration_seconds"].as_i64().is_none()
                        {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        state.update_requests.lock().unwrap().push(body.clone());
                        let mut logs = state.logs.lock().unwrap();
                        let log = logs.iter_mut().find(|log| log["id"] == id).unwrap();
                        for (key, value) in body.as_object().unwrap() {
                            log[key] = value.clone();
                        }
                        if state.lose_log_response.swap(false, Ordering::SeqCst) {
                            StatusCode::INTERNAL_SERVER_ERROR.into_response()
                        } else {
                            let mut response = log.clone();
                            if state.incorrect_update_response.load(Ordering::SeqCst) {
                                response["duration_seconds"] = json!(1);
                            }
                            Json(response).into_response()
                        }
                    },
                ),
            )
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, server)
    }

    fn mock_config(state: &MockTadoku) -> TadokuConfig {
        TadokuConfig {
            username: "reader@example.com".to_string(),
            password: "super-secret".to_string(),
            api_url: state.base_url.clone(),
            session_url: format!("{}/session", state.base_url),
            ..TadokuConfig::default()
        }
    }

    async fn mock_batch_database(
        batch: &TadokuExportBatch,
    ) -> (Arc<AppDatabase>, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("nagare-tadoku-log-{}.sqlite", uuid::Uuid::new_v4()));
        let db = Arc::new(AppDatabase::new(path.clone(), None).await.unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO tadoku_export_batches (batch_id, export_date, series_name, description, duration_seconds, language_code, created_at, updated_at) VALUES (?1, '2026-10-28', ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![batch.batch_id, batch.series_name, batch.description, batch.duration_seconds, batch.language_code, Utc::now().to_rfc3339()],
        )
        .unwrap();
        (db, path)
    }

    #[test]
    fn extracts_cookie_values_and_headers() {
        assert_eq!(extract_cookie_value("abc").unwrap(), "abc");
        assert_eq!(
            extract_cookie_value("other=xyz; tadoku_session=abc; Path=/").unwrap(),
            "abc"
        );
        assert!(extract_cookie_value("abc\ndef").is_err());
    }

    #[test]
    fn adds_configured_tags_for_case_insensitive_path_matches() {
        let config = TadokuConfig::default();
        let mut batch = TadokuExportBatch {
            batch_id: "batch".to_string(),
            tadoku_log_id: None,
            is_longform_checkpoint: false,
            has_rewatch: false,
            series_name: "Frieren".to_string(),
            description: "Frieren S01E01".to_string(),
            duration_seconds: 1_400,
            language_code: "jpn".to_string(),
            file_paths: vec!["/media/Anime/Frieren/episode01.mkv".to_string()],
        };
        assert_eq!(tadoku_tags(&config, &batch), vec!["nagare", "anime"]);
        batch.has_rewatch = true;
        assert_eq!(tadoku_tags(&config, &batch), vec!["nagare", "anime"]);
        batch.is_longform_checkpoint = true;
        assert_eq!(
            tadoku_tags(&config, &batch),
            vec!["nagare", "dense", "anime"]
        );
    }

    #[tokio::test]
    async fn creates_dense_duration_logs_and_verifies_corrections() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let mut client =
            TadokuClient::new_with_auth_url(mock_config(&state), None, false, &state.base_url)
                .unwrap();
        let connection = client.connection_info("jpn").await.unwrap();
        let batch = TadokuExportBatch {
            batch_id: "checkpoint".into(),
            tadoku_log_id: None,
            is_longform_checkpoint: true,
            has_rewatch: false,
            series_name: "Audiobook".into(),
            description: "Audiobook checkpoint".into(),
            duration_seconds: 1_815,
            language_code: "jpn".into(),
            file_paths: Vec::new(),
        };
        let id = client
            .create_log(&batch, connection.info.listening_activity_id, &[])
            .await
            .unwrap();
        assert!(state.update_requests.lock().unwrap().is_empty());
        {
            let mut logs = state.logs.lock().unwrap();
            assert_eq!(logs[0]["duration_seconds"], 1815);
            assert_eq!(logs[0]["tags"], json!(["nagare", "dense"]));
            logs[0]["duration_seconds"] = json!(1800);
            logs[0]["tags"] = json!(["nagare", "favorite"]);
        }
        let log = client
            .existing_nagare_logs("user-id")
            .await
            .unwrap()
            .remove(&id)
            .unwrap();
        client.ensure_log_duration(log, &batch).await.unwrap();
        assert_eq!(
            state.update_requests.lock().unwrap()[0],
            json!({
                "duration_seconds": 1815, "tags": ["nagare", "favorite", "dense"],
                "description": batch.description,
            })
        );
        let mut log = client
            .existing_nagare_logs("user-id")
            .await
            .unwrap()
            .remove(&id)
            .unwrap();
        log.duration_seconds = None;
        state
            .incorrect_update_response
            .store(true, Ordering::SeqCst);
        let error = client.ensure_log_duration(log, &batch).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unexpected title, duration, or tags")
        );
        assert_eq!(state.logs.lock().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn rewatch_export_ignores_original_log_and_recovers_a_lost_creation_response() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let config = mock_config(&state);
        let mut client =
            TadokuClient::new_with_auth_url(config.clone(), None, false, &state.base_url).unwrap();
        client.connection_info("jpn").await.unwrap();
        let mut batch = TadokuExportBatch {
            batch_id: uuid::Uuid::new_v4().to_string(),
            tadoku_log_id: None,
            is_longform_checkpoint: false,
            has_rewatch: true,
            series_name: "Show".into(),
            description: "Show — S01E01".into(),
            duration_seconds: 1_440,
            language_code: "jpn".into(),
            file_paths: Vec::new(),
        };
        let (db, path) = mock_batch_database(&batch).await;
        let original = json!({
            "id": "original", "description": batch.description,
            "duration_seconds": 1440,
            "tags": ["nagare"]
        });
        state.logs.lock().unwrap().push(original.clone());
        let existing = client.existing_nagare_logs("user-id").await.unwrap();
        let prior_ids = db
            .tadoku_log_match_exclusions(
                batch.batch_id.clone(),
                Some(existing.keys().cloned().collect()),
            )
            .await
            .unwrap();
        assert!(
            matching_existing_log(&batch, &existing, Some(&prior_ids))
                .unwrap()
                .is_none()
        );
        state.lose_log_response.store(true, Ordering::SeqCst);
        assert!(client.create_log(&batch, 2, &[]).await.is_err());
        assert_eq!(state.logs.lock().unwrap().len(), 2);
        assert_eq!(state.logs.lock().unwrap()[1]["tags"], json!(["nagare"]));

        // A new client represents retrying after Nagare restarts. Discovery
        // uses the local snapshot despite the original log's identical text.
        let mut restarted =
            TadokuClient::new_with_auth_url(config, None, false, &state.base_url).unwrap();
        drop(db);
        let db = Arc::new(AppDatabase::new(path.clone(), None).await.unwrap());
        let existing = restarted.existing_nagare_logs("user-id").await.unwrap();
        let prior_ids = db
            .tadoku_log_match_exclusions(
                batch.batch_id.clone(),
                Some(existing.keys().cloned().collect()),
            )
            .await
            .unwrap();
        assert_eq!(
            prior_ids,
            std::collections::HashSet::from(["original".into()])
        );
        let recovered = matching_existing_log(&batch, &existing, Some(&prior_ids))
            .unwrap()
            .unwrap();
        assert_ne!(recovered.id, "original");
        restarted
            .ensure_log_duration(recovered.clone(), &batch)
            .await
            .unwrap();
        assert_eq!(state.logs.lock().unwrap().len(), 2);
        assert_eq!(state.logs.lock().unwrap()[0], original);
        let mut ambiguous = existing.clone();
        let mut duplicate = recovered.clone();
        duplicate.id = "ambiguous".into();
        ambiguous.insert(duplicate.id.clone(), duplicate);
        assert!(matching_existing_log(&batch, &ambiguous, Some(&prior_ids)).is_err());

        batch.batch_id = uuid::Uuid::new_v4().to_string();
        let all_ids = existing.keys().cloned().collect();
        assert!(
            matching_existing_log(&batch, &existing, Some(&all_ids))
                .unwrap()
                .is_none()
        );
        batch.has_rewatch = false;
        let rewatch_ids = std::collections::HashSet::from([recovered.id.clone()]);
        assert_eq!(
            matching_existing_log(&batch, &existing, Some(&rewatch_ids))
                .unwrap()
                .unwrap()
                .id,
            "original"
        );
        server.abort();
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn removes_legacy_rewatch_tags_and_keeps_the_log_identity_after_a_lost_update_response() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let config = mock_config(&state);
        let mut batch = TadokuExportBatch {
            batch_id: uuid::Uuid::new_v4().to_string(),
            tadoku_log_id: None,
            is_longform_checkpoint: false,
            has_rewatch: true,
            series_name: "Show".into(),
            description: "Show — S01E01".into(),
            duration_seconds: 1_440,
            language_code: "jpn".into(),
            file_paths: vec!["/media/anime/show.mkv".into()],
        };
        let (db, path) = mock_batch_database(&batch).await;
        let shared = Arc::new(tokio::sync::RwLock::new(crate::config::Config {
            tadoku: config.clone(),
            ..Default::default()
        }));
        let mut client = TadokuClient::new_with_auth_url(
            config,
            Some(super::persistence(shared, db.clone())),
            false,
            &state.base_url,
        )
        .unwrap();
        client.connection_info("jpn").await.unwrap();
        state.logs.lock().unwrap().push(json!({
            "id": "rewatch", "description": batch.description,
            "duration_seconds": 1440,
            "tags": ["nagare", "anime", "favorite", super::rewatch_batch_tag(&batch)]
        }));
        let existing = client.existing_nagare_logs("user-id").await.unwrap();
        let legacy = matching_existing_log(&batch, &existing, None)
            .unwrap()
            .unwrap();
        state.lose_log_response.store(true, Ordering::SeqCst);
        assert!(
            client
                .ensure_log_duration(legacy.clone(), &batch)
                .await
                .is_err()
        );
        assert_eq!(
            state.logs.lock().unwrap()[0]["tags"],
            json!(["nagare", "anime", "favorite"])
        );

        // The marker is gone remotely, but the local ID was saved before PUT.
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (log_id, status): (String, String) = conn
            .query_row(
                "SELECT tadoku_log_id, status FROM tadoku_export_batches WHERE batch_id = ?1",
                [&batch.batch_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(log_id, "rewatch");
        assert_eq!(status, "pending");
        let other_batch_exclusions = db
            .tadoku_log_match_exclusions("another-batch".into(), None)
            .await
            .unwrap();
        assert!(other_batch_exclusions.contains("rewatch"));
        assert!(
            db.tadoku_log_match_exclusions(batch.batch_id.clone(), None)
                .await
                .unwrap()
                .is_empty()
        );
        batch.tadoku_log_id = Some(log_id);
        let existing = client.existing_nagare_logs("user-id").await.unwrap();
        let recovered = matching_existing_log(&batch, &existing, None)
            .unwrap()
            .unwrap();
        client
            .ensure_log_duration(recovered.clone(), &batch)
            .await
            .unwrap();
        assert_eq!(state.logs.lock().unwrap().len(), 1);
        server.abort();
        drop(conn);
        drop(client);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_credentials_are_rejected_before_client_creation() {
        let config = TadokuConfig::default();
        let error = TadokuClient::new(config, None, true).err().unwrap();
        assert_eq!(
            error.to_string(),
            "Tadoku email and password are not configured"
        );
    }

    #[tokio::test]
    async fn login_submits_email_and_password_as_json_with_origin() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let mut client =
            TadokuClient::new_with_auth_url(mock_config(&state), None, false, &state.base_url)
                .unwrap();
        let connection = client.connection_info("jpn").await.unwrap();
        assert_eq!(connection.info.user_id, "user-id");
        assert_eq!(connection.info.display_name.as_deref(), Some("Reader"));
        assert_eq!(connection.info.listening_activity_id, 2);
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        let requests = state.login_requests.lock().unwrap();
        let (headers, body) = requests.first().unwrap();
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(headers[header::ORIGIN], state.base_url);
        assert!(!headers.contains_key(header::AUTHORIZATION));
        assert!(!headers.contains_key("x-session-token"));
        assert_eq!(
            *body,
            json!({"email": "reader@example.com", "password": "super-secret"})
        );
        assert_eq!(
            client.session_cookie().unwrap().as_deref(),
            Some("fresh-session")
        );
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn refuses_login_redirects_without_forwarding_credentials() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        state.login_status.store(307, Ordering::SeqCst);
        let mut client =
            TadokuClient::new_with_auth_url(mock_config(&state), None, false, &state.base_url)
                .unwrap();
        let error = client.connection_info("jpn").await.unwrap_err();
        assert!(error.to_string().contains("HTTP 307"));
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn failed_or_throttled_login_is_not_retried() {
        for (status, message) in [
            (401, "saved email and password"),
            (429, "wait 15 minutes"),
            (503, "HTTP 503"),
        ] {
            let (state, server) = spawn_mock_tadoku(true, false).await;
            state.login_status.store(status, Ordering::SeqCst);
            let mut client =
                TadokuClient::new_with_auth_url(mock_config(&state), None, false, &state.base_url)
                    .unwrap();
            let error = client.connection_info("jpn").await.unwrap_err();
            assert!(error.to_string().contains(message));
            assert!(!error.to_string().contains("super-secret"));
            assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
            assert_eq!(state.options_count.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn successful_login_requires_browser_session_cookie() {
        let (state, server) = spawn_mock_tadoku(false, false).await;
        let mut client =
            TadokuClient::new_with_auth_url(mock_config(&state), None, false, &state.base_url)
                .unwrap();
        let error = client.connection_info("jpn").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Tadoku login did not return a tadoku_session cookie"
        );
        server.abort();
    }

    #[tokio::test]
    async fn reuses_saved_cookie_without_login() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let mut config = mock_config(&state);
        config.session_cookie = "saved-session".to_string();
        let mut client =
            TadokuClient::new_with_auth_url(config, None, true, &state.base_url).unwrap();
        client.connection_info("jpn").await.unwrap();
        assert_eq!(state.login_count.load(Ordering::SeqCst), 0);
        assert!(
            state
                .request_cookies
                .lock()
                .unwrap()
                .iter()
                .any(|cookie| cookie.contains("tadoku_session=saved-session"))
        );
        server.abort();
    }

    #[tokio::test]
    async fn unauthorized_request_reauthenticates_and_retries_once() {
        let (state, server) = spawn_mock_tadoku(true, true).await;
        let mut config = mock_config(&state);
        config.session_cookie = "saved-session".to_string();
        let mut client =
            TadokuClient::new_with_auth_url(config, None, true, &state.base_url).unwrap();
        client.connection_info("jpn").await.unwrap();
        assert_eq!(state.options_count.load(Ordering::SeqCst), 2);
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            client.session_cookie().unwrap().as_deref(),
            Some("fresh-session")
        );
        server.abort();
    }

    #[tokio::test]
    async fn expired_session_with_http_200_refreshes_once() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let mut config = mock_config(&state);
        // Also covers old bare Kratos tokens whose endpoint was already updated.
        config.session_cookie = "expired-session".into();
        let mut client =
            TadokuClient::new_with_auth_url(config, None, true, &state.base_url).unwrap();
        client.connection_info("jpn").await.unwrap();
        assert_eq!(state.session_count.load(Ordering::SeqCst), 2);
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(state.options_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn persistent_authentication_failure_stops_after_one_refresh() {
        for guest_session in [true, false] {
            let (state, server) = spawn_mock_tadoku(true, false).await;
            state.guest_session.store(guest_session, Ordering::SeqCst);
            state
                .reject_all_options
                .store(!guest_session, Ordering::SeqCst);
            let mut config = mock_config(&state);
            config.session_cookie = "saved-session".into();
            let mut client =
                TadokuClient::new_with_auth_url(config, None, true, &state.base_url).unwrap();
            assert!(client.connection_info("jpn").await.is_err());
            assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
            assert_eq!(
                state.options_count.load(Ordering::SeqCst),
                if guest_session { 0 } else { 2 }
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn persists_migrated_session_and_reuses_it_after_restart() {
        let (state, server) = spawn_mock_tadoku(true, false).await;
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-auth-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(
            crate::mining::AppDatabase::new(path.clone(), None)
                .await
                .unwrap(),
        );
        let mut original = crate::config::Config::default();
        original.tadoku = mock_config(&state);
        original.tadoku.session_url = "https://account.tadoku.app/kratos/sessions/whoami".into();
        original.tadoku.session_cookie = "old-kratos-token".into();
        original.tadoku.enabled = true;
        original.tadoku.sync_mode = crate::config::TadokuSyncMode::Automatic;
        db.save_config(original.clone()).await.unwrap();
        let shared = Arc::new(tokio::sync::RwLock::new(original.clone()));
        let mut client = TadokuClient::new_with_auth_url(
            original.tadoku.clone(),
            Some(super::persistence(shared.clone(), db.clone())),
            true,
            &state.base_url,
        )
        .unwrap();
        assert!(client.session_cookie().unwrap().is_none());
        client.ensure_authenticated().await.unwrap();
        let saved = db
            .load_config_or_default(path.with_extension("json"))
            .await
            .unwrap();
        let mut expected = original;
        expected.tadoku.session_url = "https://tadoku.app/api/internal/identity/session".into();
        expected.tadoku.session_cookie = "fresh-session".into();
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(shared.read().await.tadoku.session_cookie, "fresh-session");

        let mut restarted =
            TadokuClient::new_with_auth_url(saved.tadoku, None, true, &state.base_url).unwrap();
        restarted.ensure_authenticated().await.unwrap();
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            restarted.session_cookie().unwrap().as_deref(),
            Some("fresh-session")
        );
        server.abort();
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn migrates_legacy_session_settings_without_changing_credentials_or_sync_options() {
        for old_url in [
            "https://account.tadoku.app/kratos/sessions/whoami",
            " https://account.tadoku.app/kratos/sessions/whoami/ ",
        ] {
            let mut config: TadokuConfig = serde_json::from_value(json!({
                "username": "reader@example.com", "password": "saved-password",
                "session_cookie": "legacy-cookie", "session_url": old_url,
                "enabled": true, "sync_mode": "automatic", "language_code": "jpn",
                "export_hour_eastern": 18, "path_tag_rules": [{"contains": "books", "tag": "book"}]
            }))
            .unwrap();
            let mut expected = serde_json::to_value(&config).unwrap();
            expected["session_url"] = json!("https://tadoku.app/api/internal/identity/session");
            expected["session_cookie"] = json!("");
            config.normalize();
            assert_eq!(serde_json::to_value(&config).unwrap(), expected);
            config.session_cookie = "new-session".into();
            config.normalize();
            assert_eq!(config.session_cookie, "new-session");
        }
    }

    #[test]
    fn preserves_custom_endpoints_and_current_cookies() {
        let mut config = TadokuConfig {
            api_url: "https://custom.example/api".into(),
            session_url: "https://custom.example/session".into(),
            session_cookie: "tadoku_session=saved-session".into(),
            ..TadokuConfig::default()
        };
        let original = serde_json::to_value(&config).unwrap();
        config.normalize();
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        config.session_cookie = "ory_kratos_session=legacy".into();
        config.normalize();
        assert!(config.session_cookie.is_empty());
    }

    #[test]
    fn serializes_listening_duration_without_amount_or_unit() {
        let payload = CreateLogRequest {
            registration_ids: vec!["registration".to_string()],
            language_code: "jpn",
            activity_id: 2,
            duration_seconds: 1_415,
            tags: vec!["nagare".to_string()],
            description: "NARUTO 疾風伝 S10E10-13",
        };
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["duration_seconds"], 1_415);
        assert!(json.get("amount").is_none());
        assert!(json.get("unit_id").is_none());
        assert!(json.get("unit_key").is_none());
        assert_eq!(json["description"], "NARUTO 疾風伝 S10E10-13");
    }

    #[test]
    fn matches_time_only_logs_and_rejects_wrong_durations_or_amounts() {
        let mut log: TadokuLog = serde_json::from_value(json!({
            "id": "log", "duration_seconds": 1415,
            "amount": 0, "unit_name": "", "tags": ["nagare"]
        }))
        .unwrap();
        assert!(tadoku_log_duration_matches(&log, 1415));
        assert!(!tadoku_log_duration_matches(&log, 1416));
        log.duration_seconds = None;
        assert!(!tadoku_log_duration_matches(&log, 1415));
        log.duration_seconds = Some(1415);
        log.amount = Some(23.6);
        assert!(!tadoku_log_duration_matches(&log, 1415));
        log.amount = None;
        log.unit_key = Some("pages".into());
        assert!(!tadoku_log_duration_matches(&log, 1415));
    }

    #[test]
    fn migrates_obsolete_tadoku_api_urls() {
        for obsolete in [
            "https://tadoku.app/api/immersion",
            "https://tadoku.app/api/immersion/",
            "https://tadoku.app/api/internal",
        ] {
            let mut config = TadokuConfig {
                api_url: obsolete.to_string(),
                ..TadokuConfig::default()
            };
            config.normalize();
            assert_eq!(config.api_url, "https://tadoku.app/api/internal/immersion");
        }
    }

    #[test]
    fn includes_every_contest_that_allows_the_language_and_listening() {
        let registration = |id: &str, language: &str, activity_id: i32| Registration {
            id: id.to_string(),
            languages: vec![Language {
                code: language.to_string(),
            }],
            contest: Some(RegistrationContest {
                allowed_activities: vec![Activity {
                    id: activity_id,
                    name: "Listening".to_string(),
                }],
            }),
        };
        let registrations = vec![
            registration("official", "jpn", 2),
            registration("private", "jpn", 2),
            registration("reading-only", "jpn", 1),
            registration("other-language", "deu", 2),
        ];
        assert_eq!(
            eligible_registration_ids(&registrations, "jpn", 2),
            vec!["official".to_string(), "private".to_string()]
        );
    }

    #[test]
    fn eastern_time_observes_daylight_saving_boundaries() {
        let before_spring = Utc.with_ymd_and_hms(2026, 3, 8, 6, 59, 0).unwrap();
        let after_spring = Utc.with_ymd_and_hms(2026, 3, 8, 7, 0, 0).unwrap();
        assert_eq!(eastern_time(before_spring).hour(), 1);
        assert_eq!(eastern_time(after_spring).hour(), 3);

        let before_fall = Utc.with_ymd_and_hms(2026, 11, 1, 5, 59, 0).unwrap();
        let after_fall = Utc.with_ymd_and_hms(2026, 11, 1, 6, 0, 0).unwrap();
        assert_eq!(eastern_time(before_fall).hour(), 1);
        assert_eq!(eastern_time(after_fall).hour(), 1);
        assert_ne!(
            eastern_time(before_fall).offset(),
            eastern_time(after_fall).offset()
        );
    }
}
