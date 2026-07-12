use crate::config::{Config, TadokuConfig};
use crate::mining::{AppDatabase, TadokuExportBatch};
use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use reqwest::header::COOKIE;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

#[derive(Debug, Serialize)]
pub struct TadokuConnectionInfo {
    pub user_id: String,
    pub display_name: Option<String>,
    pub listening_activity_id: i32,
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

#[derive(Debug, Deserialize)]
struct TadokuLog {
    id: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreatedLog {
    id: String,
}

#[derive(Debug, Serialize)]
struct CreateLogRequest<'a> {
    registration_ids: Vec<String>,
    language_code: &'a str,
    activity_id: i32,
    duration_seconds: i32,
    tags: [&'static str; 1],
    description: &'a str,
}

struct TadokuClient {
    http: reqwest::Client,
    config: TadokuConfig,
    cookie: String,
}

impl TadokuClient {
    fn new(config: TadokuConfig) -> anyhow::Result<Self> {
        let cookie = normalize_cookie(&config.session_cookie)?;
        if config.api_url.trim().is_empty() {
            bail!("Tadoku API URL is empty");
        }
        if config.session_url.trim().is_empty() {
            bail!("Tadoku session URL is empty");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Nagare/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            config,
            cookie,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.api_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn connection_info(&self, language_code: &str) -> anyhow::Result<TadokuConnectionInfo> {
        let session: SessionResponse = response_json(
            self.http
                .get(self.config.session_url.trim())
                .header(COOKIE, &self.cookie)
                .send()
                .await?,
            "Tadoku authentication",
        )
        .await?;

        let options: ConfigurationOptions = response_json(
            self.http
                .get(self.api_url("logs/configuration-options"))
                .header(COOKIE, &self.cookie)
                .send()
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

        Ok(TadokuConnectionInfo {
            user_id: session.identity.id,
            display_name: session.identity.traits.display_name,
            listening_activity_id: activity.id,
        })
    }

    async fn registrations(&self) -> anyhow::Result<Vec<Registration>> {
        let response: RegistrationsResponse = response_json(
            self.http
                .get(self.api_url("contests/ongoing-registrations"))
                .header(COOKIE, &self.cookie)
                .send()
                .await?,
            "Tadoku contest registrations",
        )
        .await?;
        Ok(response.registrations)
    }

    async fn existing_nagare_logs(&self, user_id: &str) -> anyhow::Result<HashMap<String, String>> {
        let mut result = HashMap::new();
        let mut page = 0usize;
        loop {
            let response: LogsResponse = response_json(
                self.http
                    .get(self.api_url(&format!("users/{user_id}/logs")))
                    .query(&[("page_size", "50"), ("page", &page.to_string())])
                    .header(COOKIE, &self.cookie)
                    .send()
                    .await?,
                "Tadoku log history",
            )
            .await?;
            let returned = response.logs.len();
            for log in response.logs {
                if let Some(batch_id) = log.description.as_deref().and_then(extract_batch_id) {
                    result.insert(batch_id.to_string(), log.id);
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
        &self,
        batch: &TadokuExportBatch,
        listening_activity_id: i32,
        registrations: &[Registration],
    ) -> anyhow::Result<String> {
        let registration_ids = registrations
            .iter()
            .filter(|registration| {
                let language_allowed = registration
                    .languages
                    .iter()
                    .any(|language| language.code.eq_ignore_ascii_case(&batch.language_code));
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
            .collect();
        let payload = CreateLogRequest {
            registration_ids,
            language_code: &batch.language_code,
            activity_id: listening_activity_id,
            duration_seconds: batch.duration_seconds,
            tags: ["nagare"],
            description: &batch.description,
        };
        let created: CreatedLog = response_json(
            self.http
                .post(self.api_url("logs"))
                .header(COOKIE, &self.cookie)
                .json(&payload)
                .send()
                .await?,
            "Tadoku log creation",
        )
        .await?;
        Ok(created.id)
    }
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

fn normalize_cookie(raw: &str) -> anyhow::Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Tadoku session cookie is empty");
    }
    if raw.contains(['\r', '\n']) {
        bail!("Tadoku session cookie contains an invalid newline");
    }
    if raw.contains("ory_kratos_session=") {
        Ok(raw.to_string())
    } else {
        Ok(format!("ory_kratos_session={raw}"))
    }
}

fn extract_batch_id(description: &str) -> Option<&str> {
    let marker = "[Nagare:";
    let start = description.rfind(marker)? + marker.len();
    let rest = &description[start..];
    let end = rest.find(']')?;
    let id = &rest[..end];
    (!id.is_empty()).then_some(id)
}

pub async fn test_connection(config: TadokuConfig) -> anyhow::Result<TadokuConnectionInfo> {
    let language_code = config.language_code.trim().to_ascii_lowercase();
    TadokuClient::new(config)?
        .connection_info(&language_code)
        .await
}

pub async fn export_once(config: TadokuConfig, db: Arc<AppDatabase>) -> anyhow::Result<usize> {
    let language_code = config.language_code.trim().to_ascii_lowercase();
    let client = TadokuClient::new(config)?;
    let connection = client.connection_info(&language_code).await?;
    let registrations = client.registrations().await?;
    let eastern_date = eastern_time(Utc::now()).date_naive().to_string();
    let batches = db
        .prepare_tadoku_batches(eastern_date, language_code)
        .await?;
    if batches.is_empty() {
        info!("Tadoku export found no newly completed episodes");
        return Ok(0);
    }

    let existing = client.existing_nagare_logs(&connection.user_id).await?;
    let mut completed = 0usize;
    let mut failures = Vec::new();
    for batch in batches {
        let result = if let Some(log_id) = existing.get(&batch.batch_id) {
            info!(
                "Tadoku batch {} already exists remotely as {}; recording it locally",
                batch.batch_id, log_id
            );
            Ok(log_id.clone())
        } else {
            client
                .create_log(&batch, connection.listening_activity_id, &registrations)
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
                            let result = export_once(tadoku_config, db.clone()).await;
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
    use super::{eastern_time, extract_batch_id, normalize_cookie};
    use chrono::{TimeZone, Timelike, Utc};

    #[test]
    fn normalizes_cookie_values_and_headers() {
        assert_eq!(normalize_cookie("abc").unwrap(), "ory_kratos_session=abc");
        assert_eq!(
            normalize_cookie("ory_kratos_session=abc").unwrap(),
            "ory_kratos_session=abc"
        );
        assert!(normalize_cookie("abc\ndef").is_err());
    }

    #[test]
    fn extracts_deduplication_marker() {
        assert_eq!(
            extract_batch_id("Frieren (2 episodes) [Nagare:batch-123]"),
            Some("batch-123")
        );
        assert_eq!(extract_batch_id("Frieren"), None);
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
