use crate::anki::NewCardEvent;
use crate::config::{Config, MediaServerKind};
use crate::session::HistoryEntry;
use crate::subtitle::SubtitleTrack;
use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentSource {
    #[default]
    Pending,
    MiningHistory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentDialogState {
    pub event: NewCardEvent,
    pub matched_line_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_avif: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_line_first: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_line_last: Option<usize>,
    #[serde(default)]
    pub card_ids: Vec<i64>,
    #[serde(default)]
    pub source: EnrichmentSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningHistoryEntry {
    pub note_id: i64,
    pub card_ids: Vec<i64>,
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub item_id: String,
    pub media_source_id: String,
    pub file_path: Option<String>,
    pub title: String,
    pub event: NewCardEvent,
    pub start_ms: i64,
    pub end_ms: i64,
    pub generate_avif: bool,
    pub matched_line_index: Option<usize>,
    pub included_line_first: Option<usize>,
    pub included_line_last: Option<usize>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningHistorySummary {
    pub note_id: i64,
    pub card_ids: Vec<i64>,
    pub title: String,
    pub sentence: String,
    pub history_id: String,
    pub server_kind: MediaServerKind,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TadokuExportBatch {
    pub batch_id: String,
    pub series_name: String,
    pub description: String,
    pub duration_seconds: i32,
    pub language_code: String,
}

impl MiningHistoryEntry {
    pub fn dialog_state(&self) -> EnrichmentDialogState {
        EnrichmentDialogState {
            event: self.event.clone(),
            matched_line_index: self.matched_line_index,
            history_id: Some(self.history_id.clone()),
            start_ms: Some(self.start_ms),
            end_ms: Some(self.end_ms),
            generate_avif: Some(self.generate_avif),
            included_line_first: self.included_line_first,
            included_line_last: self.included_line_last,
            card_ids: self.card_ids.clone(),
            source: EnrichmentSource::MiningHistory,
            updated_at: Some(self.updated_at),
        }
    }
}

pub struct AppDatabase {
    db_path: PathBuf,
}

impl AppDatabase {
    pub async fn new(db_path: PathBuf, legacy_db_path: Option<PathBuf>) -> anyhow::Result<Self> {
        let init_path = db_path.clone();
        tokio::task::spawn_blocking(move || init_database(&init_path, legacy_db_path.as_deref()))
            .await
            .context("SQLite init task failed")??;
        Ok(Self { db_path })
    }

    pub async fn load_config_or_default(
        &self,
        legacy_config_path: PathBuf,
    ) -> anyhow::Result<Config> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            load_config_or_default_sync(&db_path, &legacy_config_path)
        })
        .await
        .context("SQLite config load task failed")?
    }

    pub async fn save_config(&self, config: Config) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || save_config_sync(&db_path, &config))
            .await
            .context("SQLite config save task failed")??;
        Ok(())
    }

    pub async fn load_session_history(
        &self,
        legacy_history_path: PathBuf,
        legacy_subtitle_history_path: PathBuf,
    ) -> anyhow::Result<(
        HashMap<String, HistoryEntry>,
        HashMap<String, SubtitleTrack>,
    )> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            load_session_history_sync(
                &db_path,
                &legacy_history_path,
                &legacy_subtitle_history_path,
            )
        })
        .await
        .context("SQLite history load task failed")?
    }

    pub async fn save_session_history(
        &self,
        history: HashMap<String, HistoryEntry>,
        subtitle_history: Option<HashMap<String, SubtitleTrack>>,
    ) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            save_session_history_sync(&db_path, &history, subtitle_history.as_ref())
        })
        .await
        .context("SQLite history save task failed")??;
        Ok(())
    }

    pub async fn upsert_mined_note(&self, entry: MiningHistoryEntry) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || upsert_mined_note_sync(&db_path, entry))
            .await
            .context("SQLite mined-note write task failed")??;
        Ok(())
    }

    pub async fn list_mined_notes(&self) -> anyhow::Result<Vec<MiningHistorySummary>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || list_mined_notes_sync(&db_path))
            .await
            .context("SQLite mined-note list task failed")?
    }

    pub async fn get_mined_note_by_note_id(
        &self,
        note_id: i64,
    ) -> anyhow::Result<Option<MiningHistoryEntry>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || get_mined_note_by_note_id_sync(&db_path, note_id))
            .await
            .context("SQLite mined-note note lookup task failed")?
    }

    pub async fn get_mined_note_by_card_id(
        &self,
        card_id: i64,
    ) -> anyhow::Result<Option<MiningHistoryEntry>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || get_mined_note_by_card_id_sync(&db_path, card_id))
            .await
            .context("SQLite mined-note card lookup task failed")?
    }

    pub async fn prepare_tadoku_batches(
        &self,
        export_date: String,
        language_code: String,
    ) -> anyhow::Result<Vec<TadokuExportBatch>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            prepare_tadoku_batches_sync(&db_path, &export_date, &language_code)
        })
        .await
        .context("SQLite Tadoku batch preparation task failed")?
    }

    pub async fn mark_tadoku_batch_completed(
        &self,
        batch_id: String,
        log_id: String,
    ) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = open_connection(&db_path)?;
            conn.execute(
                "UPDATE tadoku_export_batches SET status = 'completed', tadoku_log_id = ?2, last_error = NULL, updated_at = ?3 WHERE batch_id = ?1",
                params![batch_id, log_id, Utc::now().to_rfc3339()],
            )?;
            Ok(())
        })
        .await
        .context("SQLite Tadoku completion task failed")?
    }

    pub async fn mark_tadoku_batch_failed(
        &self,
        batch_id: String,
        message: String,
    ) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = open_connection(&db_path)?;
            conn.execute(
                "UPDATE tadoku_export_batches SET last_error = ?2, updated_at = ?3 WHERE batch_id = ?1",
                params![batch_id, message, Utc::now().to_rfc3339()],
            )?;
            Ok(())
        })
        .await
        .context("SQLite Tadoku failure task failed")?
    }

    pub async fn tadoku_export_due(&self, export_date: String) -> anyhow::Result<bool> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = open_connection(&db_path)?;
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT status, last_attempt_at FROM tadoku_export_runs WHERE export_date = ?1",
                    [&export_date],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((status, last_attempt)) = row else {
                return Ok(true);
            };
            if status == "completed" {
                return Ok(false);
            }
            let last_attempt = parse_timestamp(&last_attempt)?;
            Ok(Utc::now().signed_duration_since(last_attempt) >= chrono::Duration::minutes(30))
        })
        .await
        .context("SQLite Tadoku due check task failed")?
    }

    pub async fn mark_tadoku_run_started(&self, export_date: String) -> anyhow::Result<()> {
        self.save_tadoku_run(export_date, "running", None).await
    }

    pub async fn mark_tadoku_run_finished(
        &self,
        export_date: String,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let status = if error.is_some() {
            "failed"
        } else {
            "completed"
        };
        self.save_tadoku_run(export_date, status, error).await
    }

    async fn save_tadoku_run(
        &self,
        export_date: String,
        status: &'static str,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = open_connection(&db_path)?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO tadoku_export_runs (export_date, status, last_attempt_at, last_error) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(export_date) DO UPDATE SET status = excluded.status, last_attempt_at = excluded.last_attempt_at, last_error = excluded.last_error",
                params![export_date, status, now, error],
            )?;
            Ok(())
        })
        .await
        .context("SQLite Tadoku run update task failed")?
    }
}

fn init_database(path: &Path, legacy_db_path: Option<&Path>) -> anyhow::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create DB dir {}", parent.display()))?;
        }

        if let Some(legacy) = legacy_db_path {
            if legacy.exists() && legacy != path {
                std::fs::copy(legacy, path).with_context(|| {
                    format!(
                        "Failed to copy legacy DB {} -> {}",
                        legacy.display(),
                        path.display()
                    )
                })?;
            }
        }
    }

    open_connection(path).map(|_| ())
}

fn open_connection(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create DB dir {}", parent.display()))?;
    }

    let conn =
        Connection::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS app_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            config_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS media_history (
            history_id TEXT PRIMARY KEY,
            server_kind TEXT NOT NULL,
            item_id TEXT NOT NULL,
            title TEXT NOT NULL,
            series_name TEXT,
            media_source_id TEXT NOT NULL,
            file_path TEXT,
            duration_ms INTEGER,
            subtitle_count INTEGER NOT NULL,
            last_position_ms INTEGER NOT NULL,
            last_seen TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_media_history_last_seen ON media_history(last_seen DESC);

        CREATE TABLE IF NOT EXISTS subtitle_tracks (
            history_id TEXT PRIMARY KEY,
            track_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mined_notes (
            note_id INTEGER PRIMARY KEY,
            card_ids_json TEXT NOT NULL,
            history_id TEXT NOT NULL,
            server_kind TEXT NOT NULL,
            item_id TEXT NOT NULL,
            media_source_id TEXT NOT NULL,
            file_path TEXT,
            title TEXT NOT NULL,
            event_json TEXT NOT NULL,
            start_ms INTEGER NOT NULL,
            end_ms INTEGER NOT NULL,
            generate_avif INTEGER NOT NULL,
            matched_line_index INTEGER,
            included_line_first INTEGER,
            included_line_last INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mined_note_cards (
            card_id INTEGER PRIMARY KEY,
            note_id INTEGER NOT NULL REFERENCES mined_notes(note_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_mined_notes_updated_at ON mined_notes(updated_at DESC);

        CREATE TABLE IF NOT EXISTS tadoku_export_batches (
            batch_id TEXT PRIMARY KEY,
            export_date TEXT NOT NULL,
            series_name TEXT NOT NULL,
            description TEXT NOT NULL,
            duration_seconds INTEGER NOT NULL,
            language_code TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            tadoku_log_id TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tadoku_batches_status ON tadoku_export_batches(status, created_at);

        CREATE TABLE IF NOT EXISTS tadoku_export_items (
            history_id TEXT PRIMARY KEY,
            batch_id TEXT NOT NULL REFERENCES tadoku_export_batches(batch_id),
            watched_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tadoku_export_runs (
            export_date TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            last_attempt_at TEXT NOT NULL,
            last_error TEXT
        );
        ",
    )
    .context("Failed to initialize SQLite schema")?;

    // Migrations for columns added after the initial schema shipped.
    add_column_if_missing(&conn, "media_history", "series_name", "TEXT")?;

    Ok(conn)
}

/// Add `column` to `table` if it is not already present. SQLite's
/// `CREATE TABLE IF NOT EXISTS` never alters an existing table, so new columns
/// need an explicit, idempotent migration.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    drop(stmt);

    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )
        .with_context(|| format!("Failed to add column {column} to {table}"))?;
    }
    Ok(())
}

fn load_config_or_default_sync(
    db_path: &Path,
    legacy_config_path: &Path,
) -> anyhow::Result<Config> {
    let conn = open_connection(db_path)?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT config_json FROM app_config WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(raw) = raw {
        return serde_json::from_str(&raw).context("Failed to parse config from SQLite");
    }

    let config = Config::load_or_default(legacy_config_path);
    save_config_conn(&conn, &config)?;
    Ok(config)
}

fn save_config_sync(db_path: &Path, config: &Config) -> anyhow::Result<()> {
    let conn = open_connection(db_path)?;
    save_config_conn(&conn, config)
}

fn save_config_conn(conn: &Connection, config: &Config) -> anyhow::Result<()> {
    conn.execute(
        "
        INSERT INTO app_config (id, config_json, updated_at)
        VALUES (1, ?1, ?2)
        ON CONFLICT(id) DO UPDATE SET
            config_json = excluded.config_json,
            updated_at = excluded.updated_at
        ",
        params![serde_json::to_string(config)?, Utc::now().to_rfc3339()],
    )
    .context("Failed to save config to SQLite")?;
    Ok(())
}

fn load_session_history_sync(
    db_path: &Path,
    legacy_history_path: &Path,
    legacy_subtitle_history_path: &Path,
) -> anyhow::Result<(
    HashMap<String, HistoryEntry>,
    HashMap<String, SubtitleTrack>,
)> {
    let mut conn = open_connection(db_path)?;
    let history_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM media_history", [], |row| row.get(0))?;

    if history_count == 0 {
        let history = load_json_or_default::<HashMap<String, HistoryEntry>>(legacy_history_path);
        let subtitle_history =
            load_json_or_default::<HashMap<String, SubtitleTrack>>(legacy_subtitle_history_path);
        if !history.is_empty() || !subtitle_history.is_empty() {
            save_session_history_conn(&mut conn, &history, Some(&subtitle_history))?;
        }
    }

    Ok((load_history_map(&conn)?, load_subtitle_map(&conn)?))
}

fn save_session_history_sync(
    db_path: &Path,
    history: &HashMap<String, HistoryEntry>,
    subtitle_history: Option<&HashMap<String, SubtitleTrack>>,
) -> anyhow::Result<()> {
    let mut conn = open_connection(db_path)?;
    save_session_history_conn(&mut conn, history, subtitle_history)
}

fn save_session_history_conn(
    conn: &mut Connection,
    history: &HashMap<String, HistoryEntry>,
    subtitle_history: Option<&HashMap<String, SubtitleTrack>>,
) -> anyhow::Result<()> {
    let tx = conn
        .transaction()
        .context("Failed to start SQLite history transaction")?;
    let now = Utc::now().to_rfc3339();

    for entry in history.values() {
        tx.execute(
            "
            INSERT INTO media_history (
                history_id,
                server_kind,
                item_id,
                title,
                series_name,
                media_source_id,
                file_path,
                duration_ms,
                subtitle_count,
                last_position_ms,
                last_seen
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(history_id) DO UPDATE SET
                server_kind = excluded.server_kind,
                item_id = excluded.item_id,
                title = excluded.title,
                series_name = excluded.series_name,
                media_source_id = excluded.media_source_id,
                file_path = excluded.file_path,
                duration_ms = excluded.duration_ms,
                subtitle_count = excluded.subtitle_count,
                last_position_ms = excluded.last_position_ms,
                last_seen = excluded.last_seen
            ",
            params![
                entry.history_id,
                entry.server_kind.as_str(),
                entry.item_id,
                entry.title,
                entry.series_name,
                entry.media_source_id,
                entry.file_path,
                entry.duration_ms,
                entry.subtitle_count as i64,
                entry.last_position_ms,
                entry.last_seen.to_rfc3339(),
            ],
        )
        .with_context(|| format!("Failed to upsert history {}", entry.history_id))?;
    }

    if let Some(subtitle_history) = subtitle_history {
        for (history_id, track) in subtitle_history {
            tx.execute(
                "
                INSERT INTO subtitle_tracks (history_id, track_json, updated_at)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(history_id) DO UPDATE SET
                    track_json = excluded.track_json,
                    updated_at = excluded.updated_at
                ",
                params![history_id, serde_json::to_string(track)?, now],
            )
            .with_context(|| format!("Failed to upsert subtitle track {}", history_id))?;
        }
    }

    tx.commit()
        .context("Failed to commit SQLite history transaction")?;
    Ok(())
}

fn prepare_tadoku_batches_sync(
    db_path: &Path,
    export_date: &str,
    language_code: &str,
) -> anyhow::Result<Vec<TadokuExportBatch>> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .context("Failed to start Tadoku export transaction")?;

    let mut groups: BTreeMap<String, Vec<(String, String, i64)>> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "
            SELECT
                history_id,
                COALESCE(NULLIF(series_name, ''), title) AS show_name,
                title,
                last_seen,
                MIN(MAX(last_position_ms, 0), duration_ms) AS watched_ms
            FROM media_history
            WHERE duration_ms IS NOT NULL
              AND duration_ms > 0
              AND last_position_ms > 0
              AND last_position_ms * 100 >= duration_ms * 80
              AND NOT EXISTS (
                  SELECT 1 FROM tadoku_export_items tei
                  WHERE tei.history_id = media_history.history_id
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM tadoku_export_items tei
                  JOIN media_history exported ON exported.history_id = tei.history_id
                  WHERE LOWER(COALESCE(NULLIF(exported.series_name, ''), exported.title)) =
                        LOWER(COALESCE(NULLIF(media_history.series_name, ''), media_history.title))
                    AND LOWER(exported.title) = LOWER(media_history.title)
              )
              AND NOT EXISTS (
                  SELECT 1 FROM tadoku_export_batches teb
                  WHERE teb.status = 'pending'
                    AND teb.series_name = COALESCE(NULLIF(media_history.series_name, ''), media_history.title)
                    AND teb.language_code = ?1
              )
            ORDER BY show_name, last_seen
            ",
        )?;
        let rows = stmt.query_map([language_code], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut seen_content = HashSet::new();
        for row in rows {
            let (history_id, show_name, title, watched_at, watched_ms) = row?;
            let content_key = format!(
                "{}\u{1f}{}",
                show_name.trim().to_lowercase(),
                title.trim().to_lowercase()
            );
            if !seen_content.insert(content_key) {
                continue;
            }
            groups
                .entry(show_name)
                .or_default()
                .push((history_id, watched_at, watched_ms));
        }
    }

    let now = Utc::now().to_rfc3339();
    for (series_name, items) in groups {
        let batch_id = uuid::Uuid::new_v4().to_string();
        let duration_seconds = items
            .iter()
            .map(|(_, _, watched_ms)| watched_ms / 1_000)
            .sum::<i64>()
            .clamp(1, i32::MAX as i64) as i32;
        let description = tadoku_batch_description(&series_name, items.len(), &batch_id);

        tx.execute(
            "
            INSERT INTO tadoku_export_batches (
                batch_id, export_date, series_name, description,
                duration_seconds, language_code, status, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7)
            ",
            params![
                batch_id,
                export_date,
                series_name,
                description,
                duration_seconds,
                language_code,
                now,
            ],
        )?;

        for (history_id, watched_at, _) in items {
            tx.execute(
                "INSERT INTO tadoku_export_items (history_id, batch_id, watched_at) VALUES (?1, ?2, ?3)",
                params![history_id, batch_id, watched_at],
            )?;
        }
    }

    tx.commit()
        .context("Failed to commit Tadoku export transaction")?;

    let conn = open_connection(db_path)?;
    let mut stmt = conn.prepare(
        "
        SELECT batch_id, series_name, description, duration_seconds, language_code
        FROM tadoku_export_batches
        WHERE status = 'pending'
        ORDER BY created_at, series_name
        ",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TadokuExportBatch {
            batch_id: row.get(0)?,
            series_name: row.get(1)?,
            description: row.get(2)?,
            duration_seconds: row.get(3)?,
            language_code: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn tadoku_batch_description(series_name: &str, episode_count: usize, batch_id: &str) -> String {
    let noun = if episode_count == 1 {
        "episode"
    } else {
        "episodes"
    };
    format!(
        "{} ({} {}) [Nagare:{}]",
        series_name, episode_count, noun, batch_id
    )
}

fn load_history_map(conn: &Connection) -> anyhow::Result<HashMap<String, HistoryEntry>> {
    let mut stmt = conn.prepare(
        "
        SELECT
            history_id,
            server_kind,
            item_id,
            title,
            media_source_id,
            file_path,
            duration_ms,
            subtitle_count,
            last_position_ms,
            last_seen,
            series_name
        FROM media_history
        ",
    )?;

    let rows = stmt.query_map([], |row| {
        let server_kind_raw: String = row.get(1)?;
        let last_seen_raw: String = row.get(9)?;
        let subtitle_count: i64 = row.get(7)?;
        Ok(HistoryEntry {
            history_id: row.get(0)?,
            server_kind: MediaServerKind::parse(&server_kind_raw).ok_or_else(|| {
                to_sql_error(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unknown server kind '{}'", server_kind_raw),
                ))
            })?,
            item_id: row.get(2)?,
            title: row.get(3)?,
            series_name: row.get(10)?,
            media_source_id: row.get(4)?,
            file_path: row.get(5)?,
            duration_ms: row.get(6)?,
            subtitle_count: subtitle_count as usize,
            last_position_ms: row.get(8)?,
            last_seen: parse_timestamp(&last_seen_raw).map_err(to_sql_error)?,
        })
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let entry = row?;
        map.insert(entry.history_id.clone(), entry);
    }
    Ok(map)
}

fn load_subtitle_map(conn: &Connection) -> anyhow::Result<HashMap<String, SubtitleTrack>> {
    let mut stmt = conn.prepare("SELECT history_id, track_json FROM subtitle_tracks")?;
    let rows = stmt.query_map([], |row| {
        let history_id: String = row.get(0)?;
        let track_json: String = row.get(1)?;
        let track: SubtitleTrack = serde_json::from_str(&track_json).map_err(to_sql_error)?;
        Ok((history_id, track))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (history_id, track) = row?;
        map.insert(history_id, track);
    }
    Ok(map)
}

fn upsert_mined_note_sync(db_path: &Path, entry: MiningHistoryEntry) -> anyhow::Result<()> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .context("Failed to start SQLite mined-note transaction")?;

    let card_ids_json = serde_json::to_string(&entry.card_ids)?;
    let event_json = serde_json::to_string(&entry.event)?;

    tx.execute(
        "
        INSERT INTO mined_notes (
            note_id,
            card_ids_json,
            history_id,
            server_kind,
            item_id,
            media_source_id,
            file_path,
            title,
            event_json,
            start_ms,
            end_ms,
            generate_avif,
            matched_line_index,
            included_line_first,
            included_line_last,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT(note_id) DO UPDATE SET
            card_ids_json = excluded.card_ids_json,
            history_id = excluded.history_id,
            server_kind = excluded.server_kind,
            item_id = excluded.item_id,
            media_source_id = excluded.media_source_id,
            file_path = excluded.file_path,
            title = excluded.title,
            event_json = excluded.event_json,
            start_ms = excluded.start_ms,
            end_ms = excluded.end_ms,
            generate_avif = excluded.generate_avif,
            matched_line_index = excluded.matched_line_index,
            included_line_first = excluded.included_line_first,
            included_line_last = excluded.included_line_last,
            updated_at = excluded.updated_at
        ",
        params![
            entry.note_id,
            card_ids_json,
            entry.history_id,
            entry.server_kind.as_str(),
            entry.item_id,
            entry.media_source_id,
            entry.file_path,
            entry.title,
            event_json,
            entry.start_ms,
            entry.end_ms,
            entry.generate_avif,
            entry.matched_line_index.map(|value| value as i64),
            entry.included_line_first.map(|value| value as i64),
            entry.included_line_last.map(|value| value as i64),
            entry.created_at.to_rfc3339(),
            entry.updated_at.to_rfc3339(),
        ],
    )
    .context("Failed to upsert mined note")?;

    tx.execute(
        "DELETE FROM mined_note_cards WHERE note_id = ?1",
        params![entry.note_id],
    )
    .context("Failed to clear mined-note card mappings")?;

    for card_id in &entry.card_ids {
        tx.execute(
            "INSERT INTO mined_note_cards (card_id, note_id) VALUES (?1, ?2)",
            params![card_id, entry.note_id],
        )
        .with_context(|| format!("Failed to map card {} to note {}", card_id, entry.note_id))?;
    }

    tx.commit()
        .context("Failed to commit SQLite mined-note transaction")?;
    Ok(())
}

fn list_mined_notes_sync(db_path: &Path) -> anyhow::Result<Vec<MiningHistorySummary>> {
    let conn = open_connection(db_path)?;
    let mut stmt = conn.prepare(
        "
        SELECT
            note_id,
            card_ids_json,
            title,
            event_json,
            history_id,
            server_kind,
            updated_at
        FROM mined_notes
        ORDER BY updated_at DESC
        ",
    )?;

    let rows = stmt.query_map([], |row| {
        let event_json: String = row.get(3)?;
        let event: NewCardEvent = serde_json::from_str(&event_json).map_err(to_sql_error)?;
        let server_kind_raw: String = row.get(5)?;
        let updated_at_raw: String = row.get(6)?;
        Ok(MiningHistorySummary {
            note_id: row.get(0)?,
            card_ids: parse_card_ids(row.get(1)?)?,
            title: row.get(2)?,
            sentence: event.sentence,
            history_id: row.get(4)?,
            server_kind: MediaServerKind::parse(&server_kind_raw).ok_or_else(|| {
                to_sql_error(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unknown server kind '{}'", server_kind_raw),
                ))
            })?,
            updated_at: parse_timestamp(&updated_at_raw).map_err(to_sql_error)?,
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

fn get_mined_note_by_note_id_sync(
    db_path: &Path,
    note_id: i64,
) -> anyhow::Result<Option<MiningHistoryEntry>> {
    let conn = open_connection(db_path)?;
    let mut stmt = conn.prepare(
        "
        SELECT
            note_id,
            card_ids_json,
            history_id,
            server_kind,
            item_id,
            media_source_id,
            file_path,
            title,
            event_json,
            start_ms,
            end_ms,
            generate_avif,
            matched_line_index,
            included_line_first,
            included_line_last,
            created_at,
            updated_at
        FROM mined_notes
        WHERE note_id = ?1
        ",
    )?;

    stmt.query_row(params![note_id], row_to_mined_entry)
        .optional()
        .context("Failed to read mined note by note_id")
}

fn get_mined_note_by_card_id_sync(
    db_path: &Path,
    card_id: i64,
) -> anyhow::Result<Option<MiningHistoryEntry>> {
    let conn = open_connection(db_path)?;
    let mut stmt = conn.prepare(
        "
        SELECT
            mn.note_id,
            mn.card_ids_json,
            mn.history_id,
            mn.server_kind,
            mn.item_id,
            mn.media_source_id,
            mn.file_path,
            mn.title,
            mn.event_json,
            mn.start_ms,
            mn.end_ms,
            mn.generate_avif,
            mn.matched_line_index,
            mn.included_line_first,
            mn.included_line_last,
            mn.created_at,
            mn.updated_at
        FROM mined_notes mn
        JOIN mined_note_cards mnc ON mnc.note_id = mn.note_id
        WHERE mnc.card_id = ?1
        ",
    )?;

    stmt.query_row(params![card_id], row_to_mined_entry)
        .optional()
        .context("Failed to read mined note by card_id")
}

fn row_to_mined_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MiningHistoryEntry> {
    let server_kind_raw: String = row.get(3)?;
    let event_json: String = row.get(8)?;
    let created_at_raw: String = row.get(15)?;
    let updated_at_raw: String = row.get(16)?;

    Ok(MiningHistoryEntry {
        note_id: row.get(0)?,
        card_ids: parse_card_ids(row.get(1)?)?,
        history_id: row.get(2)?,
        server_kind: MediaServerKind::parse(&server_kind_raw).ok_or_else(|| {
            to_sql_error(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unknown server kind '{}'", server_kind_raw),
            ))
        })?,
        item_id: row.get(4)?,
        media_source_id: row.get(5)?,
        file_path: row.get(6)?,
        title: row.get(7)?,
        event: serde_json::from_str(&event_json).map_err(to_sql_error)?,
        start_ms: row.get(9)?,
        end_ms: row.get(10)?,
        generate_avif: row.get(11)?,
        matched_line_index: row.get::<_, Option<i64>>(12)?.map(|value| value as usize),
        included_line_first: row.get::<_, Option<i64>>(13)?.map(|value| value as usize),
        included_line_last: row.get::<_, Option<i64>>(14)?.map(|value| value as usize),
        created_at: parse_timestamp(&created_at_raw).map_err(to_sql_error)?,
        updated_at: parse_timestamp(&updated_at_raw).map_err(to_sql_error)?,
    })
}

fn parse_card_ids(raw: String) -> rusqlite::Result<Vec<i64>> {
    serde_json::from_str(&raw).map_err(to_sql_error)
}

fn parse_timestamp(raw: &str) -> Result<DateTime<Utc>, std::io::Error> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid timestamp '{}': {}", raw, error),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{open_connection, prepare_tadoku_batches_sync, save_session_history_sync};
    use crate::config::MediaServerKind;
    use crate::session::HistoryEntry;
    use chrono::Utc;
    use rusqlite::params;
    use std::collections::HashMap;

    fn history_entry(id: &str, series: &str, position_ms: i64) -> HistoryEntry {
        HistoryEntry {
            history_id: format!("plex|{id}"),
            server_kind: MediaServerKind::Plex,
            item_id: id.to_string(),
            title: format!("Episode {id}"),
            series_name: Some(series.to_string()),
            media_source_id: format!("source-{id}"),
            file_path: None,
            duration_ms: Some(100_000),
            subtitle_count: 0,
            last_position_ms: position_ms,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn groups_completed_episodes_and_never_claims_them_twice() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let mut history = HashMap::new();
        history.insert("plex|1".to_string(), history_entry("1", "Frieren", 90_000));
        history.insert("plex|2".to_string(), history_entry("2", "Frieren", 90_000));
        history.insert("plex|3".to_string(), history_entry("3", "Frieren", 50_000));
        let mut duplicate = history_entry("1", "Frieren", 90_000);
        duplicate.history_id = "jellyfin|copy-of-1".to_string();
        duplicate.server_kind = MediaServerKind::Jellyfin;
        duplicate.item_id = "copy-of-1".to_string();
        history.insert(duplicate.history_id.clone(), duplicate);
        save_session_history_sync(&path, &history, None).unwrap();

        let first = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn").unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].series_name, "Frieren");
        assert_eq!(first[0].duration_seconds, 180);
        assert!(first[0].description.contains("2 episodes"));

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET status = 'completed' WHERE batch_id = ?1",
            params![first[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let second = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn").unwrap();
        assert!(second.is_empty());
        std::fs::remove_file(path).unwrap();
    }
}

fn load_json_or_default<T>(path: &Path) -> T
where
    T: Default + for<'de> serde::Deserialize<'de>,
{
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|error| {
            warn!(
                "Failed to parse {}: {} — starting fresh",
                path.display(),
                error
            );
            T::default()
        }),
        Err(_) => T::default(),
    }
}

fn to_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}
