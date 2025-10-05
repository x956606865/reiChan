use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::job_runner::{JobProgress, JobState};
use super::types::TokenRow;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[cfg(feature = "notion-sqlite")]
use rusqlite::params;
#[cfg(feature = "notion-sqlite")]
use rusqlite::OptionalExtension;

pub trait TokenStore: Send + Sync {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow;
    fn list(&self) -> Vec<TokenRow>;
    fn delete(&self, id: &str) -> bool;
    fn get_token(&self, id: &str) -> Option<String>;
}

#[derive(Default)]
pub struct InMemoryTokenStore {
    inner: Mutex<StoreInner>,
}

#[derive(Default)]
struct StoreInner {
    seq: u64,
    rows: HashMap<String, (TokenRow, String)>, // id -> (row, token_plain)
}

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(seq: &mut u64) -> String {
        *seq += 1;
        let now_ms = chrono::Utc::now().timestamp_millis();
        format!("tok-{}-{}", now_ms, *seq)
    }
}

impl TokenStore for InMemoryTokenStore {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow {
        let mut guard = self.inner.lock().expect("poisoned");
        let id = Self::next_id(&mut guard.seq);
        let now = chrono::Utc::now().timestamp_millis();
        let row = TokenRow {
            id: id.clone(),
            name: name.to_string(),
            workspace_name,
            created_at: now,
            last_used_at: Some(now),
        };
        guard
            .rows
            .insert(id.clone(), (row.clone(), token.to_string()));
        row
    }

    fn list(&self) -> Vec<TokenRow> {
        let guard = self.inner.lock().expect("poisoned");
        let mut rows: Vec<_> = guard.rows.values().map(|(r, _)| r.clone()).collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        rows
    }

    fn delete(&self, id: &str) -> bool {
        let mut guard = self.inner.lock().expect("poisoned");
        guard.rows.remove(id).is_some()
    }

    fn get_token(&self, id: &str) -> Option<String> {
        let mut guard = self.inner.lock().expect("poisoned");
        if let Some((row, token)) = guard.rows.get_mut(id) {
            row.last_used_at = Some(chrono::Utc::now().timestamp_millis());
            Some(token.clone())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_roundtrip() {
        let store = InMemoryTokenStore::new();
        let saved = store.save("demo", "secret-123", Some("Workspace".into()));
        assert!(saved.id.starts_with("tok-"));
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "demo");
        let token = store.get_token(&saved.id).unwrap();
        assert_eq!(token, "secret-123");
        assert!(store.delete(&saved.id));
        assert!(store.list().is_empty());
    }
}

// -----------------------------
// SQLite-backed TokenStore
// -----------------------------

#[cfg(feature = "notion-sqlite")]
pub struct SqliteTokenStore {
    db_path: PathBuf,
}

#[cfg(feature = "notion-sqlite")]
impl SqliteTokenStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

#[cfg(feature = "notion-sqlite")]
impl TokenStore for SqliteTokenStore {
    fn save(&self, name: &str, token: &str, workspace_name: Option<String>) -> TokenRow {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let now = chrono::Utc::now().timestamp_millis();
        // Use SQLite to generate a random 128-bit id.
        let mut stmt = conn
            .prepare(
                "INSERT INTO notion_tokens (id, name, token_cipher, workspace_name, created_at, last_used_at, encryption_salt)
                 VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, NULL)
                 RETURNING id",
            )
            .expect("prepare insert");
        let id: String = stmt
            .query_row((name, token, workspace_name.clone(), now, now), |row| {
                row.get(0)
            })
            .expect("insert row");
        TokenRow {
            id,
            name: name.to_string(),
            workspace_name,
            created_at: now,
            last_used_at: Some(now),
        }
    }

    fn list(&self) -> Vec<TokenRow> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let mut stmt = conn
            .prepare(
                "SELECT id, name, workspace_name, created_at, last_used_at
                 FROM notion_tokens ORDER BY created_at",
            )
            .expect("prepare list");
        let rows = stmt
            .query_map([], |row| {
                Ok(TokenRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    workspace_name: row.get(2)?,
                    created_at: row.get(3)?,
                    last_used_at: row.get(4)?,
                })
            })
            .expect("query map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn delete(&self, id: &str) -> bool {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let affected = conn
            .execute("DELETE FROM notion_tokens WHERE id = ?1", [id])
            .expect("delete token");
        affected > 0
    }

    fn get_token(&self, id: &str) -> Option<String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).expect("open db");
        let now = chrono::Utc::now().timestamp_millis();
        let _ = conn
            .execute(
                "UPDATE notion_tokens SET last_used_at = ?2 WHERE id = ?1",
                (id, now),
            )
            .ok();
        // token_cipher is declared as BLOB but stores UTF-8 text in M1.
        // Read as String for maximum compatibility; future encryption can switch representation safely.
        let mut stmt = conn
            .prepare("SELECT token_cipher FROM notion_tokens WHERE id = ?1")
            .expect("prepare select token");
        let token: Option<String> = stmt.query_row([id], |row| row.get::<_, String>(0)).ok();
        token
    }
}

// -----------------------------
// Import job storage (M3)
// -----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportJobRowStatus {
    Ok,
    Failed,
    Skipped,
}

impl ImportJobRowStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    fn from_str(state: &str) -> Option<Self> {
        match state {
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportJobRecord {
    pub id: String,
    pub token_id: String,
    pub database_id: String,
    pub source_file_path: String,
    pub state: JobState,
    pub progress: JobProgress,
    pub config_snapshot_json: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub next_offset: usize,
    pub rps: Option<f64>,
    pub last_error: Option<String>,
    pub last_heartbeat: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ImportJobRowRecord {
    pub job_id: String,
    pub row_index: usize,
    pub status: ImportJobRowStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub error_payload_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewImportJob {
    pub id: String,
    pub token_id: String,
    pub database_id: String,
    pub source_file_path: String,
    pub config_snapshot_json: String,
    pub total: Option<usize>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressUpdate {
    pub total: Option<usize>,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub next_offset: Option<usize>,
    pub rps: Option<f64>,
    pub last_error: Option<String>,
    pub heartbeat_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct StateTransition {
    pub state: JobState,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub last_error: Option<String>,
}

impl Default for StateTransition {
    fn default() -> Self {
        Self {
            state: JobState::Pending,
            started_at: None,
            ended_at: None,
            last_error: None,
        }
    }
}

pub trait ImportJobStore: Send + Sync {
    fn insert_job(&self, job: NewImportJob) -> Result<ImportJobRecord, String>;
    fn update_progress(&self, job_id: &str, update: ProgressUpdate) -> Result<(), String>;
    fn mark_state(&self, job_id: &str, transition: StateTransition) -> Result<(), String>;
    fn append_row_results(&self, rows: Vec<ImportJobRowRecord>) -> Result<(), String>;
    fn load_job(&self, job_id: &str) -> Result<Option<ImportJobRecord>, String>;
    fn list_pending_jobs(&self) -> Result<Vec<ImportJobRecord>, String>;
    fn list_recent_failures(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportJobRowRecord>, String>;
}

fn job_state_to_str(state: JobState) -> &'static str {
    match state {
        JobState::Pending => "pending",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Completed => "succeeded",
        JobState::Failed => "failed",
        JobState::Canceled => "canceled",
    }
}

fn job_state_from_str(state: &str) -> JobState {
    match state {
        "pending" => JobState::Pending,
        "running" => JobState::Running,
        "paused" => JobState::Paused,
        "succeeded" | "completed" => JobState::Completed,
        "failed" => JobState::Failed,
        "canceled" => JobState::Canceled,
        _ => JobState::Pending,
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[derive(Default)]
pub struct InMemoryJobStore {
    inner: Mutex<InMemoryJobState>,
}

#[derive(Default)]
struct InMemoryJobState {
    jobs: HashMap<String, ImportJobRecord>,
    rows: HashMap<String, Vec<ImportJobRowRecord>>,
}

impl InMemoryJobStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ImportJobStore for InMemoryJobStore {
    fn insert_job(&self, job: NewImportJob) -> Result<ImportJobRecord, String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        if guard.jobs.contains_key(&job.id) {
            return Err("job already exists".into());
        }
        let record = ImportJobRecord {
            id: job.id.clone(),
            token_id: job.token_id.clone(),
            database_id: job.database_id.clone(),
            source_file_path: job.source_file_path.clone(),
            state: JobState::Pending,
            progress: JobProgress {
                total: job.total,
                done: 0,
                failed: 0,
                skipped: 0,
            },
            config_snapshot_json: job.config_snapshot_json.clone(),
            started_at: None,
            ended_at: None,
            next_offset: 0,
            rps: None,
            last_error: None,
            last_heartbeat: Some(job.created_at),
        };
        guard.jobs.insert(job.id.clone(), record.clone());
        Ok(record)
    }

    fn update_progress(&self, job_id: &str, update: ProgressUpdate) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let job = guard
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| "job not found".to_string())?;
        if let Some(total) = update.total {
            job.progress.total = Some(total);
        }
        job.progress.done += update.done;
        job.progress.failed += update.failed;
        job.progress.skipped += update.skipped;
        if let Some(offset) = update.next_offset {
            job.next_offset = offset;
        }
        if let Some(rps) = update.rps {
            job.rps = Some(rps);
        }
        if let Some(err) = update.last_error {
            job.last_error = Some(err);
        }
        job.last_heartbeat = update.heartbeat_at.or_else(|| Some(now_ms()));
        Ok(())
    }

    fn mark_state(&self, job_id: &str, transition: StateTransition) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let job = guard
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| "job not found".to_string())?;
        job.state = transition.state;
        if let Some(started) = transition.started_at {
            job.started_at = Some(started);
        }
        if let Some(ended) = transition.ended_at {
            job.ended_at = Some(ended);
        }
        if let Some(err) = transition.last_error {
            job.last_error = Some(err);
        }
        job.last_heartbeat = Some(now_ms());
        Ok(())
    }

    fn append_row_results(&self, rows: Vec<ImportJobRowRecord>) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        for row in rows {
            guard.rows.entry(row.job_id.clone()).or_default().push(row);
        }
        Ok(())
    }

    fn load_job(&self, job_id: &str) -> Result<Option<ImportJobRecord>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        Ok(guard.jobs.get(job_id).cloned())
    }

    fn list_pending_jobs(&self) -> Result<Vec<ImportJobRecord>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        Ok(guard
            .jobs
            .values()
            .filter(|job| matches!(job.state, JobState::Pending | JobState::Running))
            .cloned()
            .collect())
    }

    fn list_recent_failures(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportJobRowRecord>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let mut rows = guard.rows.get(job_id).cloned().unwrap_or_default();
        rows.retain(|row| matches!(row.status, ImportJobRowStatus::Failed));
        if rows.len() > limit {
            rows.sort_by(|a, b| b.row_index.cmp(&a.row_index));
            rows.truncate(limit);
            rows.reverse();
        }
        Ok(rows)
    }
}

#[cfg(feature = "notion-sqlite")]
pub struct SqliteJobStore {
    db_path: PathBuf,
    caps: JobTableCapabilities,
}

#[cfg(feature = "notion-sqlite")]
#[derive(Default, Debug, Clone, Copy)]
struct JobTableCapabilities {
    has_next_offset: bool,
    has_rps: bool,
    has_last_error: bool,
    has_last_heartbeat: bool,
    has_error_payload_json: bool,
}

#[cfg(feature = "notion-sqlite")]
impl SqliteJobStore {
    pub fn new(db_path: PathBuf) -> Self {
        let caps = detect_caps(&db_path).unwrap_or_default();
        Self { db_path, caps }
    }
}

#[cfg(feature = "notion-sqlite")]
fn detect_caps(path: &PathBuf) -> Result<JobTableCapabilities, String> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut caps = JobTableCapabilities::default();
    let mut stmt = conn
        .prepare("PRAGMA table_info(notion_import_jobs)")
        .map_err(|e| e.to_string())?;
    let column_names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    caps.has_next_offset = column_names.iter().any(|c| c == "next_offset");
    caps.has_rps = column_names.iter().any(|c| c == "rps");
    caps.has_last_error = column_names.iter().any(|c| c == "last_error");
    caps.has_last_heartbeat = column_names.iter().any(|c| c == "last_heartbeat");

    let mut row_stmt = conn
        .prepare("PRAGMA table_info(notion_import_job_rows)")
        .map_err(|e| e.to_string())?;
    let row_columns: Vec<String> = row_stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    caps.has_error_payload_json = row_columns.iter().any(|c| c == "error_payload_json");
    Ok(caps)
}

#[cfg(feature = "notion-sqlite")]
impl ImportJobStore for SqliteJobStore {
    fn insert_job(&self, job: NewImportJob) -> Result<ImportJobRecord, String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO notion_import_jobs (
                id, token_id, database_id, source_file_path, status, total, done, failed, skipped,
                started_at, ended_at, config_snapshot_json
            ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, 0, 0, 0, NULL, NULL, ?6)",
            params![
                job.id,
                job.token_id,
                job.database_id,
                job.source_file_path,
                job.total.map(|v| v as i64),
                job.config_snapshot_json
            ],
        )
        .map_err(|e| e.to_string())?;

        if self.caps.has_next_offset {
            conn.execute(
                "UPDATE notion_import_jobs SET next_offset = 0 WHERE id = ?1",
                [job.id.as_str()],
            )
            .map_err(|e| e.to_string())?;
        }
        if self.caps.has_last_heartbeat {
            conn.execute(
                "UPDATE notion_import_jobs SET last_heartbeat = ?2 WHERE id = ?1",
                params![job.id.as_str(), job.created_at],
            )
            .map_err(|e| e.to_string())?;
        }

        self.load_job(&job.id)?
            .ok_or_else(|| "job insert failed".into())
    }

    fn update_progress(&self, job_id: &str, update: ProgressUpdate) -> Result<(), String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut sql = String::from(
            "UPDATE notion_import_jobs SET done = done + ?2, failed = failed + ?3, skipped = skipped + ?4",
        );
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::from(job_id.to_string()),
            rusqlite::types::Value::from(update.done as i64),
            rusqlite::types::Value::from(update.failed as i64),
            rusqlite::types::Value::from(update.skipped as i64),
        ];
        if let Some(total) = update.total {
            sql.push_str(", total = ?5");
            params.push(rusqlite::types::Value::from(total as i64));
        }
        let mut index = 5 + if update.total.is_some() { 1 } else { 0 };
        if self.caps.has_next_offset {
            if let Some(offset) = update.next_offset {
                sql.push_str(&format!(", next_offset = ?{}", index));
                params.push(rusqlite::types::Value::from(offset as i64));
                index += 1;
            }
        }
        if self.caps.has_rps {
            if let Some(rps) = update.rps {
                sql.push_str(&format!(", rps = ?{}", index));
                params.push(rusqlite::types::Value::from(rps));
                index += 1;
            }
        }
        if self.caps.has_last_error {
            if let Some(err) = update.last_error {
                sql.push_str(&format!(", last_error = ?{}", index));
                params.push(rusqlite::types::Value::from(err));
                index += 1;
            }
        }
        if self.caps.has_last_heartbeat {
            let hb = update.heartbeat_at.unwrap_or_else(now_ms);
            sql.push_str(&format!(", last_heartbeat = ?{}", index));
            params.push(rusqlite::types::Value::from(hb));
        }
        sql.push_str(" WHERE id = ?1");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        stmt.execute(rusqlite::params_from_iter(params.into_iter()))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn mark_state(&self, job_id: &str, transition: StateTransition) -> Result<(), String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut sql = String::from("UPDATE notion_import_jobs SET status = ?2");
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::from(job_id.to_string()),
            rusqlite::types::Value::from(job_state_to_str(transition.state).to_string()),
        ];
        let mut index = 3;
        if let Some(started) = transition.started_at {
            sql.push_str(&format!(", started_at = ?{}", index));
            params.push(rusqlite::types::Value::from(started));
            index += 1;
        }
        if let Some(ended) = transition.ended_at {
            sql.push_str(&format!(", ended_at = ?{}", index));
            params.push(rusqlite::types::Value::from(ended));
            index += 1;
        }
        if self.caps.has_last_error {
            if let Some(err) = transition.last_error {
                sql.push_str(&format!(", last_error = ?{}", index));
                params.push(rusqlite::types::Value::from(err));
                index += 1;
            }
        }
        if self.caps.has_last_heartbeat {
            sql.push_str(&format!(", last_heartbeat = ?{}", index));
            params.push(rusqlite::types::Value::from(now_ms()));
        }
        sql.push_str(" WHERE id = ?1");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        stmt.execute(rusqlite::params_from_iter(params.into_iter()))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn append_row_results(&self, rows: Vec<ImportJobRowRecord>) -> Result<(), String> {
        use rusqlite::{params, Connection, TransactionBehavior};
        if rows.is_empty() {
            return Ok(());
        }
        let mut conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for row in rows.iter() {
            tx.execute(
                "INSERT OR REPLACE INTO notion_import_job_rows (
                    job_id, row_index, status, error_code, error_message, error_payload_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.job_id,
                    row.row_index as i64,
                    row.status.as_str(),
                    row.error_code,
                    row.error_message,
                    if self.caps.has_error_payload_json {
                        row.error_payload_json.clone()
                    } else {
                        None
                    }
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    fn load_job(&self, job_id: &str) -> Result<Option<ImportJobRecord>, String> {
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut columns = String::from(
            "id, token_id, database_id, source_file_path, status, total, done, failed, skipped, started_at, ended_at, config_snapshot_json",
        );
        if self.caps.has_next_offset {
            columns.push_str(", next_offset");
        }
        if self.caps.has_rps {
            columns.push_str(", rps");
        }
        if self.caps.has_last_error {
            columns.push_str(", last_error");
        }
        if self.caps.has_last_heartbeat {
            columns.push_str(", last_heartbeat");
        }
        let sql = format!("SELECT {} FROM notion_import_jobs WHERE id = ?1", columns);
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let result = stmt
            .query_row(
                params![job_id],
                |row| -> rusqlite::Result<ImportJobRecord> {
                    let mut col_index = 0;
                    let id: String = row.get(col_index)?;
                    col_index += 1;
                    let token_id: String = row.get(col_index)?;
                    col_index += 1;
                    let database_id: String = row.get(col_index)?;
                    col_index += 1;
                    let source_file_path: String = row.get(col_index)?;
                    col_index += 1;
                    let status: String = row.get(col_index)?;
                    col_index += 1;
                    let total: Option<i64> = row.get(col_index)?;
                    col_index += 1;
                    let done: i64 = row.get(col_index)?;
                    col_index += 1;
                    let failed: i64 = row.get(col_index)?;
                    col_index += 1;
                    let skipped: i64 = row.get(col_index)?;
                    col_index += 1;
                    let started_at: Option<i64> = row.get(col_index)?;
                    col_index += 1;
                    let ended_at: Option<i64> = row.get(col_index)?;
                    col_index += 1;
                    let config_snapshot_json: String = row.get(col_index)?;
                    col_index += 1;
                    let next_offset = if self.caps.has_next_offset {
                        let offset: i64 = row.get(col_index)?;
                        col_index += 1;
                        offset.max(0) as usize
                    } else {
                        0
                    };
                    let rps = if self.caps.has_rps {
                        let rps_val: Option<f64> = row.get(col_index)?;
                        col_index += 1;
                        rps_val
                    } else {
                        None
                    };
                    let last_error = if self.caps.has_last_error {
                        let val: Option<String> = row.get(col_index)?;
                        col_index += 1;
                        val
                    } else {
                        None
                    };
                    let last_heartbeat = if self.caps.has_last_heartbeat {
                        let val: Option<i64> = row.get(col_index)?;
                        val
                    } else {
                        None
                    };

                    Ok(ImportJobRecord {
                        id,
                        token_id,
                        database_id,
                        source_file_path,
                        state: job_state_from_str(&status),
                        progress: JobProgress {
                            total: total.map(|v| v as usize),
                            done: done.max(0) as usize,
                            failed: failed.max(0) as usize,
                            skipped: skipped.max(0) as usize,
                        },
                        config_snapshot_json,
                        started_at,
                        ended_at,
                        next_offset,
                        rps,
                        last_error,
                        last_heartbeat,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(result)
    }

    fn list_pending_jobs(&self) -> Result<Vec<ImportJobRecord>, String> {
        use rusqlite::Connection;
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut columns = String::from(
            "id, token_id, database_id, source_file_path, status, total, done, failed, skipped, started_at, ended_at, config_snapshot_json",
        );
        if self.caps.has_next_offset {
            columns.push_str(", next_offset");
        }
        if self.caps.has_rps {
            columns.push_str(", rps");
        }
        if self.caps.has_last_error {
            columns.push_str(", last_error");
        }
        if self.caps.has_last_heartbeat {
            columns.push_str(", last_heartbeat");
        }
        let sql = format!(
            "SELECT {} FROM notion_import_jobs WHERE status IN ('pending','running','paused')",
            columns
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| -> rusqlite::Result<ImportJobRecord> {
                let mut col_index = 0;
                let id: String = row.get(col_index)?;
                col_index += 1;
                let token_id: String = row.get(col_index)?;
                col_index += 1;
                let database_id: String = row.get(col_index)?;
                col_index += 1;
                let source_file_path: String = row.get(col_index)?;
                col_index += 1;
                let status: String = row.get(col_index)?;
                col_index += 1;
                let total: Option<i64> = row.get(col_index)?;
                col_index += 1;
                let done: i64 = row.get(col_index)?;
                col_index += 1;
                let failed: i64 = row.get(col_index)?;
                col_index += 1;
                let skipped: i64 = row.get(col_index)?;
                col_index += 1;
                let started_at: Option<i64> = row.get(col_index)?;
                col_index += 1;
                let ended_at: Option<i64> = row.get(col_index)?;
                col_index += 1;
                let config_snapshot_json: String = row.get(col_index)?;
                col_index += 1;
                let next_offset = if self.caps.has_next_offset {
                    let offset: i64 = row.get(col_index)?;
                    col_index += 1;
                    offset.max(0) as usize
                } else {
                    0
                };
                let rps = if self.caps.has_rps {
                    let rps_val: Option<f64> = row.get(col_index)?;
                    col_index += 1;
                    rps_val
                } else {
                    None
                };
                let last_error = if self.caps.has_last_error {
                    let val: Option<String> = row.get(col_index)?;
                    col_index += 1;
                    val
                } else {
                    None
                };
                let last_heartbeat = if self.caps.has_last_heartbeat {
                    let val: Option<i64> = row.get(col_index)?;
                    val
                } else {
                    None
                };
                Ok(ImportJobRecord {
                    id,
                    token_id,
                    database_id,
                    source_file_path,
                    state: job_state_from_str(&status),
                    progress: JobProgress {
                        total: total.map(|v| v as usize),
                        done: done.max(0) as usize,
                        failed: failed.max(0) as usize,
                        skipped: skipped.max(0) as usize,
                    },
                    config_snapshot_json,
                    started_at,
                    ended_at,
                    next_offset,
                    rps,
                    last_error,
                    last_heartbeat,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for job in rows {
            out.push(job.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    fn list_recent_failures(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportJobRowRecord>, String> {
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut columns = String::from("job_id, row_index, status, error_code, error_message");
        if self.caps.has_error_payload_json {
            columns.push_str(", error_payload_json");
        }
        let sql = format!(
            "SELECT {} FROM notion_import_job_rows WHERE job_id = ?1 AND status = 'failed' ORDER BY row_index DESC LIMIT ?2",
            columns
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![job_id, limit as i64],
                |row| -> rusqlite::Result<ImportJobRowRecord> {
                    let mut col_index = 0;
                    let job_id: String = row.get(col_index)?;
                    col_index += 1;
                    let row_index: i64 = row.get(col_index)?;
                    col_index += 1;
                    let status: String = row.get(col_index)?;
                    col_index += 1;
                    let error_code: Option<String> = row.get(col_index)?;
                    col_index += 1;
                    let error_message: Option<String> = row.get(col_index)?;
                    col_index += 1;
                    let error_payload_json = if self.caps.has_error_payload_json {
                        let payload: Option<String> = row.get(col_index)?;
                        payload
                    } else {
                        None
                    };
                    Ok(ImportJobRowRecord {
                        job_id,
                        row_index: row_index.max(0) as usize,
                        status: ImportJobRowStatus::from_str(&status)
                            .unwrap_or(ImportJobRowStatus::Failed),
                        error_code,
                        error_message,
                        error_payload_json,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        let mut out: Vec<ImportJobRowRecord> = rows
            .map(|row| row.map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        out.reverse();
        Ok(out)
    }
}
