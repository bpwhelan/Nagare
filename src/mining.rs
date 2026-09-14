use crate::anki::NewCardEvent;
use crate::config::{Config, MediaServerKind};
use crate::session::HistoryEntry;
use crate::subtitle::SubtitleTrack;
use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentSource {
    #[default]
    Pending,
    /// A previous enhancement attempt failed. Keep the card available for a
    /// deliberate retry, but do not let frontend auto-approval resubmit it.
    Retry,
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
    pub tadoku_log_id: Option<String>,
    pub is_longform_checkpoint: bool,
    pub series_name: String,
    pub description: String,
    pub duration_seconds: i32,
    pub language_code: String,
    pub file_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TadokuCandidate {
    pub history_id: String,
    pub series_name: String,
    pub title: String,
    pub title_overridden: bool,
    pub watched_at: String,
    pub duration_seconds: i32,
    /// True when the displayed duration is current playback progress rather
    /// than the full media runtime.
    #[serde(default)]
    pub is_in_progress: bool,
    pub pending_retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

struct TadokuCandidateRow {
    candidate: TadokuCandidate,
    server_kind: String,
    duration_ms: i64,
    sync_duration_ms: i64,
    audio_languages: Vec<String>,
    file_path: Option<String>,
    is_longform_checkpoint: bool,
}

const LONGFORM_TADOKU_THRESHOLD_MS: i64 = 2 * 60 * 60 * 1_000;
/// Long-form progress must contain a meaningful listening session; a pause
/// must not turn a few minutes of browsing into a Tadoku entry.
const LONGFORM_TADOKU_MIN_SESSION_MS: i64 = 30 * 60 * 1_000;
/// Manual review is intentionally looser than automatic export, but do not
/// offer a card for a title that was only opened for a few seconds.
const MANUAL_LONGFORM_MIN_PROGRESS_MS: i64 = 60 * 1_000;

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
            prepare_tadoku_batches_sync(&db_path, &export_date, &language_code, None)
        })
        .await
        .context("SQLite Tadoku batch preparation task failed")?
    }

    pub async fn prepare_automatic_tadoku_batches(
        &self,
        export_date: String,
        language_code: String,
        target_audio_language: String,
    ) -> anyhow::Result<Vec<TadokuExportBatch>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            prepare_tadoku_batches_with_policy_sync(
                &db_path,
                &export_date,
                &language_code,
                None,
                true,
                Some(&target_audio_language),
            )
        })
        .await
        .context("SQLite automatic Tadoku batch preparation task failed")?
    }

    pub async fn list_tadoku_candidates(
        &self,
        language_code: String,
        target_audio_language: String,
    ) -> anyhow::Result<Vec<TadokuCandidate>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            list_tadoku_candidates_with_target_sync(
                &db_path,
                &language_code,
                &target_audio_language,
            )
        })
        .await
        .context("SQLite Tadoku candidate lookup task failed")?
    }

    pub async fn prepare_selected_tadoku_batches(
        &self,
        export_date: String,
        language_code: String,
        target_audio_language: String,
        history_ids: Vec<String>,
    ) -> anyhow::Result<Vec<TadokuExportBatch>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let selected = history_ids.into_iter().collect::<HashSet<_>>();
            prepare_tadoku_batches_with_policy_sync(
                &db_path,
                &export_date,
                &language_code,
                Some(&selected),
                false,
                Some(&target_audio_language),
            )
        })
        .await
        .context("SQLite selected Tadoku batch preparation task failed")?
    }

    pub async fn decline_tadoku_candidates(
        &self,
        history_ids: Vec<String>,
    ) -> anyhow::Result<usize> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || decline_tadoku_candidates_sync(&db_path, &history_ids))
            .await
            .context("SQLite Tadoku decline task failed")?
    }

    pub async fn set_tadoku_candidate_title(
        &self,
        history_id: String,
        title: Option<String>,
    ) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            set_tadoku_candidate_title_sync(&db_path, &history_id, title.as_deref())
        })
        .await
        .context("SQLite Tadoku title override task failed")?
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

    let mut conn =
        Connection::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .context("Failed to configure SQLite busy timeout")?;
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS app_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            config_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
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
            audio_languages_json TEXT NOT NULL DEFAULT '[]',
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
            updated_at TEXT NOT NULL,
            is_longform_checkpoint INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_tadoku_batches_status ON tadoku_export_batches(status, created_at);

        CREATE TABLE IF NOT EXISTS tadoku_export_items (
            history_id TEXT NOT NULL,
            batch_id TEXT NOT NULL REFERENCES tadoku_export_batches(batch_id),
            watched_at TEXT NOT NULL,
            PRIMARY KEY (history_id, batch_id)
        );

        CREATE TABLE IF NOT EXISTS tadoku_export_runs (
            export_date TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            last_attempt_at TEXT NOT NULL,
            last_error TEXT
        );

        CREATE TABLE IF NOT EXISTS tadoku_episode_decisions (
            history_id TEXT PRIMARY KEY,
            decision TEXT NOT NULL CHECK (decision IN ('declined')),
            decided_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tadoku_episode_overrides (
            history_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        ",
    )
    .context("Failed to initialize SQLite schema")?;

    // Migrations for columns added after the initial schema shipped.
    add_column_if_missing(&conn, "media_history", "series_name", "TEXT")?;
    add_column_if_missing(
        &conn,
        "media_history",
        "audio_languages_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(
        &conn,
        "tadoku_export_batches",
        "is_longform_checkpoint",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_tadoku_export_items_for_checkpoints(&mut conn)?;
    initialize_tadoku_candidate_cutoff(&mut conn)?;

    Ok(conn)
}

/// Long-form media can create a new Tadoku batch for each distinct listening
/// session, so one history item must be allowed to belong to more than one
/// batch.
fn migrate_tadoku_export_items_for_checkpoints(conn: &mut Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(tadoku_export_items)")?;
    let columns = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let batch_id_is_part_of_primary_key = columns
        .iter()
        .any(|(name, primary_key_order)| name == "batch_id" && *primary_key_order > 0);
    if batch_id_is_part_of_primary_key {
        return Ok(());
    }

    let tx = conn
        .transaction()
        .context("Failed to start Tadoku checkpoint schema migration")?;
    tx.execute_batch(
        "
        CREATE TABLE tadoku_export_items_checkpoint_migration (
            history_id TEXT NOT NULL,
            batch_id TEXT NOT NULL REFERENCES tadoku_export_batches(batch_id),
            watched_at TEXT NOT NULL,
            PRIMARY KEY (history_id, batch_id)
        );
        INSERT INTO tadoku_export_items_checkpoint_migration (history_id, batch_id, watched_at)
        SELECT history_id, batch_id, watched_at FROM tadoku_export_items;
        DROP TABLE tadoku_export_items;
        ALTER TABLE tadoku_export_items_checkpoint_migration RENAME TO tadoku_export_items;
        ",
    )
    .context("Failed to allow multiple Tadoku checkpoints per history item")?;
    tx.commit()
        .context("Failed to commit Tadoku checkpoint schema migration")?;
    Ok(())
}

fn initialize_tadoku_candidate_cutoff(conn: &mut Connection) -> anyhow::Result<()> {
    let already_initialized: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_metadata WHERE key = 'tadoku_candidate_cutoff')",
        [],
        |row| row.get(0),
    )?;
    let previous_sync_migration_applied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_metadata WHERE key = 'tadoku_candidate_cutoff_previous_sync_v1')",
        [],
        |row| row.get(0),
    )?;
    if already_initialized && previous_sync_migration_applied {
        return Ok(());
    }

    let tx = conn
        .transaction()
        .context("Failed to start Tadoku cutoff migration")?;
    let now = Utc::now().to_rfc3339();
    let initial_cutoff = if already_initialized {
        tx.query_row(
            "SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'",
            [],
            |row| row.get::<_, String>(0),
        )?
    } else {
        tx.execute(
            "INSERT INTO app_metadata (key, value) VALUES ('tadoku_candidate_cutoff', ?1)",
            [&now],
        )?;
        // Pending batches predate per-episode review and must not be retried after
        // the feature is introduced. Completed batches remain as duplicate guards.
        tx.execute(
            "UPDATE tadoku_export_batches SET status = 'declined', updated_at = ?1 WHERE status = 'pending'",
            [&now],
        )?;
        now.clone()
    };

    if !previous_sync_migration_applied {
        // The review workflow originally used its installation time as the cutoff.
        // That loses episodes watched after the preceding daily sync but before an
        // upgrade/restart. Anchor it to the latest successful sync that predates the
        // original cutoff instead. The batch fallback also covers older databases
        // that have export items but no recorded export run.
        let previous_sync: Option<String> = tx
            .query_row(
                "
                SELECT synced_at
                FROM (
                    SELECT last_attempt_at AS synced_at
                    FROM tadoku_export_runs
                    WHERE status = 'completed'
                    UNION ALL
                    SELECT updated_at AS synced_at
                    FROM tadoku_export_batches
                    WHERE status = 'completed'
                )
                WHERE julianday(synced_at) < julianday(?1)
                ORDER BY julianday(synced_at) DESC
                LIMIT 1
                ",
                [&initial_cutoff],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(previous_sync) = previous_sync {
            tx.execute(
                "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
                [&previous_sync],
            )?;
        }
        tx.execute(
            "INSERT INTO app_metadata (key, value) VALUES ('tadoku_candidate_cutoff_previous_sync_v1', ?1)",
            [&now],
        )?;
    }

    tx.commit()
        .context("Failed to commit Tadoku cutoff migration")?;
    Ok(())
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
                audio_languages_json,
                last_position_ms,
                last_seen
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(history_id) DO UPDATE SET
                server_kind = excluded.server_kind,
                item_id = excluded.item_id,
                title = excluded.title,
                series_name = excluded.series_name,
                media_source_id = excluded.media_source_id,
                file_path = excluded.file_path,
                duration_ms = excluded.duration_ms,
                subtitle_count = excluded.subtitle_count,
                audio_languages_json = excluded.audio_languages_json,
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
                serde_json::to_string(&entry.audio_languages)?,
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
    selected_history_ids: Option<&HashSet<String>>,
) -> anyhow::Result<Vec<TadokuExportBatch>> {
    prepare_tadoku_batches_with_policy_sync(
        db_path,
        export_date,
        language_code,
        selected_history_ids,
        false,
        None,
    )
}

fn prepare_tadoku_batches_with_policy_sync(
    db_path: &Path,
    export_date: &str,
    language_code: &str,
    selected_history_ids: Option<&HashSet<String>>,
    automatic: bool,
    target_audio_language: Option<&str>,
) -> anyhow::Result<Vec<TadokuExportBatch>> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .context("Failed to start Tadoku export transaction")?;

    refresh_pending_tadoku_batches(&tx, language_code)?;

    if automatic {
        prepare_longform_tadoku_checkpoints(
            &tx,
            export_date,
            language_code,
            target_audio_language.unwrap_or_default(),
        )?;
    }

    let mut retry_batches = if let Some(selected) = selected_history_ids {
        query_pending_tadoku_batches(&tx, language_code)?
            .into_iter()
            .filter(|(_, history_ids)| history_ids.iter().any(|id| selected.contains(id)))
            .map(|(batch, _)| batch)
            .collect()
    } else {
        Vec::new()
    };

    let mut candidates = query_tadoku_candidates(&tx, language_code)?;
    if selected_history_ids.is_some() && !automatic {
        candidates.extend(query_manual_longform_tadoku_candidates(
            &tx,
            language_code,
            target_audio_language.unwrap_or(language_code),
        )?);
    }
    if automatic {
        // Completed episodes can sync promptly. The 30-minute inactivity and
        // minimum-progress rule belongs only to the AudioBookShelf long-form
        // path below, not to ordinary anime episodes.
        let now = Utc::now();
        candidates.retain(|candidate| {
            if candidate.server_kind == MediaServerKind::Audiobookshelf.as_str()
                && candidate.duration_ms > LONGFORM_TADOKU_THRESHOLD_MS
            {
                return false;
            }
            let target_language = target_audio_language.unwrap_or_default();
            if !tadoku_audio_matches_target(
                &candidate.server_kind,
                &candidate.audio_languages,
                candidate.file_path.as_deref(),
                target_language,
            ) {
                return false;
            }
            automatic_tadoku_completed_media_is_recent(&candidate.candidate.watched_at, now)
        });
    }
    let mut groups: BTreeMap<(String, Option<String>), Vec<TadokuCandidateRow>> = BTreeMap::new();
    for candidate in candidates {
        if selected_history_ids
            .is_some_and(|selected| !selected.contains(&candidate.candidate.history_id))
        {
            continue;
        }
        groups
            .entry((
                candidate.candidate.series_name.clone(),
                candidate
                    .is_longform_checkpoint
                    .then(|| candidate.candidate.history_id.clone()),
            ))
            .or_default()
            .push(candidate);
    }

    let now = Utc::now().to_rfc3339();
    let mut created_batches = Vec::new();
    for ((series_name, checkpoint_history_id), items) in groups {
        // Checkpoint credit belongs to one history item, even when multiple
        // books share the same display title. Episodes can still group by show.
        let is_longform_checkpoint = checkpoint_history_id.is_some();
        let batch_id = uuid::Uuid::new_v4().to_string();
        let duration_seconds = items
            .iter()
            .map(|item| {
                let duration_ms = if item.is_longform_checkpoint {
                    item.sync_duration_ms
                } else {
                    item.duration_ms
                };
                i128::from(tadoku_duration_seconds(i128::from(duration_ms)))
            })
            .sum::<i128>()
            .clamp(1, i32::MAX as i128) as i32;
        let description = tadoku_batch_description(
            &series_name,
            items.iter().map(|item| item.candidate.title.as_str()),
        );
        let file_paths = items
            .iter()
            .filter_map(|item| item.file_path.clone())
            .collect();

        tx.execute(
            "
            INSERT INTO tadoku_export_batches (
                batch_id, export_date, series_name, description,
                duration_seconds, language_code, is_longform_checkpoint,
                status, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)
            ",
            params![
                batch_id,
                export_date,
                series_name,
                description,
                duration_seconds,
                language_code,
                is_longform_checkpoint,
                now,
            ],
        )?;

        for item in items {
            tx.execute(
                "INSERT INTO tadoku_export_items (history_id, batch_id, watched_at) VALUES (?1, ?2, ?3)",
                params![item.candidate.history_id, batch_id, item.candidate.watched_at],
            )?;
        }

        created_batches.push(TadokuExportBatch {
            batch_id,
            tadoku_log_id: None,
            is_longform_checkpoint,
            series_name,
            description,
            duration_seconds,
            language_code: language_code.to_string(),
            file_paths,
        });
    }

    tx.commit()
        .context("Failed to commit Tadoku export transaction")?;

    if selected_history_ids.is_some() {
        retry_batches.extend(created_batches);
        return Ok(retry_batches);
    }

    let conn = open_connection(db_path)?;
    let mut stmt = conn.prepare(
        "
        SELECT teb.batch_id, teb.tadoku_log_id, teb.series_name, teb.description,
               teb.duration_seconds, teb.language_code, teb.is_longform_checkpoint
        FROM tadoku_export_batches teb
        WHERE status = 'pending' AND language_code = ?1
        ORDER BY created_at, series_name
        ",
    )?;
    let rows = stmt.query_map([language_code], |row| {
        Ok(TadokuExportBatch {
            batch_id: row.get(0)?,
            tadoku_log_id: row.get(1)?,
            series_name: row.get(2)?,
            description: row.get(3)?,
            duration_seconds: row.get(4)?,
            language_code: row.get(5)?,
            is_longform_checkpoint: row.get(6)?,
            file_paths: Vec::new(),
        })
    })?;
    let mut batches = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for batch in &mut batches {
        batch.file_paths = tadoku_batch_file_paths(&conn, &batch.batch_id)?;
    }
    if automatic {
        let target_language = target_audio_language.unwrap_or_default();
        let mut eligible_batches = Vec::new();
        for batch in batches {
            if tadoku_batch_is_automatically_eligible(&conn, &batch.batch_id, target_language)? {
                eligible_batches.push(batch);
            }
        }
        return Ok(eligible_batches);
    }
    Ok(batches)
}

/// Pending batches can outlive an automatic exporter restart. Recheck their
/// source items at export time so an old batch never bypasses the current
/// target-language, playback-duration, or recency rules. The batch remains in
/// manual review when it is not eligible for automatic sync.
fn tadoku_batch_is_automatically_eligible(
    conn: &Connection,
    batch_id: &str,
    target_language: &str,
) -> anyhow::Result<bool> {
    if target_language.is_empty() {
        return Ok(false);
    }
    let is_longform_checkpoint: bool = conn.query_row(
        "SELECT is_longform_checkpoint FROM tadoku_export_batches WHERE batch_id = ?1",
        [batch_id],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "
        SELECT mh.last_seen, mh.last_position_ms, mh.audio_languages_json,
               mh.server_kind, mh.file_path
        FROM tadoku_export_items tei
        JOIN media_history mh ON mh.history_id = tei.history_id
        WHERE tei.batch_id = ?1
        ",
    )?;
    let rows = stmt.query_map([batch_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Ok(false);
    }

    let now = Utc::now();
    Ok(rows.into_iter().all(
        |(last_seen, position_ms, languages_json, server_kind, file_path)| {
            let audio_languages =
                serde_json::from_str::<Vec<String>>(&languages_json).unwrap_or_default();
            tadoku_audio_matches_target(
                &server_kind,
                &audio_languages,
                file_path.as_deref(),
                target_language,
            ) && if is_longform_checkpoint {
                automatic_tadoku_longform_session_is_eligible(&last_seen, position_ms, now)
            } else {
                automatic_tadoku_completed_media_is_recent(&last_seen, now)
            }
        },
    ))
}

/// Prepare one batch for each recent, substantial listening session of
/// AudioBookShelf media whose maximum runtime is over two hours. A session must have at least 30
/// minutes of new progress, then be inactive for 30 minutes but no longer than
/// an hour, so old history cannot be exported as current listening.
fn prepare_longform_tadoku_checkpoints(
    conn: &Connection,
    export_date: &str,
    language_code: &str,
    target_audio_language: &str,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let inactivity_cutoff = now - chrono::Duration::minutes(30);
    let recency_cutoff = now - chrono::Duration::hours(1);
    let mut candidate_stmt = conn.prepare(
        "
        SELECT
            mh.history_id,
            COALESCE(NULLIF(mh.series_name, ''), teo.title) AS series_name,
            mh.title,
            mh.last_seen,
            mh.last_position_ms,
            mh.duration_ms,
            mh.file_path,
            mh.audio_languages_json,
            COALESCE(SUM(CASE
                WHEN teb.status IN ('pending', 'completed') THEN teb.duration_seconds
                ELSE 0
            END), 0) AS credited_seconds
        FROM media_history mh
        LEFT JOIN tadoku_episode_overrides teo ON teo.history_id = mh.history_id
        LEFT JOIN tadoku_export_items tei ON tei.history_id = mh.history_id
        LEFT JOIN tadoku_export_batches teb
          ON teb.batch_id = tei.batch_id AND teb.language_code = ?2
        WHERE mh.duration_ms > ?1
          AND mh.server_kind = ?3
          AND mh.last_position_ms > 0
          AND julianday(mh.last_seen) >= julianday((
              SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'
          ))
          AND NOT EXISTS (
              SELECT 1 FROM tadoku_episode_decisions ted WHERE ted.history_id = mh.history_id
          )
        GROUP BY mh.history_id
        ORDER BY COALESCE(NULLIF(mh.series_name, ''), teo.title, mh.title), mh.last_seen
        ",
    )?;
    let candidate_rows = candidate_stmt.query_map(
        params![
            LONGFORM_TADOKU_THRESHOLD_MS,
            language_code,
            MediaServerKind::Audiobookshelf.as_str(),
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    )?;
    let candidate_rows = candidate_rows.collect::<Result<Vec<_>, _>>()?;
    drop(candidate_stmt);

    for (
        history_id,
        series_name,
        title,
        watched_at,
        position_ms,
        duration_ms,
        file_path,
        audio_languages_json,
        credited_seconds,
    ) in candidate_rows
    {
        let audio_languages =
            serde_json::from_str::<Vec<String>>(&audio_languages_json).unwrap_or_default();
        if !tadoku_audio_matches_target(
            MediaServerKind::Audiobookshelf.as_str(),
            &audio_languages,
            file_path.as_deref(),
            target_audio_language,
        ) {
            continue;
        }
        let last_activity = parse_timestamp(&watched_at)
            .with_context(|| format!("Invalid last-playback timestamp for history {history_id}"))?;
        if !(recency_cutoff..=inactivity_cutoff).contains(&last_activity) {
            continue;
        }

        let checkpoint_ms = position_ms.clamp(0, duration_ms);
        let credited_ms = credited_seconds.saturating_mul(1_000);
        let unsynced_ms = checkpoint_ms.saturating_sub(credited_ms);
        if unsynced_ms < LONGFORM_TADOKU_MIN_SESSION_MS {
            continue;
        }

        let batch_id = uuid::Uuid::new_v4().to_string();
        let description =
            longform_tadoku_description(series_name.as_deref(), &title, file_path.as_deref());
        let series_name = description.clone();
        conn.execute(
            "
            INSERT INTO tadoku_export_batches (
                batch_id, export_date, series_name, description,
                duration_seconds, language_code, is_longform_checkpoint,
                status, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'pending', ?7, ?7)
            ",
            params![
                batch_id,
                export_date,
                series_name,
                description,
                tadoku_duration_seconds(i128::from(unsynced_ms)),
                language_code,
                now.to_rfc3339(),
            ],
        )?;
        conn.execute(
            "INSERT INTO tadoku_export_items (history_id, batch_id, watched_at) VALUES (?1, ?2, ?3)",
            params![history_id, batch_id, watched_at],
        )?;
    }

    Ok(())
}

fn refresh_pending_tadoku_batches(conn: &Connection, language_code: &str) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        "
        SELECT
            tadoku_export_batches.batch_id,
            tadoku_export_batches.series_name,
            tadoku_export_batches.duration_seconds,
            tadoku_export_batches.is_longform_checkpoint,
            media_history.title,
            media_history.duration_ms,
            media_history.last_position_ms
        FROM tadoku_export_batches
        JOIN tadoku_export_items
          ON tadoku_export_items.batch_id = tadoku_export_batches.batch_id
        JOIN media_history
          ON media_history.history_id = tadoku_export_items.history_id
        WHERE tadoku_export_batches.status = 'pending'
          AND tadoku_export_batches.language_code = ?1
          AND media_history.duration_ms IS NOT NULL
          AND media_history.duration_ms > 0
        ORDER BY tadoku_export_batches.created_at, tadoku_export_items.watched_at
        ",
    )?;
    let rows = stmt.query_map([language_code], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    let mut batches: BTreeMap<String, (String, Vec<(String, i64, bool)>)> = BTreeMap::new();
    for row in rows {
        let (
            batch_id,
            series_name,
            batch_duration_seconds,
            is_longform_checkpoint,
            title,
            duration_ms,
            _position_ms,
        ) = row?;
        let credited_ms = if is_longform_checkpoint {
            batch_duration_seconds.saturating_mul(1_000)
        } else {
            duration_ms
        };
        let batch = batches
            .entry(batch_id)
            .or_insert_with(|| (series_name, Vec::new()));
        batch.1.push((title, credited_ms, is_longform_checkpoint));
    }
    drop(stmt);

    let now = Utc::now().to_rfc3339();
    for (batch_id, (series_name, episodes)) in batches {
        let duration_seconds = episodes
            .iter()
            .map(|(_, duration_ms, _)| {
                i128::from(tadoku_duration_seconds(i128::from(*duration_ms)))
            })
            .sum::<i128>()
            .clamp(1, i32::MAX as i128) as i32;
        let description = if episodes.len() == 1 && episodes[0].2 {
            series_name.clone()
        } else {
            tadoku_batch_description(
                &series_name,
                episodes.iter().map(|(title, _, _)| title.as_str()),
            )
        };
        conn.execute(
            "UPDATE tadoku_export_batches SET description = ?2, duration_seconds = ?3, updated_at = ?4 WHERE batch_id = ?1",
            params![batch_id, description, duration_seconds, now],
        )?;
    }
    Ok(())
}

#[cfg(test)]
fn list_tadoku_candidates_sync(
    db_path: &Path,
    language_code: &str,
) -> anyhow::Result<Vec<TadokuCandidate>> {
    list_tadoku_candidates_with_target_sync(db_path, language_code, language_code)
}

fn list_tadoku_candidates_with_target_sync(
    db_path: &Path,
    language_code: &str,
    target_audio_language: &str,
) -> anyhow::Result<Vec<TadokuCandidate>> {
    let conn = open_connection(db_path)?;
    let mut candidates = query_pending_tadoku_candidates(&conn, language_code)?;
    candidates.extend(
        query_tadoku_candidates(&conn, language_code)?
            .into_iter()
            .map(|row| row.candidate),
    );
    candidates.extend(
        query_manual_longform_tadoku_candidates(&conn, language_code, target_audio_language)?
            .into_iter()
            .map(|row| row.candidate),
    );
    candidates.sort_by(|left, right| {
        left.series_name
            .cmp(&right.series_name)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(candidates)
}

fn set_tadoku_candidate_title_sync(
    db_path: &Path,
    history_id: &str,
    title: Option<&str>,
) -> anyhow::Result<()> {
    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .context("Failed to start Tadoku title override transaction")?;
    let original_title: String = tx
        .query_row(
            "SELECT title FROM media_history WHERE history_id = ?1",
            [history_id],
            |row| row.get(0),
        )
        .optional()?
        .with_context(|| format!("Tadoku episode '{}' was not found", history_id))?;

    match title.map(str::trim) {
        Some(title) if title.is_empty() => bail!("Tadoku title cannot be empty"),
        Some(title) if title.chars().count() > 500 => {
            bail!("Tadoku title cannot be longer than 500 characters")
        }
        Some(title) if title != original_title => {
            tx.execute(
                "
                INSERT INTO tadoku_episode_overrides (history_id, title, updated_at)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(history_id) DO UPDATE SET
                    title = excluded.title,
                    updated_at = excluded.updated_at
                ",
                params![history_id, title, Utc::now().to_rfc3339()],
            )?;
        }
        _ => {
            tx.execute(
                "DELETE FROM tadoku_episode_overrides WHERE history_id = ?1",
                [history_id],
            )?;
        }
    }

    // Pending batches have not reached Tadoku yet. Unpack any batch containing
    // this episode so the next sync rebuilds its description from the override.
    let mut pending_stmt = tx.prepare(
        "
        SELECT teb.batch_id
        FROM tadoku_export_items tei
        JOIN tadoku_export_batches teb ON teb.batch_id = tei.batch_id
        WHERE tei.history_id = ?1 AND teb.status = 'pending'
        ",
    )?;
    let pending_batches = pending_stmt
        .query_map([history_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(pending_stmt);
    for batch_id in pending_batches {
        tx.execute(
            "UPDATE tadoku_export_batches SET status = 'superseded', updated_at = ?2 WHERE batch_id = ?1",
            params![batch_id, Utc::now().to_rfc3339()],
        )?;
        tx.execute(
            "DELETE FROM tadoku_export_items WHERE batch_id = ?1",
            [batch_id],
        )?;
    }

    tx.commit()
        .context("Failed to commit Tadoku title override transaction")?;
    Ok(())
}

fn decline_tadoku_candidates_sync(db_path: &Path, history_ids: &[String]) -> anyhow::Result<usize> {
    let unique_ids = history_ids.iter().collect::<HashSet<_>>();
    if unique_ids.is_empty() {
        return Ok(0);
    }

    let mut conn = open_connection(db_path)?;
    let tx = conn
        .transaction()
        .context("Failed to start Tadoku decline transaction")?;
    let now = Utc::now().to_rfc3339();
    let mut affected_batches = HashSet::new();
    let mut declined = 0;

    for history_id in unique_ids {
        let mut pending_stmt = tx.prepare(
            "
                SELECT teb.batch_id
                FROM tadoku_export_items tei
                JOIN tadoku_export_batches teb ON teb.batch_id = tei.batch_id
                WHERE tei.history_id = ?1 AND teb.status = 'pending'
                ",
        )?;
        let pending_batches = pending_stmt
            .query_map([history_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(pending_stmt);
        for batch_id in pending_batches {
            affected_batches.insert(batch_id);
        }

        declined += tx.execute(
            "
            INSERT INTO tadoku_episode_decisions (history_id, decision, decided_at)
            SELECT history_id, 'declined', ?2 FROM media_history WHERE history_id = ?1
            ON CONFLICT(history_id) DO UPDATE SET
                decision = excluded.decision,
                decided_at = excluded.decided_at
            ",
            params![history_id, now],
        )?;
    }

    // A failed/pending batch may contain several episodes. Retire and unpack it
    // so declined episodes stay excluded while the remaining episodes can be
    // regrouped on the next sync attempt.
    for batch_id in affected_batches {
        tx.execute(
            "UPDATE tadoku_export_batches SET status = 'declined', updated_at = ?2 WHERE batch_id = ?1",
            params![batch_id, now],
        )?;
        tx.execute(
            "DELETE FROM tadoku_export_items WHERE batch_id = ?1",
            [&batch_id],
        )?;
    }

    tx.commit()
        .context("Failed to commit Tadoku decline transaction")?;
    Ok(declined)
}

fn query_pending_tadoku_batches(
    conn: &Connection,
    language_code: &str,
) -> anyhow::Result<Vec<(TadokuExportBatch, HashSet<String>)>> {
    let mut stmt = conn.prepare(
        "
        SELECT teb.batch_id, teb.tadoku_log_id, teb.series_name, teb.description,
               teb.duration_seconds, teb.language_code, teb.is_longform_checkpoint
        FROM tadoku_export_batches teb
        WHERE status = 'pending' AND language_code = ?1
        ORDER BY created_at, series_name
        ",
    )?;
    let rows = stmt.query_map([language_code], |row| {
        Ok(TadokuExportBatch {
            batch_id: row.get(0)?,
            tadoku_log_id: row.get(1)?,
            series_name: row.get(2)?,
            description: row.get(3)?,
            duration_seconds: row.get(4)?,
            language_code: row.get(5)?,
            is_longform_checkpoint: row.get(6)?,
            file_paths: Vec::new(),
        })
    })?;
    let batches = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut result = Vec::with_capacity(batches.len());
    for mut batch in batches {
        batch.file_paths = tadoku_batch_file_paths(conn, &batch.batch_id)?;
        let mut item_stmt =
            conn.prepare("SELECT history_id FROM tadoku_export_items WHERE batch_id = ?1")?;
        let history_ids = item_stmt
            .query_map([&batch.batch_id], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?;
        result.push((batch, history_ids));
    }
    Ok(result)
}

fn tadoku_batch_file_paths(conn: &Connection, batch_id: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "
        SELECT mh.file_path
        FROM tadoku_export_items tei
        JOIN media_history mh ON mh.history_id = tei.history_id
        WHERE tei.batch_id = ?1 AND mh.file_path IS NOT NULL AND mh.file_path != ''
        ",
    )?;
    Ok(stmt
        .query_map([batch_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn query_pending_tadoku_candidates(
    conn: &Connection,
    language_code: &str,
) -> anyhow::Result<Vec<TadokuCandidate>> {
    let mut stmt = conn.prepare(
        "
        SELECT
            media_history.history_id,
            COALESCE(NULLIF(media_history.series_name, ''), tadoku_episode_overrides.title, media_history.title) AS show_name,
            COALESCE(tadoku_episode_overrides.title, media_history.title) AS episode_title,
            tadoku_export_items.watched_at,
            media_history.duration_ms,
            tadoku_export_batches.duration_seconds,
            tadoku_export_batches.last_error,
            tadoku_episode_overrides.title IS NOT NULL AS title_overridden,
            tadoku_export_batches.is_longform_checkpoint
        FROM tadoku_export_items
        JOIN tadoku_export_batches
          ON tadoku_export_batches.batch_id = tadoku_export_items.batch_id
        JOIN media_history
          ON media_history.history_id = tadoku_export_items.history_id
        LEFT JOIN tadoku_episode_overrides
          ON tadoku_episode_overrides.history_id = media_history.history_id
        WHERE tadoku_export_batches.status = 'pending'
          AND tadoku_export_batches.language_code = ?1
          AND NOT EXISTS (
              SELECT 1 FROM tadoku_episode_decisions ted
              WHERE ted.history_id = media_history.history_id
          )
        ORDER BY show_name, tadoku_export_items.watched_at
        ",
    )?;
    let rows = stmt.query_map([language_code], |row| {
        let duration_ms = row.get::<_, i64>(4)?;
        let is_longform_checkpoint = row.get::<_, bool>(8)?;
        Ok(TadokuCandidate {
            history_id: row.get(0)?,
            series_name: row.get(1)?,
            title: row.get(2)?,
            title_overridden: row.get(7)?,
            watched_at: row.get(3)?,
            duration_seconds: if is_longform_checkpoint {
                row.get(5)?
            } else {
                tadoku_duration_seconds(i128::from(duration_ms))
            },
            is_in_progress: is_longform_checkpoint,
            pending_retry: true,
            last_error: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn query_tadoku_candidates(
    conn: &Connection,
    language_code: &str,
) -> anyhow::Result<Vec<TadokuCandidateRow>> {
    let mut stmt = conn.prepare(
        "
        SELECT
            media_history.history_id,
            media_history.server_kind,
            COALESCE(NULLIF(media_history.series_name, ''), tadoku_episode_overrides.title, media_history.title) AS show_name,
            COALESCE(tadoku_episode_overrides.title, media_history.title) AS episode_title,
            media_history.last_seen,
            media_history.duration_ms,
            media_history.file_path,
            media_history.audio_languages_json,
            tadoku_episode_overrides.title IS NOT NULL AS title_overridden
        FROM media_history
        LEFT JOIN tadoku_episode_overrides
          ON tadoku_episode_overrides.history_id = media_history.history_id
        WHERE media_history.duration_ms IS NOT NULL
          AND media_history.duration_ms > 0
          AND media_history.last_position_ms > 0
          AND media_history.last_position_ms * 100 >= media_history.duration_ms * 80
          AND media_history.last_position_ms + 300000 >= media_history.duration_ms
          AND julianday(media_history.last_seen) >= julianday((
              SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'
          ))
          AND NOT EXISTS (
              SELECT 1 FROM tadoku_episode_decisions ted
              WHERE ted.history_id = media_history.history_id
          )
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
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, bool>(8)?,
        ))
    })?;
    let mut seen_content = HashSet::new();
    let mut candidates = Vec::new();
    for row in rows {
        let (
            history_id,
            server_kind,
            series_name,
            title,
            watched_at,
            duration_ms,
            file_path,
            audio_languages_json,
            title_overridden,
        ) = row?;
        let content_key = format!(
            "{}\u{1f}{}",
            series_name.trim().to_lowercase(),
            title.trim().to_lowercase()
        );
        if !seen_content.insert(content_key) {
            continue;
        }
        candidates.push(TadokuCandidateRow {
            candidate: TadokuCandidate {
                history_id,
                series_name,
                title,
                title_overridden,
                watched_at,
                duration_seconds: tadoku_duration_seconds(i128::from(duration_ms)),
                is_in_progress: false,
                pending_retry: false,
                last_error: None,
            },
            server_kind,
            duration_ms,
            sync_duration_ms: duration_ms,
            audio_languages: serde_json::from_str(&audio_languages_json).unwrap_or_default(),
            file_path,
            is_longform_checkpoint: false,
        });
    }
    Ok(candidates)
}

fn query_manual_longform_tadoku_candidates(
    conn: &Connection,
    language_code: &str,
    target_audio_language: &str,
) -> anyhow::Result<Vec<TadokuCandidateRow>> {
    let mut stmt = conn.prepare(
        "
        SELECT
            mh.history_id,
            COALESCE(NULLIF(mh.series_name, ''), teo.title, mh.title) AS series_name,
            COALESCE(teo.title, mh.title) AS episode_title,
            mh.last_seen,
            mh.last_position_ms,
            mh.duration_ms,
            mh.file_path,
            mh.audio_languages_json,
            COALESCE(SUM(CASE
                WHEN teb.status IN ('pending', 'completed') THEN teb.duration_seconds
                ELSE 0
            END), 0) AS credited_seconds,
            teo.title IS NOT NULL AS title_overridden
        FROM media_history mh
        LEFT JOIN tadoku_episode_overrides teo ON teo.history_id = mh.history_id
        LEFT JOIN tadoku_export_items tei ON tei.history_id = mh.history_id
        LEFT JOIN tadoku_export_batches teb
          ON teb.batch_id = tei.batch_id AND teb.language_code = ?1
        WHERE mh.server_kind = ?2
          AND mh.duration_ms > ?3
          AND mh.last_position_ms > 0
          AND (
              NOT (
                  mh.last_position_ms * 100 >= mh.duration_ms * 80
                  AND mh.last_position_ms + 300000 >= mh.duration_ms
              )
              OR EXISTS (
                  SELECT 1 FROM tadoku_export_items prior_item
                  JOIN tadoku_export_batches prior_batch ON prior_batch.batch_id = prior_item.batch_id
                  WHERE prior_item.history_id = mh.history_id
                    AND prior_batch.language_code = ?1
                    AND prior_batch.is_longform_checkpoint = 1
                    AND prior_batch.status = 'completed'
              )
          )
          AND julianday(mh.last_seen) >= julianday((
              SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'
          ))
          AND NOT EXISTS (
              SELECT 1 FROM tadoku_episode_decisions ted
              WHERE ted.history_id = mh.history_id
          )
          AND NOT EXISTS (
              SELECT 1
              FROM tadoku_export_items tei_pending
              JOIN tadoku_export_batches teb_pending
                ON teb_pending.batch_id = tei_pending.batch_id
              WHERE tei_pending.history_id = mh.history_id
                AND teb_pending.status = 'pending'
                AND teb_pending.language_code = ?1
          )
        GROUP BY mh.history_id
        ORDER BY series_name, mh.last_seen
        ",
    )?;
    let rows = stmt.query_map(
        params![
            language_code,
            MediaServerKind::Audiobookshelf.as_str(),
            LONGFORM_TADOKU_THRESHOLD_MS,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, bool>(9)?,
            ))
        },
    )?;

    let mut candidates = Vec::new();
    for row in rows {
        let (
            history_id,
            series_name,
            title,
            watched_at,
            position_ms,
            duration_ms,
            file_path,
            audio_languages_json,
            credited_seconds,
            title_overridden,
        ) = row?;
        let audio_languages =
            serde_json::from_str::<Vec<String>>(&audio_languages_json).unwrap_or_default();
        if !tadoku_audio_matches_target(
            MediaServerKind::Audiobookshelf.as_str(),
            &audio_languages,
            file_path.as_deref(),
            target_audio_language,
        ) {
            continue;
        }

        let checkpoint_ms = position_ms.clamp(0, duration_ms);
        let credited_ms = credited_seconds.saturating_mul(1_000);
        let unsynced_ms = checkpoint_ms.saturating_sub(credited_ms);
        if unsynced_ms < MANUAL_LONGFORM_MIN_PROGRESS_MS {
            continue;
        }

        let description =
            longform_tadoku_description(Some(&series_name), &title, file_path.as_deref());
        candidates.push(TadokuCandidateRow {
            candidate: TadokuCandidate {
                history_id,
                series_name: description.clone(),
                title: description,
                title_overridden,
                watched_at,
                duration_seconds: tadoku_duration_seconds(i128::from(unsynced_ms)),
                is_in_progress: true,
                pending_retry: false,
                last_error: None,
            },
            server_kind: MediaServerKind::Audiobookshelf.as_str().to_string(),
            duration_ms,
            sync_duration_ms: unsynced_ms,
            audio_languages,
            file_path,
            is_longform_checkpoint: true,
        });
    }
    Ok(candidates)
}

/// AudioBookShelf can populate every file's language metadata with a default
/// such as `eng`, even when the library is explicitly organized by language.
/// Use a matching library-directory component as a narrowly scoped fallback;
/// never infer a language from the filename itself or from other server kinds.
fn tadoku_audio_matches_target(
    server_kind: &str,
    audio_languages: &[String],
    file_path: Option<&str>,
    target_language: &str,
) -> bool {
    if target_language.trim().is_empty() {
        return false;
    }

    if audio_languages.iter().any(|language| {
        crate::session::SessionManager::language_matches_target(Some(language), target_language)
    }) {
        return true;
    }

    server_kind == MediaServerKind::Audiobookshelf.as_str()
        && file_path.is_some_and(|path| {
            let mut components = path
                .split(['/', '\\'])
                .filter(|component| !component.trim().is_empty())
                .peekable();
            while let Some(component) = components.next() {
                // The last component is the media filename, which is not a
                // reliable language declaration.
                if components.peek().is_none() {
                    break;
                }
                if crate::session::SessionManager::language_matches_target(
                    Some(component),
                    target_language,
                ) {
                    return true;
                }
            }
            false
        })
}

fn automatic_tadoku_completed_media_is_recent(watched_at: &str, now: DateTime<Utc>) -> bool {
    let Ok(last_activity) = parse_timestamp(watched_at) else {
        warn!("Ignoring Tadoku history with invalid last-playback timestamp");
        return false;
    };
    last_activity >= now - chrono::Duration::hours(1)
}

fn automatic_tadoku_longform_session_is_eligible(
    watched_at: &str,
    position_ms: i64,
    now: DateTime<Utc>,
) -> bool {
    if position_ms < LONGFORM_TADOKU_MIN_SESSION_MS {
        return false;
    }

    let Ok(last_activity) = parse_timestamp(watched_at) else {
        warn!("Ignoring Tadoku history with invalid last-playback timestamp");
        return false;
    };
    let inactivity_cutoff = now - chrono::Duration::minutes(30);
    let recency_cutoff = now - chrono::Duration::hours(1);
    (recency_cutoff..=inactivity_cutoff).contains(&last_activity)
}

fn tadoku_duration_seconds(total_duration_ms: i128) -> i32 {
    (total_duration_ms / 1_000).clamp(1, i32::MAX as i128) as i32
}

fn longform_tadoku_description(
    series_name: Option<&str>,
    title: &str,
    file_path: Option<&str>,
) -> String {
    let series_name = series_name
        .map(trim_html_space_suffix)
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let book_title = title
        .rsplit_once(" — ")
        .and_then(|(book_title, track_title)| {
            let track_title = trim_html_space_suffix(track_title);
            let matches_file = file_path
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.trim().eq_ignore_ascii_case(track_title));
            let is_audio_filename = Path::new(track_title)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(extension.to_ascii_lowercase().as_str(), "mp3" | "m4b")
                });
            (matches_file || is_audio_filename).then(|| trim_html_space_suffix(book_title).trim())
        });

    if let (Some(base_title), Some(path)) = (series_name.or(book_title), file_path)
        && let Some(volume_title) = longform_volume_title_from_path(base_title, path)
    {
        return volume_title;
    }

    if let Some(book_title) = book_title
        && series_name.is_none_or(|series| !book_title.eq_ignore_ascii_case(series))
    {
        return book_title.to_string();
    }

    if let Some(series_name) = series_name {
        return series_name.to_string();
    }

    trim_html_space_suffix(title).to_string()
}

fn longform_volume_title_from_path(series_name: &str, file_path: &str) -> Option<String> {
    let series_name_lowercase = series_name.to_lowercase();
    Path::new(file_path)
        .components()
        .rev()
        .find_map(|component| {
            let value = component.as_os_str().to_str()?;
            let value = Path::new(value)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(value);
            let value = trim_html_space_suffix(value).trim();
            (value.len() > series_name.len()
                && value.to_lowercase().contains(&series_name_lowercase))
            .then(|| value.to_string())
        })
}

fn trim_html_space_suffix(mut value: &str) -> &str {
    value = value.trim();
    while let Some(trimmed) = value.strip_suffix("&#x20;") {
        value = trimmed.trim_end();
    }
    value
}

fn tadoku_batch_description<'a>(
    series_name: &str,
    titles: impl IntoIterator<Item = &'a str>,
) -> String {
    let titles = titles.into_iter().collect::<Vec<_>>();
    if !titles.is_empty()
        && titles
            .iter()
            .all(|title| title.trim().eq_ignore_ascii_case(series_name.trim()))
    {
        return series_name.to_string();
    }
    let parsed = titles
        .iter()
        .map(|title| parse_episode_number(series_name, title))
        .collect::<Option<Vec<_>>>();

    if let Some(episodes) = parsed {
        let mut by_season: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (season, episode) in episodes {
            by_season.entry(season).or_default().push(episode);
        }
        let mut ranges = Vec::new();
        for (season, mut episodes) in by_season {
            episodes.sort_unstable();
            episodes.dedup();
            let mut start = 0;
            while start < episodes.len() {
                let mut end = start;
                while end + 1 < episodes.len() && episodes[end + 1] == episodes[end] + 1 {
                    end += 1;
                }
                let first = episodes[start];
                let last = episodes[end];
                if first == last {
                    ranges.push(format!("S{season:02}E{first:02}"));
                } else {
                    ranges.push(format!("S{season:02}E{first:02}-{last:02}"));
                }
                start = end + 1;
            }
        }
        return format!("{} {}", series_name, ranges.join(", "));
    }

    let episode_titles = titles
        .iter()
        .map(|title| {
            title
                .strip_prefix(series_name)
                .map(str::trim)
                .filter(|suffix| !suffix.is_empty())
                .unwrap_or(title)
        })
        .collect::<Vec<_>>();
    format!("{} — {}", series_name, episode_titles.join("; "))
}

fn parse_episode_number(series_name: &str, title: &str) -> Option<(u32, u32)> {
    let suffix = title
        .strip_prefix(series_name)?
        .trim_start()
        .strip_prefix('S')?;
    let (season, episode_and_title) = suffix.split_once('E')?;
    let episode_end = episode_and_title
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(episode_and_title.len());
    let episode = &episode_and_title[..episode_end];
    if season.is_empty() || episode.is_empty() || !season.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let season = season.parse().ok()?;
    let mut episode = episode.parse().ok()?;
    if episode == 0 && (1900..=9999).contains(&season) {
        let episode_name = episode_and_title[episode_end..].trim_start();
        let episode_name = episode_name
            .strip_prefix('-')
            .unwrap_or(episode_name)
            .trim();
        episode = crate::media_server::episode_number_from_name(episode_name)
            .filter(|episode| *episode > 0)
            .unwrap_or(episode);
    }
    Some((season, episode))
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
            series_name,
            audio_languages_json
        FROM media_history
        ",
    )?;

    let rows = stmt.query_map([], |row| {
        let server_kind_raw: String = row.get(1)?;
        let last_seen_raw: String = row.get(9)?;
        let subtitle_count: i64 = row.get(7)?;
        let audio_languages_raw: String = row.get(11)?;
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
            audio_languages: serde_json::from_str(&audio_languages_raw).map_err(to_sql_error)?,
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
    use super::{
        decline_tadoku_candidates_sync, list_tadoku_candidates_sync, open_connection,
        prepare_tadoku_batches_sync, prepare_tadoku_batches_with_policy_sync,
        save_session_history_sync, set_tadoku_candidate_title_sync,
    };
    use crate::config::MediaServerKind;
    use crate::session::HistoryEntry;
    use chrono::{Duration, Utc};
    use rusqlite::params;
    use std::collections::{HashMap, HashSet};

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
            audio_languages: vec!["jpn".to_string()],
            last_position_ms: position_ms,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn migrates_tadoku_items_to_allow_multiple_batches_per_history_item() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-checkpoint-schema-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute_batch(
            "
            INSERT INTO tadoku_export_batches (
                batch_id, export_date, series_name, description, duration_seconds,
                language_code, status, created_at, updated_at
            ) VALUES (
                'batch-1', '2026-08-24', 'Book', 'Book 1', 1800,
                'jpn', 'completed', '2026-08-24T00:00:00Z', '2026-08-24T00:00:00Z'
            );
            DROP TABLE tadoku_export_items;
            CREATE TABLE tadoku_export_items (
                history_id TEXT PRIMARY KEY,
                batch_id TEXT NOT NULL REFERENCES tadoku_export_batches(batch_id),
                watched_at TEXT NOT NULL
            );
            INSERT INTO tadoku_export_items (history_id, batch_id, watched_at)
            VALUES ('book-1', 'batch-1', '2026-08-24T00:00:00Z');
            ",
        )
        .unwrap();
        drop(conn);

        let migrated = open_connection(&path).unwrap();
        migrated
            .execute_batch(
                "
                INSERT INTO tadoku_export_batches (
                    batch_id, export_date, series_name, description, duration_seconds,
                    language_code, status, created_at, updated_at
                ) VALUES (
                    'batch-2', '2026-08-24', 'Book', 'Book 1', 1800,
                    'jpn', 'pending', '2026-08-24T00:30:00Z', '2026-08-24T00:30:00Z'
                );
                INSERT INTO tadoku_export_items (history_id, batch_id, watched_at)
                VALUES ('book-1', 'batch-2', '2026-08-24T00:30:00Z');
                ",
            )
            .unwrap();
        let item_count: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM tadoku_export_items WHERE history_id = 'book-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(item_count, 2);
        drop(migrated);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn groups_completed_episodes_with_titles_and_never_claims_them_twice() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());
        let mut history = HashMap::new();
        let mut first_episode = history_entry("1", "Frieren", 90_001);
        first_episode.file_path = Some("/media/Anime/Frieren/episode01.mkv".to_string());
        history.insert("plex|1".to_string(), first_episode);
        history.insert("plex|2".to_string(), history_entry("2", "Frieren", 90_000));
        history.insert("plex|3".to_string(), history_entry("3", "Frieren", 50_000));
        let mut duplicate = history_entry("1", "Frieren", 90_001);
        duplicate.history_id = "jellyfin|copy-of-1".to_string();
        duplicate.server_kind = MediaServerKind::Jellyfin;
        duplicate.item_id = "copy-of-1".to_string();
        duplicate.file_path = Some("/media/Anime/Frieren/episode01.mkv".to_string());
        history.insert(duplicate.history_id.clone(), duplicate);
        save_session_history_sync(&path, &history, None).unwrap();

        let first = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", None).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].series_name, "Frieren");
        assert_eq!(first[0].duration_seconds, 200);
        assert_eq!(first[0].description, "Frieren — Episode 1; Episode 2");
        assert!(
            first[0]
                .file_paths
                .iter()
                .any(|path| path.contains("Anime"))
        );
        assert!(!first[0].description.contains("Nagare:"));

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET status = 'completed' WHERE batch_id = ?1",
            params![first[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let second = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", None).unwrap();
        assert!(second.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn manual_tadoku_batches_only_claim_selected_episodes() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-manual-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());
        let mut history = HashMap::new();
        history.insert("plex|1".to_string(), history_entry("1", "Frieren", 90_000));
        history.insert(
            "plex|2".to_string(),
            history_entry("2", "Apothecary", 90_000),
        );
        save_session_history_sync(&path, &history, None).unwrap();

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 2);

        let selected = HashSet::from(["plex|1".to_string()]);
        let batches =
            prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", Some(&selected)).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].series_name, "Frieren");
        assert_eq!(batches[0].duration_seconds, 100);

        let retry_candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(retry_candidates.len(), 2);
        assert!(
            retry_candidates
                .iter()
                .find(|candidate| candidate.history_id == "plex|1")
                .unwrap()
                .pending_retry
        );

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET status = 'completed' WHERE batch_id = ?1",
            params![batches[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let remaining = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].history_id, "plex|2");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tadoku_uses_full_episode_runtime_after_completion_threshold() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-full-runtime-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let mut episode = history_entry("1", "Frieren", 1_260_000);
        episode.duration_ms = Some(1_389_000); // 23:09
        let history = HashMap::from([(episode.history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].duration_seconds, 1_389);

        let selected = HashSet::from(["plex|1".to_string()]);
        let batches =
            prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", Some(&selected)).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].duration_seconds, 1_389);

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET duration_seconds = 1260 WHERE batch_id = ?1",
            [&batches[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let retried =
            prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", Some(&selected)).unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].duration_seconds, 1_389);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tadoku_does_not_finish_long_media_with_too_much_remaining() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-long-media-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let mut audiobook = history_entry("1", "Audiobook", 14_400_000); // 80% of five hours
        audiobook.duration_ms = Some(18_000_000);
        let history = HashMap::from([(audiobook.history_id.clone(), audiobook)]);
        save_session_history_sync(&path, &history, None).unwrap();

        assert!(
            list_tadoku_candidates_sync(&path, "jpn")
                .unwrap()
                .is_empty()
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn automatic_tadoku_logs_longform_once_after_inactivity() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-longform-inactivity-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let series_title = "陰の実力者になりたくて！";
        let volume_title = "[2巻] 陰の実力者になりたくて！";
        let mut audiobook = history_entry("book-1", series_title, 5 * 60 * 60 * 1_000);
        audiobook.title = format!("{series_title} — chapter-01.mp3 &#x20;");
        audiobook.series_name = Some(format!("{series_title} &#x20;"));
        audiobook.file_path = Some(format!("/audiobooks/{volume_title}/chapter-01.mp3"));
        audiobook.server_kind = MediaServerKind::Audiobookshelf;
        audiobook.duration_ms = Some(5 * 60 * 60 * 1_000);
        let history_id = audiobook.history_id.clone();
        let mut history = HashMap::from([(history_id.clone(), audiobook)]);
        save_session_history_sync(&path, &history, None).unwrap();

        // Completing or unloading a book is not enough to create a log: the
        // last observed playback activity must first become inactive.
        let still_active = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(still_active.is_empty());

        // Less than 30 minutes of listening never becomes an automatic
        // long-form entry, even after the inactivity delay.
        history.get_mut(&history_id).unwrap().last_position_ms = 29 * 60 * 1_000;
        history.get_mut(&history_id).unwrap().last_seen = Utc::now() - Duration::minutes(31);
        save_session_history_sync(&path, &history, None).unwrap();
        let too_short = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(too_short.is_empty());

        // Old history is deliberately outside the recent automatic-export
        // window, even when it contains hours of progress.
        history.get_mut(&history_id).unwrap().last_position_ms = 5 * 60 * 60 * 1_000;
        history.get_mut(&history_id).unwrap().last_seen = Utc::now() - Duration::minutes(61);
        save_session_history_sync(&path, &history, None).unwrap();
        let stale = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(stale.is_empty());

        history.get_mut(&history_id).unwrap().last_seen = Utc::now() - Duration::minutes(31);
        save_session_history_sync(&path, &history, None).unwrap();
        let batch = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert_eq!(batch.len(), 1);
        assert!(batch[0].is_longform_checkpoint);
        assert_eq!(batch[0].duration_seconds, 5 * 60 * 60);
        assert_eq!(batch[0].description, volume_title);

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET status = 'completed' WHERE batch_id = ?1",
            [&batch[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let after_export = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(after_export.is_empty());

        let conn = open_connection(&path).unwrap();
        let batch_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tadoku_export_batches", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(batch_count, 1);
        let completed_batch: (String, i64) = conn
            .query_row(
                "SELECT status, duration_seconds FROM tadoku_export_batches WHERE batch_id = ?1",
                [&batch[0].batch_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(completed_batch, ("completed".to_string(), 5 * 60 * 60));
        drop(conn);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn automatic_tadoku_syncs_recent_completed_anime_regardless_of_duration() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-automatic-recency-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let mut episode = history_entry("recent", "Frieren", 29 * 60 * 1_000);
        episode.duration_ms = Some(29 * 60 * 1_000);
        episode.last_seen = Utc::now();
        let history_id = episode.history_id.clone();
        let mut history = HashMap::from([(history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let recent_anime = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert_eq!(recent_anime.len(), 1);
        assert_eq!(recent_anime[0].duration_seconds, 29 * 60);
        assert!(!recent_anime[0].is_longform_checkpoint);

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE tadoku_export_batches SET status = 'completed' WHERE batch_id = ?1",
            [&recent_anime[0].batch_id],
        )
        .unwrap();
        drop(conn);

        let mut stale_episode = history_entry("stale", "Older Series", 45 * 60 * 1_000);
        stale_episode.duration_ms = Some(45 * 60 * 1_000);
        stale_episode.last_seen = Utc::now() - Duration::minutes(61);
        history.insert(stale_episode.history_id.clone(), stale_episode);
        save_session_history_sync(&path, &history, None).unwrap();

        let stale = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(stale.is_empty());

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn automatic_tadoku_rejects_media_without_target_audio() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-target-audio-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let mut episode = history_entry("english", "Frieren", 45 * 60 * 1_000);
        episode.duration_ms = Some(45 * 60 * 1_000);
        episode.last_seen = Utc::now() - Duration::minutes(31);
        episode.audio_languages = vec!["eng".to_string()];
        let history = HashMap::from([(episode.history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let automatic = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(automatic.is_empty());

        // The item stays visible to the review workflow so the user can
        // explicitly decline or otherwise handle it.
        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].history_id, "plex|english");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn automatic_tadoku_accepts_abs_target_library_path_when_track_metadata_is_wrong() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-abs-language-path-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let mut audiobook = history_entry("book", "Audiobook", 45 * 60 * 1_000);
        audiobook.server_kind = MediaServerKind::Audiobookshelf;
        audiobook.duration_ms = Some(5 * 60 * 60 * 1_000);
        audiobook.last_seen = Utc::now() - Duration::minutes(31);
        audiobook.audio_languages = vec!["eng".to_string()];
        audiobook.file_path =
            Some("/audiobooks/Japanese/With Audio/Audiobook/Audiobook 03.m4b".to_string());
        let history = HashMap::from([(audiobook.history_id.clone(), audiobook)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let batches = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].is_longform_checkpoint);
        assert_eq!(batches[0].duration_seconds, 45 * 60);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn manual_tadoku_accepts_stale_abs_longform_playtime_only() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-manual-abs-longform-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let series_title = "陰の実力者になりたくて！";
        let mut audiobook = history_entry("book-manual", series_title, 45 * 60 * 1_000);
        audiobook.title = format!("{series_title} — {series_title} 03.m4b");
        audiobook.history_id = "audiobookshelf|book-manual".to_string();
        audiobook.server_kind = MediaServerKind::Audiobookshelf;
        audiobook.duration_ms = Some(5 * 60 * 60 * 1_000);
        audiobook.last_seen = Utc::now() - Duration::hours(2);
        audiobook.audio_languages = vec!["eng".to_string()];
        audiobook.file_path = Some(
            "/audiobooks/Japanese/With Audio/陰の実力者になりたくて/陰の実力者になりたくて！ 03.m4b"
                .to_string(),
        );

        let mut movie = history_entry("movie-manual", "Film", 45 * 60 * 1_000);
        movie.duration_ms = Some(5 * 60 * 60 * 1_000);
        movie.last_seen = audiobook.last_seen;
        let history = HashMap::from([
            (audiobook.history_id.clone(), audiobook),
            (movie.history_id.clone(), movie),
        ]);
        save_session_history_sync(&path, &history, None).unwrap();

        let automatic = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert!(automatic.is_empty());

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].history_id, "audiobookshelf|book-manual");
        assert!(candidates[0].is_in_progress);
        assert_eq!(candidates[0].duration_seconds, 45 * 60);

        let selected = HashSet::from(["audiobookshelf|book-manual".to_string()]);
        let batches = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            Some(&selected),
            false,
            Some("jpn"),
        )
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].is_longform_checkpoint);
        assert_eq!(batches[0].duration_seconds, 45 * 60);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn manual_audiobook_checkpoints_keep_item_credit_and_final_progress() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-manual-checkpoints-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());
        let mut history = HashMap::new();
        for id in ["book-one", "book-two"] {
            let mut book = history_entry(id, "Shared title", 45 * 60 * 1_000);
            book.history_id = format!("audiobookshelf|{id}");
            book.server_kind = MediaServerKind::Audiobookshelf;
            book.title = "Shared title".into();
            book.duration_ms = Some(5 * 60 * 60 * 1_000);
            history.insert(book.history_id.clone(), book);
        }
        save_session_history_sync(&path, &history, None).unwrap();
        let selected = history.keys().cloned().collect::<HashSet<_>>();
        let prepare = || {
            prepare_tadoku_batches_with_policy_sync(
                &path,
                "2026-09-13",
                "jpn",
                Some(&selected),
                false,
                Some("jpn"),
            )
            .unwrap()
        };

        let batches = prepare();
        assert_eq!(batches.len(), 2);
        assert!(
            batches
                .iter()
                .all(|batch| batch.is_longform_checkpoint && batch.duration_seconds == 45 * 60)
        );
        let retried = prepare();
        assert_eq!(retried.len(), 2);
        assert!(
            retried
                .iter()
                .all(|batch| batch.duration_seconds == 45 * 60)
        );

        let conn = open_connection(&path).unwrap();
        conn.execute("UPDATE tadoku_export_batches SET status = 'completed'", [])
            .unwrap();
        drop(conn);
        for book in history.values_mut() {
            book.last_position_ms = book.duration_ms.unwrap();
        }
        save_session_history_sync(&path, &history, None).unwrap();
        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|card| card.duration_seconds == (5 * 60 - 45) * 60)
        );
        let final_batches = prepare();
        assert_eq!(final_batches.len(), 2);
        assert!(
            final_batches
                .iter()
                .all(|batch| batch.is_longform_checkpoint
                    && batch.duration_seconds == (5 * 60 - 45) * 60)
        );
        let retried = prepare();
        assert_eq!(retried.len(), 2);
        assert!(
            retried
                .iter()
                .all(|batch| batch.duration_seconds == (5 * 60 - 45) * 60)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn automatic_longform_path_is_limited_to_audiobookshelf() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-non-abs-longform-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        conn.execute(
            "UPDATE app_metadata SET value = ?1 WHERE key = 'tadoku_candidate_cutoff'",
            [(Utc::now() - Duration::days(1)).to_rfc3339()],
        )
        .unwrap();
        drop(conn);

        let mut movie = history_entry("movie", "Film", 3 * 60 * 60 * 1_000);
        movie.duration_ms = Some(3 * 60 * 60 * 1_000);
        movie.last_seen = Utc::now() - Duration::minutes(31);
        let history = HashMap::from([(movie.history_id.clone(), movie)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let batches = prepare_tadoku_batches_with_policy_sync(
            &path,
            "2026-08-24",
            "jpn",
            None,
            true,
            Some("jpn"),
        )
        .unwrap();
        assert_eq!(batches.len(), 1);
        assert!(!batches[0].is_longform_checkpoint);
        assert_eq!(batches[0].duration_seconds, 3 * 60 * 60);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tadoku_description_collapses_consecutive_episode_numbers() {
        let description = super::tadoku_batch_description(
            "NARUTO 疾風伝",
            [
                "NARUTO 疾風伝 S10E10 - Title 10",
                "NARUTO 疾風伝 S10E11 - Title 11",
                "NARUTO 疾風伝 S10E12 - Title 12",
                "NARUTO 疾風伝 S10E13 - Title 13",
            ],
        );
        assert_eq!(description, "NARUTO 疾風伝 S10E10-13");
        assert!(!description.contains("Nagare:"));
    }

    #[test]
    fn tadoku_description_recovers_date_based_episode_number_from_name() {
        let description = super::tadoku_batch_description(
            "水曜日のダウンタウン",
            ["水曜日のダウンタウン S2014E00 - Episode 002"],
        );
        assert_eq!(description, "水曜日のダウンタウン S2014E02");
    }

    #[test]
    fn tadoku_description_does_not_repeat_a_title_used_as_the_series_fallback() {
        let description =
            super::tadoku_batch_description("Monster House - E2", ["Monster House - E2"]);
        assert_eq!(description, "Monster House - E2");
    }

    #[test]
    fn tadoku_title_override_is_used_for_candidates_and_batches() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-title-override-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let mut episode = history_entry("monster-house-2", "", 90_000);
        episode.title = "Monster House - E2".to_string();
        episode.series_name = None;
        let history = HashMap::from([(episode.history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();

        set_tadoku_candidate_title_sync(
            &path,
            "plex|monster-house-2",
            Some("Monster House Episode 2"),
        )
        .unwrap();
        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates[0].title, "Monster House Episode 2");
        assert_eq!(candidates[0].series_name, "Monster House Episode 2");
        assert!(candidates[0].title_overridden);

        let selected = HashSet::from(["plex|monster-house-2".to_string()]);
        let batches =
            prepare_tadoku_batches_sync(&path, "2026-07-17", "jpn", Some(&selected)).unwrap();
        assert_eq!(batches[0].description, "Monster House Episode 2");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cutoff_migration_includes_episodes_watched_after_previous_sync() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-previous-sync-cutoff-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let conn = open_connection(&path).unwrap();
        let installation_cutoff: String = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        let installation_cutoff = chrono::DateTime::parse_from_rfc3339(&installation_cutoff)
            .unwrap()
            .with_timezone(&Utc);
        let previous_sync = installation_cutoff - chrono::Duration::hours(1);
        let watched_at = installation_cutoff - chrono::Duration::minutes(30);
        let empty_run_after_install = installation_cutoff + chrono::Duration::minutes(10);

        let mut episode = history_entry("after-sync", "Frieren", 90_000);
        episode.last_seen = watched_at;
        let history = HashMap::from([(episode.history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "INSERT INTO tadoku_export_runs (export_date, status, last_attempt_at) VALUES (?1, 'completed', ?2)",
            params!["2026-07-13", previous_sync.to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tadoku_export_runs (export_date, status, last_attempt_at) VALUES (?1, 'completed', ?2)",
            params!["2026-07-14", empty_run_after_install.to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM app_metadata WHERE key = 'tadoku_candidate_cutoff_previous_sync_v1'",
            [],
        )
        .unwrap();
        drop(conn);

        drop(open_connection(&path).unwrap());

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].history_id, "plex|after-sync");

        let conn = open_connection(&path).unwrap();
        let migrated_cutoff: String = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let migrated_cutoff = chrono::DateTime::parse_from_rfc3339(&migrated_cutoff)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(migrated_cutoff, previous_sync);

        drop(conn);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn excludes_history_from_before_candidate_cutoff() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-cutoff-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let mut old = history_entry("old", "Frieren", 90_000);
        old.last_seen = Utc::now() - chrono::Duration::days(1);
        let fresh = history_entry("fresh", "Frieren", 90_000);
        let history = HashMap::from([
            (old.history_id.clone(), old),
            (fresh.history_id.clone(), fresh),
        ]);
        save_session_history_sync(&path, &history, None).unwrap();

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].history_id, "plex|fresh");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cutoff_migration_retires_preexisting_pending_batches() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-pending-cutoff-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let episode = history_entry("1", "Frieren", 90_000);
        let history = HashMap::from([(episode.history_id.clone(), episode)]);
        save_session_history_sync(&path, &history, None).unwrap();
        let batches = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", None).unwrap();
        assert_eq!(batches.len(), 1);

        let conn = open_connection(&path).unwrap();
        conn.execute(
            "DELETE FROM app_metadata WHERE key = 'tadoku_candidate_cutoff'",
            [],
        )
        .unwrap();
        drop(conn);

        let conn = open_connection(&path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM tadoku_export_batches WHERE batch_id = ?1",
                [&batches[0].batch_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "declined");
        drop(conn);
        assert!(
            list_tadoku_candidates_sync(&path, "jpn")
                .unwrap()
                .is_empty()
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn declining_one_episode_releases_others_from_its_pending_batch() {
        let path = std::env::temp_dir().join(format!(
            "nagare-tadoku-decline-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        drop(open_connection(&path).unwrap());

        let first = history_entry("1", "Frieren", 90_000);
        let second = history_entry("2", "Frieren", 90_000);
        let history = HashMap::from([
            (first.history_id.clone(), first),
            (second.history_id.clone(), second),
        ]);
        save_session_history_sync(&path, &history, None).unwrap();
        let batches = prepare_tadoku_batches_sync(&path, "2026-07-12", "jpn", None).unwrap();
        assert_eq!(batches.len(), 1);

        let declined = decline_tadoku_candidates_sync(&path, &["plex|1".to_string()]).unwrap();
        assert_eq!(declined, 1);

        let candidates = list_tadoku_candidates_sync(&path, "jpn").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].history_id, "plex|2");
        assert!(!candidates[0].pending_retry);

        let conn = open_connection(&path).unwrap();
        let retired_status: String = conn
            .query_row(
                "SELECT status FROM tadoku_export_batches WHERE batch_id = ?1",
                [&batches[0].batch_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retired_status, "declined");
        drop(conn);
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
