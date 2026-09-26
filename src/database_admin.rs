//! The advanced database tool uses isolated connections to the live database.
//! It deliberately bypasses application caches, but cannot open other files.
use crate::api::AppState;
use anyhow::{Context, bail, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::{
    Batch, Connection, OpenFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    params_from_iter,
    types::{Value, ValueRef},
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_ROWS: usize = 1000;
const MAX_CELL_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/advanced/database/schema", get(schema))
        .route("/api/advanced/database/query", post(query))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .route_layer(middleware::from_fn(require_same_origin))
}

// The rest of Nagare supports cross-origin companion clients. Raw SQL must not
// inherit that access: require a custom header AND reject cross-origin browsers.
fn allowed_request(headers: &HeaderMap) -> bool {
    if headers
        .get("x-nagare-database")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return false;
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| site != "same-origin" && site != "none")
    {
        return false;
    }
    if let Some(origin) = headers.get("origin") {
        let Ok(origin) = origin.to_str().unwrap_or_default().parse::<Uri>() else {
            return false;
        };
        if !matches!(origin.scheme_str(), Some("http" | "https")) {
            return false;
        }
        return origin.authority().map(|a| a.as_str())
            == headers.get("host").and_then(|v| v.to_str().ok());
    }
    true
}

async fn require_same_origin(request: Request, next: Next) -> Response {
    if !allowed_request(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Open the database tool from the same Nagare server."
            })),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

fn open_database(path: &Path, writable: bool) -> anyhow::Result<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024);
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 128 * 1024 * 1024);
    Ok(conn)
}

type ApiResult = Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)>;

async fn database_task<T: Serialize + Send + 'static>(
    task: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> ApiResult {
    match tokio::task::spawn_blocking(task).await {
        Ok(Ok(result)) => Ok(Json(serde_json::to_value(result).unwrap())),
        Ok(Err(error)) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )),
        Err(error) => {
            tracing::error!(%error, "Database tool task failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Database task failed." })),
            ))
        }
    }
}

#[derive(Serialize)]
struct Column {
    name: String,
    data_type: String,
    not_null: bool,
    default_value: Option<String>,
    pk: i64,
    hidden: i64,
}

#[derive(Serialize)]
struct Table {
    name: String,
    kind: String,
    sql: Option<String>,
    columns: Vec<Column>,
    rowid: Option<String>,
}

fn read_schema(conn: &Connection) -> anyhow::Result<Vec<Table>> {
    let mut statement = conn.prepare(
        "SELECT s.name, p.type, s.sql, p.wr FROM sqlite_schema s
         JOIN pragma_table_list p ON p.name = s.name AND p.schema = 'main'
         WHERE s.type IN ('table', 'view') AND s.name NOT LIKE 'sqlite_%'
         AND p.type != 'shadow' ORDER BY s.name",
    )?;
    let mut rows = statement.query([])?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let mut columns = conn.prepare("SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1, 'main')")?;
        let columns = columns
            .query_map([&name], |row| {
                Ok(Column {
                    name: row.get(0)?,
                    data_type: row.get(1)?,
                    not_null: row.get(2)?,
                    default_value: row.get(3)?,
                    pk: row.get(4)?,
                    hidden: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let rowid = if kind == "table" && row.get::<_, i64>(3)? == 0 {
            ["_rowid_", "rowid", "oid"]
                .into_iter()
                .find(|alias| !columns.iter().any(|c| c.name.eq_ignore_ascii_case(alias)))
                .map(str::to_owned)
        } else {
            None
        };
        tables.push(Table {
            name,
            kind,
            sql: row.get(2)?,
            columns,
            rowid,
        });
    }
    Ok(tables)
}

async fn schema(State(state): State<Arc<AppState>>) -> ApiResult {
    let path = state.db.db_path.clone();
    database_task(move || {
        let conn = open_database(&path, false)?;
        Ok(serde_json::json!({ "database": "nagare.sqlite", "tables": read_schema(&conn)? }))
    })
    .await
}

// Integers travel as strings: SQLite IDs can exceed JavaScript's safe integers.
// Large text/blob cells are explicitly previews and cannot be submitted as edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cell {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
}

impl Cell {
    fn read(value: ValueRef<'_>) -> anyhow::Result<Self> {
        let mut cell = Self {
            kind: "null".into(),
            value: String::new(),
            truncated: false,
            bytes: None,
        };
        match value {
            ValueRef::Null => {}
            ValueRef::Integer(value) => {
                cell.kind = "integer".into();
                cell.value = value.to_string();
            }
            ValueRef::Real(value) => {
                cell.kind = "real".into();
                cell.value = value.to_string();
            }
            ValueRef::Text(value) => {
                cell.kind = "text".into();
                let text = std::str::from_utf8(value).context(
                    "A TEXT cell contains invalid UTF-8; use hex(column) to inspect it.",
                )?;
                let mut end = text.len().min(MAX_CELL_BYTES);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                cell.value = text[..end].to_owned();
                cell.truncated = end < text.len();
                cell.bytes = Some(value.len());
            }
            ValueRef::Blob(value) => {
                cell.kind = "blob".into();
                cell.bytes = Some(value.len());
                cell.truncated = value.len() > 64;
                cell.value = value
                    .iter()
                    .take(64)
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
            }
        }
        Ok(cell)
    }

    fn parameter(&self) -> anyhow::Result<Value> {
        ensure!(
            !self.truncated,
            "A preview cannot be used as a value. Use SQL to edit this cell."
        );
        Ok(match self.kind.as_str() {
            "null" => Value::Null,
            "text" => Value::Text(self.value.clone()),
            "integer" => Value::Integer(self.value.parse().context("Invalid 64-bit integer")?),
            "real" => {
                let value: f64 = self.value.parse().context("Invalid real number")?;
                ensure!(value.is_finite(), "Real numbers must be finite");
                Value::Real(value)
            }
            "blob" => {
                ensure!(
                    self.value.len() % 2 == 0 && self.value.bytes().all(|c| c.is_ascii_hexdigit()),
                    "BLOB values must contain pairs of hexadecimal digits"
                );
                Value::Blob(
                    (0..self.value.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&self.value[i..i + 2], 16).unwrap())
                        .collect(),
                )
            }
            _ => bail!("Unknown SQLite value type"),
        })
    }
}

#[derive(Deserialize)]
struct QueryRequest {
    sql: String,
    #[serde(default)]
    params: Vec<Cell>,
    #[serde(default)]
    allow_write: bool,
    expected_changes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<Cell>>,
    changes: u64,
    readonly: bool,
    truncated: bool,
    elapsed_ms: u128,
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::CreateVtable { .. }
        | AuthAction::DropVtable { .. } => Authorization::Deny,
        AuthAction::Function { function_name }
            if ["load_extension", "writefile", "readfile"]
                .contains(&function_name.to_ascii_lowercase().as_str()) =>
        {
            Authorization::Deny
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => {
            let name = pragma_name.to_ascii_lowercase();
            let metadata = [
                "table_info",
                "table_xinfo",
                "index_list",
                "index_info",
                "index_xinfo",
                "foreign_key_list",
                "table_list",
            ]
            .contains(&name.as_str());
            let readonly = pragma_value.is_none()
                && [
                    "database_list",
                    "user_version",
                    "schema_version",
                    "foreign_keys",
                    "integrity_check",
                    "quick_check",
                    "page_count",
                    "freelist_count",
                ]
                .contains(&name.as_str());
            if metadata || readonly {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Allow,
    }
}

fn run_query(
    conn: &mut Connection,
    request: QueryRequest,
    timeout: Duration,
) -> anyhow::Result<QueryResult> {
    ensure!(!request.sql.trim().is_empty(), "Enter a SQL statement.");
    let started = Instant::now();
    conn.pragma_update(None, "query_only", !request.allow_write)?;
    let tx = conn.transaction()?;
    tx.progress_handler(10_000, Some(move || started.elapsed() > timeout));
    tx.authorizer(Some(authorize));
    let result = (|| {
        let mut batch = Batch::new(&tx, &request.sql);
        let mut statement = batch.next()?.context("Enter a SQL statement.")?;
        ensure!(
            batch.next()?.is_none(),
            "Run one SQL statement at a time. Nothing was executed."
        );
        let readonly = statement.readonly();
        ensure!(
            readonly || request.allow_write,
            "Enable writes before running a statement that changes the database."
        );
        let params = request
            .params
            .iter()
            .map(Cell::parameter)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let columns = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut rows = statement.query(params_from_iter(params))?;
        let mut output = Vec::new();
        let mut bytes = 0;
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if output.len() == MAX_ROWS {
                truncated = true;
                if readonly {
                    break;
                }
                bail!(
                    "Write result exceeds 1,000 rows; the statement was rolled back. Use a smaller RETURNING result."
                );
            }
            let values = (0..columns.len())
                .map(|i| Cell::read(row.get_ref(i)?))
                .collect::<anyhow::Result<Vec<_>>>()?;
            bytes += values
                .iter()
                .map(|cell| cell.value.len() + 96)
                .sum::<usize>();
            if bytes > MAX_RESULT_BYTES {
                truncated = true;
                if readonly {
                    break;
                }
                bail!("Write result is too large; the statement was rolled back.");
            }
            output.push(values);
        }
        drop(rows);
        let changes = if readonly { 0 } else { tx.changes() };
        if let Some(expected) = request.expected_changes {
            ensure!(
                changes == expected,
                "The row changed or no longer exists. Refresh and try again. Nothing was saved."
            );
        }
        Ok(QueryResult {
            columns,
            rows: output,
            changes,
            readonly,
            truncated,
            elapsed_ms: started.elapsed().as_millis(),
        })
    })();
    // Remove hooks before committing or rolling back the enclosing transaction.
    tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    tx.progress_handler(0, None::<fn() -> bool>);
    let result = result?;
    tx.commit()?;
    Ok(result)
}

async fn query(State(state): State<Arc<AppState>>, Json(request): Json<QueryRequest>) -> ApiResult {
    let path = state.db.db_path.clone();
    database_task(move || {
        let mut conn = open_database(&path, request.allow_write)?;
        run_query(&mut conn, request, Duration::from_secs(5))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(sql: &str, writable: bool) -> QueryRequest {
        QueryRequest {
            sql: sql.into(),
            params: vec![],
            allow_write: writable,
            expected_changes: None,
        }
    }

    fn execute(conn: &mut Connection, sql: &str, writable: bool) -> anyhow::Result<QueryResult> {
        run_query(conn, request(sql, writable), Duration::from_secs(5))
    }

    #[test]
    fn values_round_trip_without_losing_integer_precision_or_nulls() {
        let mut conn = Connection::open_in_memory().unwrap();
        let result = execute(
            &mut conn,
            "SELECT 9223372036854775807, NULL, '', '日本語', x'00ff', 1.5",
            false,
        )
        .unwrap();
        assert_eq!(result.rows[0][0].value, "9223372036854775807");
        let mut bound = request("SELECT ?, ?, ?, ?, ?, ?", false);
        bound.params = result.rows[0].clone();
        let repeated = run_query(&mut conn, bound, Duration::from_secs(5)).unwrap();
        assert_eq!(
            serde_json::to_value(result.rows).unwrap(),
            serde_json::to_value(repeated.rows).unwrap()
        );
    }

    #[test]
    fn writes_require_opt_in_and_failed_edits_roll_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT UNIQUE); INSERT INTO items VALUES(1, 'one'), (2, 'two');").unwrap();
        assert!(execute(&mut conn, "DELETE FROM items", false).is_err());
        let mut edit = request("UPDATE items SET name = 'changed' WHERE id = 1", true);
        edit.expected_changes = Some(2);
        assert!(run_query(&mut conn, edit, Duration::from_secs(5)).is_err());
        assert_eq!(
            execute(&mut conn, "SELECT name FROM items WHERE id=1", false)
                .unwrap()
                .rows[0][0]
                .value,
            "one"
        );
        assert!(execute(&mut conn, "UPDATE items SET name='same'", true).is_err());
        assert_eq!(
            execute(&mut conn, "INSERT INTO items VALUES(3, 'three')", true)
                .unwrap()
                .changes,
            1
        );
        assert_eq!(
            execute(
                &mut conn,
                "UPDATE items SET name='three!' WHERE id=3 RETURNING name",
                true
            )
            .unwrap()
            .rows[0][0]
                .value,
            "three!"
        );
        assert_eq!(
            execute(&mut conn, "DELETE FROM items WHERE id=3", true)
                .unwrap()
                .changes,
            1
        );
    }

    #[test]
    fn other_files_unsafe_pragmas_and_multi_statements_are_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES(1);",
        )
        .unwrap();
        for sql in [
            "ATTACH ':memory:' AS other",
            "PRAGMA writable_schema=ON",
            "PRAGMA journal_mode=OFF",
            "COMMIT",
            "DELETE FROM items; SELECT 1",
            "VACUUM INTO 'unwanted.sqlite'",
        ] {
            assert!(execute(&mut conn, sql, true).is_err(), "{sql}");
        }
        assert_eq!(
            execute(
                &mut conn,
                "SELECT count(*) FROM items; -- trailing comment",
                false
            )
            .unwrap()
            .rows[0][0]
                .value,
            "1"
        );
        assert!(
            !execute(&mut conn, "PRAGMA table_info(items)", false)
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn limits_previews_and_timeout_leave_the_database_usable() {
        let mut conn = Connection::open_in_memory().unwrap();
        let result = execute(&mut conn, "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1100) SELECT x FROM n", false).unwrap();
        assert_eq!(result.rows.len(), MAX_ROWS);
        assert!(result.truncated);
        let result = execute(
            &mut conn,
            "SELECT zeroblob(100), printf('%.*c', 70000, 'a')",
            false,
        )
        .unwrap();
        assert!(
            result.rows[0]
                .iter()
                .all(|cell| cell.truncated && cell.parameter().is_err())
        );
        assert!(run_query(&mut conn, request("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n", false), Duration::ZERO).is_err());
        assert!(execute(&mut conn, "SELECT 1", false).is_ok());
    }

    #[test]
    fn schema_handles_composite_keys_views_and_shadowed_rowids() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE composite(a TEXT, b INTEGER, PRIMARY KEY(a,b)) WITHOUT ROWID; CREATE TABLE shadow(rowid TEXT); CREATE VIEW names AS SELECT a FROM composite;").unwrap();
        let tables = read_schema(&conn).unwrap();
        assert_eq!(tables.len(), 3);
        assert!(tables[0].rowid.is_none());
        assert_eq!(tables[0].columns[1].pk, 2);
        assert_eq!(tables[1].kind, "view");
        assert_eq!(tables[2].rowid.as_deref(), Some("_rowid_"));
    }

    #[test]
    fn database_access_is_not_exposed_to_cross_origin_companions() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "nagare.local:9470".parse().unwrap());
        assert!(!allowed_request(&headers));
        headers.insert("x-nagare-database", "1".parse().unwrap());
        headers.insert("origin", "http://nagare.local:9470".parse().unwrap());
        assert!(allowed_request(&headers));
        headers.insert("origin", "http://other.local".parse().unwrap());
        assert!(!allowed_request(&headers));
        headers.insert("origin", "null".parse().unwrap());
        assert!(!allowed_request(&headers));
        headers.remove("origin");
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!allowed_request(&headers));
    }
}
