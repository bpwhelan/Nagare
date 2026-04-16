use crate::anki::NewCardEvent;
use crate::config::{Config, MediaServerKind};
use crate::session::HistoryEntry;
use crate::subtitle::SubtitleTrack;
use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
        ",
    )
    .context("Failed to initialize SQLite schema")?;
    Ok(conn)
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
                media_source_id,
                file_path,
                duration_ms,
                subtitle_count,
                last_position_ms,
                last_seen
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(history_id) DO UPDATE SET
                server_kind = excluded.server_kind,
                item_id = excluded.item_id,
                title = excluded.title,
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
            last_seen
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
