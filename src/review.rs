use crate::mining::{AppDatabase, EnrichmentDialogState, open_connection};
use crate::subtitle::SubtitleTrack;
use anyhow::Context;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

#[derive(Serialize)]
pub struct ReviewSession {
    pub id: String,
    pub history_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub card_count: usize,
    pub reviewed_count: usize,
    pub enhanced_count: usize,
    pub imported: bool,
}

#[derive(Serialize)]
pub struct ReviewCard {
    pub dialog: EnrichmentDialogState,
    pub status: String,
    pub reviewed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
pub struct ReviewDetail {
    pub session: ReviewSession,
    pub track: SubtitleTrack,
    pub cards: Vec<ReviewCard>,
}

pub fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_sessions (
            id TEXT PRIMARY KEY, history_id TEXT NOT NULL, title TEXT NOT NULL,
            track_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            imported INTEGER NOT NULL DEFAULT 0, audio_index INTEGER, audio_ordinal INTEGER
        );
        CREATE TABLE IF NOT EXISTS review_cards (
            note_id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES review_sessions(id),
            dialog_json TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
            reviewed_at TEXT, last_error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_review_cards_session ON review_cards(session_id);
        CREATE INDEX IF NOT EXISTS idx_review_sessions_updated ON review_sessions(updated_at DESC);
        INSERT OR IGNORE INTO review_sessions
            (id, history_id, title, track_json, created_at, updated_at, imported)
            SELECT 'legacy:' || mn.history_id, mn.history_id, MAX(mn.title),
                COALESCE(st.track_json, '{\"lines\":[],\"offset_ms\":0}'),
                MIN(mn.created_at), MAX(mn.updated_at), 1
            FROM mined_notes mn LEFT JOIN subtitle_tracks st ON st.history_id = mn.history_id
            WHERE NOT EXISTS (SELECT 1 FROM review_cards rc WHERE rc.note_id = mn.note_id)
            GROUP BY mn.history_id;
        INSERT OR IGNORE INTO review_cards (note_id, session_id, dialog_json, status)
            SELECT note_id, 'legacy:' || history_id, json_object(
                'event', json(event_json), 'history_id', history_id,
                'matched_line_index', matched_line_index, 'start_ms', start_ms, 'end_ms', end_ms,
                'included_line_first', included_line_first, 'included_line_last', included_line_last,
                'generate_avif', json(CASE WHEN generate_avif THEN 'true' ELSE 'false' END),
                'card_ids', json(card_ids_json), 'source', 'mining_history', 'updated_at', updated_at
            ), 'enhanced' FROM mined_notes;
        UPDATE review_cards SET status = 'failed',
            last_error = 'Enhancement was interrupted when Nagare stopped. Review and retry.'
            WHERE status IN ('queued', 'running');",
    )?;
    Ok(())
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewSession> {
    Ok(ReviewSession {
        id: row.get(0)?,
        history_id: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        card_count: row.get(5)?,
        reviewed_count: row.get(6)?,
        enhanced_count: row.get(7)?,
        imported: row.get(8)?,
    })
}

const SESSION_SELECT: &str = "SELECT s.id, s.history_id, s.title, s.created_at, s.updated_at,
    COUNT(c.note_id), COALESCE(SUM(c.reviewed_at IS NOT NULL),0),
    COALESCE(SUM(c.status = 'enhanced'),0), s.imported
    FROM review_sessions s JOIN review_cards c ON c.session_id = s.id";

impl AppDatabase {
    async fn review_query<T: Send + 'static>(
        &self,
        f: impl FnOnce(Connection) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let path = self.db_path.clone();
        tokio::task::spawn_blocking(move || f(open_connection(&path)?))
            .await
            .context("Review database task failed")?
    }

    pub async fn record_review_card(
        &self,
        session_id: String,
        title: String,
        track: SubtitleTrack,
        dialog: EnrichmentDialogState,
        audio_mapping: (Option<u32>, Option<usize>),
    ) -> anyhow::Result<()> {
        self.review_query(move |mut conn| {
            // Acquire the writer before checking for an existing session so a
            // concurrent history save uses busy_timeout instead of failing a
            // deferred read-to-write transaction upgrade immediately.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let now = Utc::now().to_rfc3339();
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM review_sessions WHERE id = ?1)", [&session_id], |row| row.get(0))?;
            if exists {
                tx.execute("UPDATE review_sessions SET updated_at = ?2 WHERE id = ?1", params![session_id, now])?;
            } else {
            tx.execute("INSERT INTO review_sessions
                (id, history_id, title, track_json, created_at, updated_at, audio_index, audio_ordinal)
                VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at",
                params![session_id, dialog.history_id, title, serde_json::to_string(&track)?, now, audio_mapping.0, audio_mapping.1.map(|n| n as i64)])?;
            }
            // A duplicate notification must never erase enhancement or review progress.
            let status = if dialog.source == crate::mining::EnrichmentSource::MiningHistory { "existing" } else { "pending" };
            tx.execute("INSERT OR IGNORE INTO review_cards (note_id, session_id, dialog_json, status)
                VALUES (?1, ?2, ?3, ?4)",
                params![dialog.event.note_id, session_id, serde_json::to_string(&dialog)?, status])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn list_review_sessions(&self) -> anyhow::Result<Vec<ReviewSession>> {
        self.review_query(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{SESSION_SELECT} GROUP BY s.id ORDER BY s.updated_at DESC"
            ))?;
            Ok(stmt
                .query_map([], session_row)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn review_audio_mapping(
        &self,
        note_id: i64,
    ) -> anyhow::Result<Option<(Option<u32>, Option<usize>)>> {
        self.review_query(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT s.audio_index, s.audio_ordinal FROM review_sessions s
                JOIN review_cards c ON c.session_id = s.id WHERE c.note_id = ?1 AND s.imported = 0",
                    [note_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get::<_, Option<i64>>(1)?.map(|v| v as usize),
                        ))
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn review_detail(&self, id: String) -> anyhow::Result<Option<ReviewDetail>> {
        self.review_query(move |conn| {
            let session = conn
                .query_row(
                    &format!("{SESSION_SELECT} WHERE s.id = ?1 GROUP BY s.id"),
                    [&id],
                    session_row,
                )
                .optional()?;
            let Some(session) = session else {
                return Ok(None);
            };
            let raw: String = conn.query_row(
                "SELECT track_json FROM review_sessions WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )?;
            let mut stmt = conn.prepare(
                "SELECT dialog_json, status, reviewed_at, last_error
                FROM review_cards WHERE session_id = ?1 ORDER BY note_id",
            )?;
            let rows = stmt.query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })?;
            let mut cards = Vec::new();
            for row in rows {
                let (raw, status, reviewed_at, last_error) = row?;
                cards.push(ReviewCard {
                    dialog: serde_json::from_str(&raw)?,
                    status,
                    reviewed_at,
                    last_error,
                });
            }
            Ok(Some(ReviewDetail {
                session,
                track: serde_json::from_str(&raw)?,
                cards,
            }))
        })
        .await
    }

    pub async fn review_dialog(
        &self,
        note_id: i64,
    ) -> anyhow::Result<Option<EnrichmentDialogState>> {
        self.review_query(move |conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT dialog_json FROM review_cards WHERE note_id = ?1",
                    [note_id],
                    |row| row.get(0),
                )
                .optional()?;
            raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn backfill_review_card_ids(
        &self,
        note_id: i64,
        ids: Vec<i64>,
    ) -> anyhow::Result<()> {
        self.review_query(move |conn| {
            conn.execute("UPDATE review_cards SET dialog_json = json_set(dialog_json, '$.card_ids', json(?2))
                WHERE note_id = ?1", params![note_id, serde_json::to_string(&ids)?])?;
            Ok(())
        }).await
    }

    pub async fn update_review_card(
        &self,
        note_id: i64,
        status: &'static str,
        dialog: Option<EnrichmentDialogState>,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        self.review_query(move |conn| {
            let raw = dialog.map(|d| serde_json::to_string(&d)).transpose()?;
            conn.execute("UPDATE review_cards SET status = ?2, dialog_json = COALESCE(?3, dialog_json),
                last_error = ?4, reviewed_at = CASE WHEN ?2 IN ('queued', 'enhanced') THEN NULL ELSE reviewed_at END
                WHERE note_id = ?1", params![note_id, status, raw, error])?;
            Ok(())
        }).await
    }

    pub async fn mark_reviewed(&self, note_id: i64, reviewed: bool) -> anyhow::Result<bool> {
        self.review_query(move |conn| {
            Ok(conn.execute(
                "UPDATE review_cards SET reviewed_at = ?2 WHERE note_id = ?1
                AND status NOT IN ('queued', 'running')",
                params![note_id, reviewed.then(|| Utc::now().to_rfc3339())],
            )? > 0)
        })
        .await
    }

    pub async fn review_dialog_by_card_id(
        &self,
        card_id: i64,
    ) -> anyhow::Result<Option<EnrichmentDialogState>> {
        self.review_query(move |conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT dialog_json FROM review_cards
                WHERE EXISTS (SELECT 1 FROM json_each(dialog_json, '$.card_ids') WHERE value = ?1)",
                    [card_id],
                    |row| row.get(0),
                )
                .optional()?;
            raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
                .transpose()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anki::NewCardEvent;
    use crate::mining::EnrichmentSource;
    use crate::subtitle::parse_srt;
    use std::collections::HashMap;

    #[tokio::test]
    async fn snapshots_progress_duplicates_and_restarts() {
        let path = std::env::temp_dir().join(format!("nagare-review-{}.db", uuid::Uuid::new_v4()));
        let db = AppDatabase::new(path.clone(), None).await.unwrap();
        let track = parse_srt("1\n00:00:01,000 --> 00:00:02,000\n対象の文\n");
        let card = EnrichmentDialogState {
            matched_text: None,
            event: NewCardEvent {
                note_id: 1,
                sentence: "対象の文".into(),
                fields: HashMap::new(),
                model_name: "Mining".into(),
                tags: vec![],
            },
            history_id: Some("plex|one".into()),
            matched_line_index: Some(0),
            start_ms: Some(900),
            end_ms: Some(2500),
            generate_avif: Some(false),
            included_line_first: Some(0),
            included_line_last: Some(0),
            card_ids: vec![],
            source: EnrichmentSource::Pending,
            updated_at: Some(Utc::now()),
        };
        db.record_review_card(
            "session-one".into(),
            "Episode".into(),
            track.clone(),
            card.clone(),
            (Some(2), Some(1)),
        )
        .await
        .unwrap();
        db.update_review_card(1, "enhanced", Some(card.clone()), None)
            .await
            .unwrap();
        assert!(db.mark_reviewed(1, true).await.unwrap());
        db.record_review_card(
            "session-one".into(),
            "Episode".into(),
            track,
            card,
            (Some(4), Some(3)),
        )
        .await
        .unwrap();
        assert_eq!(
            db.review_audio_mapping(1).await.unwrap(),
            Some((Some(2), Some(1)))
        );
        let detail = db
            .review_detail("session-one".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.session.card_count, 1);
        assert_eq!(detail.session.reviewed_count, 1);
        assert_eq!(detail.track.lines[0].text, "対象の文");
        db.update_review_card(1, "queued", None, None)
            .await
            .unwrap();
        assert!(!db.mark_reviewed(1, true).await.unwrap());
        let db = AppDatabase::new(path.clone(), None).await.unwrap();
        let detail = db
            .review_detail("session-one".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.cards[0].status, "failed");
        assert_eq!(detail.session.reviewed_count, 0);
        assert!(db.review_detail("missing".into()).await.unwrap().is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn imports_older_mined_notes_once_without_reassigning_new_sessions() {
        use crate::config::MediaServerKind;
        use crate::mining::MiningHistoryEntry;
        let path = std::env::temp_dir().join(format!(
            "nagare-review-migration-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = AppDatabase::new(path.clone(), None).await.unwrap();
        let event = NewCardEvent {
            note_id: 42,
            sentence: "保存した文".into(),
            fields: HashMap::new(),
            model_name: "Mining".into(),
            tags: vec![],
        };
        let entry = MiningHistoryEntry {
            note_id: 42,
            card_ids: vec![43],
            history_id: "plex|legacy".into(),
            server_kind: MediaServerKind::Plex,
            item_id: "legacy".into(),
            media_source_id: "legacy".into(),
            file_path: None,
            title: "Earlier episode".into(),
            event,
            start_ms: 1_000,
            end_ms: 2_000,
            generate_avif: false,
            matched_line_index: Some(0),
            included_line_first: Some(0),
            included_line_last: Some(0),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.upsert_mined_note(entry.clone()).await.unwrap();
        let db = AppDatabase::new(path.clone(), None).await.unwrap();
        let imported = db
            .review_detail("legacy:plex|legacy".into())
            .await
            .unwrap()
            .unwrap();
        assert!(imported.session.imported);
        assert_eq!(imported.cards[0].status, "enhanced");
        assert_eq!(imported.cards[0].dialog.card_ids, vec![43]);
        assert_eq!(
            db.review_dialog_by_card_id(43)
                .await
                .unwrap()
                .unwrap()
                .event
                .note_id,
            42
        );
        db.mark_reviewed(42, true).await.unwrap();
        // A current capture with an existing mined-note ID must not move it to
        // a fresh session or discard its review marker on the next startup.
        db.record_review_card(
            "new-session".into(),
            "Earlier episode".into(),
            SubtitleTrack {
                lines: vec![],
                offset_ms: 0,
            },
            entry.dialog_state(),
            (None, None),
        )
        .await
        .unwrap();
        let db = AppDatabase::new(path.clone(), None).await.unwrap();
        let sessions = db.list_review_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].reviewed_count, 1);
        std::fs::remove_file(path).unwrap();
    }
}
