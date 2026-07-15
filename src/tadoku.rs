use crate::config::{Config, TadokuConfig};
use crate::mining::{AppDatabase, TadokuExportBatch};
use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use reqwest::cookie::{CookieStore, Jar};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

const TADOKU_AUTH_BASE_URL: &str = "https://account.tadoku.app/kratos";

#[derive(Debug, Serialize)]
pub struct TadokuConnectionInfo {
    pub user_id: String,
    pub display_name: Option<String>,
    pub listening_activity_id: i32,
    pub listening_minutes_unit_id: String,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    identity: SessionIdentity,
}

#[derive(Debug, Deserialize)]
struct SessionIdentity {
    id: String,
    #[serde(default)]
    traits: SessionTraits,
}

#[derive(Debug, Default, Deserialize)]
struct SessionTraits {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigurationOptions {
    activities: Vec<Activity>,
    languages: Vec<Language>,
    units: Vec<Unit>,
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

#[derive(Debug, Deserialize)]
struct Unit {
    id: String,
    log_activity_id: i32,
    name: String,
    language_code: Option<String>,
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
    unit_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CreateLogRequest<'a> {
    registration_ids: Vec<String>,
    language_code: &'a str,
    activity_id: i32,
    amount: f64,
    unit_id: &'a str,
    tags: [&'static str; 1],
    description: &'a str,
}

#[derive(Debug, Serialize)]
struct UpdateLogRequest<'a> {
    amount: f64,
    unit_id: &'a str,
    tags: [&'static str; 1],
    description: &'a str,
}

#[derive(Debug, Deserialize)]
struct LoginFlow {
    ui: LoginUi,
}

#[derive(Debug, Deserialize)]
struct LoginUi {
    action: String,
    #[serde(default)]
    nodes: Vec<LoginNode>,
}

#[derive(Debug, Deserialize)]
struct LoginNode {
    attributes: LoginNodeAttributes,
}

#[derive(Debug, Deserialize)]
struct LoginNodeAttributes {
    name: Option<String>,
    value: Option<serde_json::Value>,
}

#[derive(Clone)]
struct TadokuPersistence {
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
}

impl TadokuPersistence {
    async fn save_cookie(&self, cookie: &str) -> anyhow::Result<()> {
        let mut config = self.config.write().await;
        let previous = std::mem::replace(&mut config.tadoku.session_cookie, cookie.to_string());
        if let Err(error) = self.db.save_config(config.clone()).await {
            config.tadoku.session_cookie = previous;
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
        Self::new_with_auth_url(
            config,
            persistence,
            seed_saved_cookie,
            TADOKU_AUTH_BASE_URL,
        )
    }

    fn new_with_auth_url(
        mut config: TadokuConfig,
        persistence: Option<TadokuPersistence>,
        seed_saved_cookie: bool,
        auth_url: &str,
    ) -> anyhow::Result<Self> {
        config.normalize();
        if !config.has_credentials() {
            bail!("Tadoku username and password are not configured");
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

    fn login_flow_url(&self) -> String {
        format!("{}/self-service/login/browser", self.auth_url)
    }

    fn expected_login_action_prefix(&self) -> String {
        format!("{}/self-service/login?", self.auth_url)
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
            "ory_kratos_session=; Max-Age=0; Path=/; Domain=.tadoku.app; Secure"
        } else {
            "ory_kratos_session=; Max-Age=0; Path=/"
        };
        self.cookies.add_cookie_str(expired, &url);
        Ok(())
    }

    async fn login(&mut self) -> anyhow::Result<()> {
        let flow_response = self
            .http
            .get(self.login_flow_url())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .context("Could not start Tadoku login")?;
        if !flow_response.status().is_success() {
            bail!("Could not start Tadoku login (HTTP {})", flow_response.status());
        }
        let flow: LoginFlow = flow_response
            .json()
            .await
            .context("Tadoku returned an invalid login flow")?;
        if !flow.ui.action.starts_with(&self.expected_login_action_prefix()) {
            bail!("Tadoku returned an invalid login flow");
        }
        let csrf_token = flow
            .ui
            .nodes
            .iter()
            .find(|node| node.attributes.name.as_deref() == Some("csrf_token"))
            .and_then(|node| node.attributes.value.as_ref())
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .context("Tadoku did not return a CSRF token")?;

        let login_response = self
            .http
            .post(&flow.ui.action)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("identifier", self.config.username.as_str()),
                ("password", self.config.password.as_str()),
                ("method", "password"),
                ("csrf_token", csrf_token),
            ])
            .send()
            .await
            .context("Could not complete Tadoku login")?;
        if !login_response.status().is_success() {
            bail!("Tadoku login failed; check the saved username and password");
        }
        let cookie = self
            .session_cookie()?
            .context("Tadoku login did not return a browser session cookie")?;
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

    async fn connection_info(&mut self, language_code: &str) -> anyhow::Result<TadokuConnectionInfo> {
        let session_url = self.config.session_url.clone();
        let session: SessionResponse = response_json(
            self.send_authenticated(|http| http.get(&session_url)).await?,
            "Tadoku authentication",
        )
        .await?;

        let options_url = self.api_url("logs/configuration-options");
        let options: ConfigurationOptions = response_json(
            self.send_authenticated(|http| http.get(&options_url)).await?,
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
        let minutes_unit = options
            .units
            .iter()
            .filter(|unit| {
                unit.log_activity_id == activity.id && unit.name.eq_ignore_ascii_case("minute")
            })
            .find(|unit| {
                unit.language_code
                    .as_deref()
                    .is_some_and(|code| code.eq_ignore_ascii_case(language_code))
            })
            .or_else(|| {
                options.units.iter().find(|unit| {
                    unit.log_activity_id == activity.id
                        && unit.name.eq_ignore_ascii_case("minute")
                        && unit.language_code.is_none()
                })
            })
            .context("Tadoku did not return a Minute unit for Listening")?;

        Ok(TadokuConnectionInfo {
            user_id: session.identity.id,
            display_name: session.identity.traits.display_name,
            listening_activity_id: activity.id,
            listening_minutes_unit_id: minutes_unit.id.clone(),
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
                if from_nagare && let Some(description) = log.description.clone() {
                    result.insert(description, log);
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
        listening_minutes_unit_id: &str,
        registrations: &[Registration],
    ) -> anyhow::Result<String> {
        let registration_ids = eligible_registration_ids(
            registrations,
            &batch.language_code,
            listening_activity_id,
        );
        let payload = CreateLogRequest {
            registration_ids,
            language_code: &batch.language_code,
            activity_id: listening_activity_id,
            amount: tadoku_minutes(batch.duration_seconds),
            unit_id: listening_minutes_unit_id,
            tags: ["nagare"],
            description: &batch.description,
        };
        let url = self.api_url("logs");
        let created: TadokuLog = response_json(
            self.send_authenticated(|http| http.post(&url).json(&payload))
                .await?,
            "Tadoku log creation",
        )
        .await?;
        self.ensure_log_minutes(created, batch, listening_minutes_unit_id)
            .await
    }

    async fn ensure_log_minutes(
        &mut self,
        log: TadokuLog,
        batch: &TadokuExportBatch,
        listening_minutes_unit_id: &str,
    ) -> anyhow::Result<String> {
        let minutes = tadoku_minutes(batch.duration_seconds);
        let amount_matches = log
            .amount
            .is_some_and(|amount| (amount - minutes).abs() < 0.001);
        let unit_matches = log.unit_id.as_deref() == Some(listening_minutes_unit_id);
        if amount_matches && unit_matches {
            return Ok(log.id);
        }

        warn!(
            "Tadoku log {} returned amount {:?} and unit {:?}; updating it to {:.1} minutes",
            log.id, log.amount, log.unit_id, minutes
        );
        let payload = UpdateLogRequest {
            amount: minutes,
            unit_id: listening_minutes_unit_id,
            tags: ["nagare"],
            description: &batch.description,
        };
        let url = self.api_url(&format!("logs/{}", log.id));
        let updated: TadokuLog = response_json(
            self.send_authenticated(|http| http.put(&url).json(&payload))
                .await?,
            "Tadoku log minutes correction",
        )
        .await?;
        let updated_amount_matches = updated
            .amount
            .is_some_and(|amount| (amount - minutes).abs() < 0.001);
        if !updated_amount_matches
            || updated.unit_id.as_deref() != Some(listening_minutes_unit_id)
        {
            bail!(
                "Tadoku returned amount {:?} and unit {:?} after updating log {} to {:.1} minutes",
                updated.amount,
                updated.unit_id,
                updated.id,
                minutes
            );
        }
        Ok(updated.id)
    }
}

fn tadoku_minutes(duration_seconds: i32) -> f64 {
    let seconds = i64::from(duration_seconds).max(1);
    let tenths = (seconds + 5) / 6;
    tenths as f64 / 10.0
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
        (name == "ory_kratos_session" && !value.is_empty()).then(|| value.to_string())
    })
}

fn is_tadoku_production_host(url: &reqwest::Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "tadoku.app" || host.ends_with(".tadoku.app"))
}

fn add_session_cookie(cookies: &Jar, auth_url: &str, value: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(auth_url).context("Invalid Tadoku auth URL")?;
    let cookie = if is_tadoku_production_host(&url) {
        format!("ory_kratos_session={value}; Path=/; Domain=.tadoku.app; Secure; HttpOnly")
    } else {
        format!("ory_kratos_session={value}; Path=/; HttpOnly")
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
    let mut client = TadokuClient::new(
        tadoku_config,
        Some(persistence(config, db)),
        true,
    )?;
    client.connection_info(&language_code).await
}

pub async fn refresh_authentication(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<()> {
    let tadoku_config = config.read().await.tadoku.clone();
    let mut client = TadokuClient::new(
        tadoku_config,
        Some(persistence(config, db)),
        false,
    )?;
    client.refresh_session().await
}

pub async fn export_once(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
) -> anyhow::Result<usize> {
    export(config, db, None).await
}

pub async fn export_selected(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
    history_ids: Vec<String>,
) -> anyhow::Result<usize> {
    if history_ids.is_empty() {
        bail!("Select at least one episode to sync");
    }
    export(config, db, Some(history_ids)).await
}

async fn export(
    config: Arc<RwLock<Config>>,
    db: Arc<AppDatabase>,
    selected_history_ids: Option<Vec<String>>,
) -> anyhow::Result<usize> {
    let tadoku_config = config.read().await.tadoku.clone();
    let language_code = tadoku_config.language_code.trim().to_ascii_lowercase();
    let mut client = TadokuClient::new(
        tadoku_config,
        Some(persistence(config, db.clone())),
        true,
    )?;
    let connection = client.connection_info(&language_code).await?;
    let registrations = client.registrations().await?;
    let eastern_date = eastern_time(Utc::now()).date_naive().to_string();
    let batches = match selected_history_ids {
        Some(history_ids) => {
            db.prepare_selected_tadoku_batches(eastern_date, language_code, history_ids)
                .await?
        }
        None => {
            db.prepare_tadoku_batches(eastern_date, language_code)
                .await?
        }
    };
    if batches.is_empty() {
        info!("Tadoku export found no eligible episodes");
        return Ok(0);
    }

    let existing = client.existing_nagare_logs(&connection.user_id).await?;
    let mut completed = 0usize;
    let mut failures = Vec::new();
    for batch in batches {
        let result = if let Some(log) = existing.get(&batch.description) {
            info!(
                "Tadoku batch {} already exists remotely as {}; verifying it locally",
                batch.batch_id, log.id
            );
            client
                .ensure_log_minutes(
                    log.clone(),
                    &batch,
                    &connection.listening_minutes_unit_id,
                )
                .await
        } else {
            client
                .create_log(
                    &batch,
                    connection.listening_activity_id,
                    &connection.listening_minutes_unit_id,
                    &registrations,
                )
                .await
        };

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

pub async fn run_exporter(config: Arc<RwLock<Config>>, db: Arc<AppDatabase>) {
    loop {
        let tadoku_config = config.read().await.tadoku.clone();
        if tadoku_config.enabled {
            let now_eastern = eastern_time(Utc::now());
            let export_hour = tadoku_config.export_hour_eastern.min(23);
            if now_eastern.hour() >= export_hour {
                let export_date = now_eastern.date_naive().to_string();
                match db.tadoku_export_due(export_date.clone()).await {
                    Ok(true) => {
                        if let Err(error) = db.mark_tadoku_run_started(export_date.clone()).await {
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
        eastern_time, eligible_registration_ids, extract_cookie_value, tadoku_minutes,
    };
    use crate::config::TadokuConfig;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use chrono::{TimeZone, Timelike, Utc};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct MockTadoku {
        base_url: String,
        trusted_action: bool,
        include_csrf: bool,
        set_session_cookie: bool,
        reject_first_options_request: bool,
        login_count: AtomicUsize,
        options_count: AtomicUsize,
        login_forms: Mutex<Vec<String>>,
        request_cookies: Mutex<Vec<String>>,
    }

    async fn login_flow(State(state): State<Arc<MockTadoku>>) -> Response {
        let action = if state.trusted_action {
            format!("{}/self-service/login?flow=test", state.base_url)
        } else {
            "https://malicious.example/self-service/login?flow=test".to_string()
        };
        let nodes = if state.include_csrf {
            json!([{"attributes": {"name": "csrf_token", "value": "csrf-value"}}])
        } else {
            json!([])
        };
        let mut response = Json(json!({"ui": {"action": action, "nodes": nodes}})).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("csrf_cookie=csrf-cookie; Path=/; HttpOnly"),
        );
        response
    }

    async fn submit_login(
        State(state): State<Arc<MockTadoku>>,
        headers: HeaderMap,
        body: String,
    ) -> Response {
        state.login_count.fetch_add(1, Ordering::SeqCst);
        state.login_forms.lock().unwrap().push(body);
        if let Some(cookie) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) {
            state.request_cookies.lock().unwrap().push(cookie.to_string());
        }
        let mut response = Response::new(Body::from("{}"));
        if state.set_session_cookie {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("ory_kratos_session=fresh-session; Path=/; HttpOnly"),
            );
        }
        response
    }

    async fn whoami(State(state): State<Arc<MockTadoku>>, headers: HeaderMap) -> Response {
        if let Some(cookie) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) {
            state.request_cookies.lock().unwrap().push(cookie.to_string());
        }
        Json(json!({
            "identity": {"id": "user-id", "traits": {"display_name": "Reader"}}
        }))
        .into_response()
    }

    async fn configuration_options(
        State(state): State<Arc<MockTadoku>>,
        headers: HeaderMap,
    ) -> Response {
        if let Some(cookie) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) {
            state.request_cookies.lock().unwrap().push(cookie.to_string());
        }
        let call = state.options_count.fetch_add(1, Ordering::SeqCst);
        if state.reject_first_options_request && call == 0 {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Json(json!({
            "activities": [{"id": 2, "name": "Listening"}],
            "languages": [{"code": "jpn"}],
            "units": [{
                "id": "minutes", "log_activity_id": 2,
                "name": "Minute", "language_code": "jpn"
            }]
        }))
        .into_response()
    }

    async fn spawn_mock_tadoku(
        trusted_action: bool,
        include_csrf: bool,
        set_session_cookie: bool,
        reject_first_options_request: bool,
    ) -> (Arc<MockTadoku>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(MockTadoku {
            base_url,
            trusted_action,
            include_csrf,
            set_session_cookie,
            reject_first_options_request,
            login_count: AtomicUsize::new(0),
            options_count: AtomicUsize::new(0),
            login_forms: Mutex::new(Vec::new()),
            request_cookies: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/self-service/login/browser", get(login_flow))
            .route("/self-service/login", post(submit_login))
            .route("/sessions/whoami", get(whoami))
            .route("/logs/configuration-options", get(configuration_options))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, server)
    }

    fn mock_config(state: &MockTadoku) -> TadokuConfig {
        TadokuConfig {
            username: "reader".to_string(),
            password: "super-secret".to_string(),
            api_url: state.base_url.clone(),
            session_url: format!("{}/sessions/whoami", state.base_url),
            ..TadokuConfig::default()
        }
    }

    #[test]
    fn extracts_cookie_values_and_headers() {
        assert_eq!(extract_cookie_value("abc").unwrap(), "abc");
        assert_eq!(
            extract_cookie_value("ory_kratos_session=abc").unwrap(),
            "abc"
        );
        assert!(extract_cookie_value("abc\ndef").is_err());
    }

    #[test]
    fn missing_credentials_are_rejected_before_client_creation() {
        let config = TadokuConfig::default();
        let error = TadokuClient::new(config, None, true).err().unwrap();
        assert_eq!(
            error.to_string(),
            "Tadoku username and password are not configured"
        );
    }

    #[tokio::test]
    async fn browser_login_submits_credentials_and_csrf_as_form_data() {
        let (state, server) = spawn_mock_tadoku(true, true, true, false).await;
        let mut client = TadokuClient::new_with_auth_url(
            mock_config(&state), None, false, &state.base_url,
        ).unwrap();
        let connection = client.connection_info("jpn").await.unwrap();
        assert_eq!(connection.user_id, "user-id");
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        let forms = state.login_forms.lock().unwrap();
        let form = forms.first().unwrap();
        assert!(form.contains("identifier=reader"));
        assert!(form.contains("password=super-secret"));
        assert!(form.contains("method=password"));
        assert!(form.contains("csrf_token=csrf-value"));
        assert_eq!(client.session_cookie().unwrap().as_deref(), Some("fresh-session"));
        drop(forms);
        server.abort();
    }

    #[tokio::test]
    async fn rejects_untrusted_login_action_before_submitting_credentials() {
        let (state, server) = spawn_mock_tadoku(false, true, true, false).await;
        let mut client = TadokuClient::new_with_auth_url(
            mock_config(&state), None, false, &state.base_url,
        ).unwrap();
        let error = client.connection_info("jpn").await.unwrap_err();
        assert_eq!(error.to_string(), "Tadoku returned an invalid login flow");
        assert_eq!(state.login_count.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn rejects_login_flow_without_csrf_token() {
        let (state, server) = spawn_mock_tadoku(true, false, true, false).await;
        let mut client = TadokuClient::new_with_auth_url(
            mock_config(&state), None, false, &state.base_url,
        ).unwrap();
        let error = client.connection_info("jpn").await.unwrap_err();
        assert_eq!(error.to_string(), "Tadoku did not return a CSRF token");
        assert_eq!(state.login_count.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn successful_login_requires_browser_session_cookie() {
        let (state, server) = spawn_mock_tadoku(true, true, false, false).await;
        let mut client = TadokuClient::new_with_auth_url(
            mock_config(&state), None, false, &state.base_url,
        ).unwrap();
        let error = client.connection_info("jpn").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Tadoku login did not return a browser session cookie"
        );
        server.abort();
    }

    #[tokio::test]
    async fn reuses_saved_cookie_without_login() {
        let (state, server) = spawn_mock_tadoku(true, true, true, false).await;
        let mut config = mock_config(&state);
        config.session_cookie = "saved-session".to_string();
        let mut client = TadokuClient::new_with_auth_url(
            config, None, true, &state.base_url,
        ).unwrap();
        client.connection_info("jpn").await.unwrap();
        assert_eq!(state.login_count.load(Ordering::SeqCst), 0);
        assert!(
            state
                .request_cookies
                .lock()
                .unwrap()
                .iter()
                .any(|cookie| cookie.contains("ory_kratos_session=saved-session"))
        );
        server.abort();
    }

    #[tokio::test]
    async fn unauthorized_request_reauthenticates_and_retries_once() {
        let (state, server) = spawn_mock_tadoku(true, true, true, true).await;
        let mut config = mock_config(&state);
        config.session_cookie = "expired-session".to_string();
        let mut client = TadokuClient::new_with_auth_url(
            config, None, true, &state.base_url,
        ).unwrap();
        client.connection_info("jpn").await.unwrap();
        assert_eq!(state.options_count.load(Ordering::SeqCst), 2);
        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(client.session_cookie().unwrap().as_deref(), Some("fresh-session"));
        server.abort();
    }

    #[test]
    fn serializes_minutes_rounded_up_to_one_decimal_for_tadoku() {
        assert_eq!(tadoku_minutes(1_410), 23.5);
        assert_eq!(tadoku_minutes(1_415), 23.6);
        let payload = CreateLogRequest {
            registration_ids: vec!["registration".to_string()],
            language_code: "jpn",
            activity_id: 2,
            amount: tadoku_minutes(1_415),
            unit_id: "minute-unit",
            tags: ["nagare"],
            description: "NARUTO 疾風伝 S10E10-13",
        };
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(json["amount"], 23.6);
        assert_eq!(json["unit_id"], "minute-unit");
        assert!(json.get("duration_seconds").is_none());
        assert_eq!(json["description"], "NARUTO 疾風伝 S10E10-13");
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
            assert_eq!(
                config.api_url,
                "https://tadoku.app/api/internal/immersion"
            );
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
