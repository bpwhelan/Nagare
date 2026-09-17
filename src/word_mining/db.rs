use super::{Workspace, jobs::Job};
use crate::mining::open_connection;
use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf};

pub fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS miner_workspaces (history_id TEXT PRIMARY KEY, data_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS miner_known (term TEXT PRIMARY KEY, status TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS miner_anki_words (scope TEXT NOT NULL, term TEXT NOT NULL, PRIMARY KEY(scope, term));
        CREATE TABLE IF NOT EXISTS miner_dictionary (id INTEGER PRIMARY KEY CHECK(id=1), archive BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS miner_note_fields (note_id INTEGER PRIMARY KEY, data_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS miner_jobs (id TEXT PRIMARY KEY, history_id TEXT NOT NULL, status TEXT NOT NULL,
            data_json TEXT NOT NULL, cancel_requested INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS miner_jobs_history ON miner_jobs(history_id, created_at DESC);
        UPDATE miner_jobs SET status = 'paused' WHERE status = 'running';")?;
    Ok(())
}

pub async fn query<T: Send + 'static>(
    path: PathBuf,
    f: impl FnOnce(Connection) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(move || f(open_connection(&path)?))
        .await
        .context("Mining database task failed")?
}

pub async fn workspace(path: PathBuf, id: String) -> anyhow::Result<Option<Workspace>> {
    query(path, move |conn| {
        let json: Option<String> = conn
            .query_row(
                "SELECT data_json FROM miner_workspaces WHERE history_id=?1",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(Into::into)
    })
    .await
}

pub async fn save_workspace(path: PathBuf, workspace: Workspace) -> anyhow::Result<()> {
    query(path, move |conn| {
        conn.execute("INSERT INTO miner_workspaces VALUES (?1,?2) ON CONFLICT(history_id) DO UPDATE SET data_json=excluded.data_json",
            params![workspace.history.history_id, serde_json::to_string(&workspace)?])?;
        Ok(())
    }).await
}

pub async fn known(path: PathBuf) -> anyhow::Result<BTreeMap<String, String>> {
    query(path, |conn| {
        let mut stmt = conn.prepare("SELECT term, 'in_anki' FROM miner_anki_words UNION ALL SELECT term,status FROM miner_known")?;
        Ok(stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<BTreeMap<_,_>,_>>()?)
    }).await
}

pub async fn set_known(path: PathBuf, terms: Vec<String>, status: String) -> anyhow::Result<()> {
    query(path, move |mut conn| {
        let tx = conn.transaction()?;
        for term in terms {
            if status == "new" {
                tx.execute("DELETE FROM miner_known WHERE term=?1", [&term])?;
                tx.execute("DELETE FROM miner_anki_words WHERE term=?1", [&term])?;
            } else {
                tx.execute("INSERT INTO miner_known VALUES (?1,?2) ON CONFLICT(term) DO UPDATE SET status=excluded.status", params![term,status])?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await
}

pub async fn sync_known(path: PathBuf, scope: String, words: Vec<String>) -> anyhow::Result<()> {
    query(path, move |mut conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM miner_anki_words WHERE scope=?1", [&scope])?;
        for word in words {
            tx.execute(
                "INSERT OR IGNORE INTO miner_anki_words VALUES (?1,?2)",
                params![scope, word],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

pub async fn dictionary_archive(path: PathBuf) -> anyhow::Result<Option<Vec<u8>>> {
    query(path, |conn| {
        Ok(conn
            .query_row(
                "SELECT archive FROM miner_dictionary WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    })
    .await
}

pub async fn save_dictionary(path: PathBuf, bytes: Vec<u8>) -> anyhow::Result<()> {
    query(path, move |conn| {
        conn.execute("INSERT INTO miner_dictionary VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET archive=excluded.archive", [bytes])?;
        Ok(())
    }).await
}

pub async fn save_job(path: PathBuf, job: Job) -> anyhow::Result<()> {
    query(path, move |conn| {
        conn.execute("INSERT INTO miner_jobs (id,history_id,status,data_json,created_at) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(id) DO UPDATE SET status=excluded.status, data_json=excluded.data_json",
            params![job.id,job.history.history_id,job.status,serde_json::to_string(&job)?,job.created_at])?;
        Ok(())
    }).await
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get(0)?, row.get(1)?))
}
fn decode_job((json, status): (String, String)) -> anyhow::Result<Job> {
    let mut job: Job = serde_json::from_str(&json)?;
    job.status = status;
    Ok(job)
}

pub async fn job(path: PathBuf, id: String) -> anyhow::Result<Option<Job>> {
    query(path, move |conn| {
        conn.query_row(
            "SELECT data_json,status FROM miner_jobs WHERE id=?1",
            [id],
            read_job,
        )
        .optional()?
        .map(decode_job)
        .transpose()
    })
    .await
}

#[derive(Serialize)]
pub struct JobSummary {
    pub id: String,
    pub status: String,
    pub total: usize,
    pub created: usize,
    pub failed: usize,
    pub created_at: String,
}

pub async fn jobs(path: PathBuf, id: String) -> anyhow::Result<Vec<JobSummary>> {
    query(path, move |conn| {
        let mut stmt = conn.prepare("SELECT data_json,status FROM miner_jobs WHERE history_id=?1 ORDER BY created_at DESC LIMIT 20")?;
        let rows = stmt.query_map([id], read_job)?.collect::<Result<Vec<_>,_>>()?;
        rows.into_iter().map(|row| {
            let job = decode_job(row)?;
            Ok(JobSummary { id:job.id, status:job.status, total:job.cards.len(),
                created:job.cards.iter().filter(|c| c.status=="created").count(),
                failed:job.cards.iter().filter(|c| c.status=="failed").count(), created_at:job.created_at })
        }).collect()
    }).await
}

pub async fn cancel(path: PathBuf, id: String, requested: bool) -> anyhow::Result<()> {
    query(path, move |conn| {
        conn.execute(
            "UPDATE miner_jobs SET cancel_requested=?2 WHERE id=?1",
            params![id, requested],
        )?;
        Ok(())
    })
    .await
}

pub async fn cancelled(path: PathBuf, id: String) -> anyhow::Result<bool> {
    query(path, move |conn| {
        Ok(conn.query_row(
            "SELECT cancel_requested FROM miner_jobs WHERE id=?1",
            [id],
            |row| row.get(0),
        )?)
    })
    .await
}

pub async fn note_fields(
    path: PathBuf,
    note_id: i64,
) -> anyhow::Result<Option<super::jobs::Fields>> {
    query(path, move |conn| {
        let json: Option<String> = conn
            .query_row(
                "SELECT data_json FROM miner_note_fields WHERE note_id=?1",
                [note_id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(Into::into)
    })
    .await
}

pub async fn save_note_fields(
    path: PathBuf,
    note_id: i64,
    fields: super::jobs::Fields,
) -> anyhow::Result<()> {
    query(path, move |conn| {
        conn.execute("INSERT INTO miner_note_fields VALUES (?1,?2) ON CONFLICT(note_id) DO UPDATE SET data_json=excluded.data_json", params![note_id,serde_json::to_string(&fields)?])?;
        Ok(())
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn known_sync_replaces_one_scope_and_preserves_manual_choices() {
        let dir = std::env::temp_dir().join(format!("nagare-miner-{}", uuid::Uuid::new_v4()));
        let path = dir.join("test.sqlite");
        let conn = open_connection(&path).unwrap();
        initialize(&conn).unwrap();
        drop(conn);
        sync_known(
            path.clone(),
            "deck-a".into(),
            vec!["猫".into(), "犬".into()],
        )
        .await
        .unwrap();
        set_known(path.clone(), vec!["猫".into()], "ignored".into())
            .await
            .unwrap();
        sync_known(path.clone(), "deck-b".into(), vec!["鳥".into()])
            .await
            .unwrap();
        sync_known(path.clone(), "deck-a".into(), vec![])
            .await
            .unwrap();
        let words = known(path).await.unwrap();
        assert_eq!(words.get("猫").map(String::as_str), Some("ignored"));
        assert!(words.contains_key("鳥"));
        assert!(!words.contains_key("犬"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
