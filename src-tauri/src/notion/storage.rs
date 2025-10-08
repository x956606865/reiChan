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

    fn insert_demo_job(
        store: &InMemoryJobStore,
        id: &str,
        state: JobState,
        created_at: i64,
        ended_at: Option<i64>,
    ) {
        let new_job = NewImportJob {
            id: id.to_string(),
            token_id: "tok-1".into(),
            database_id: "db-1".into(),
            source_file_path: format!("/tmp/{id}.json"),
            config_snapshot_json: "{\"version\":1}".into(),
            total: Some(10),
            created_at,
            priority: 0,
            lease_expires_at: None,
            conflict_total: Some(0),
        };
        let record = store.insert_job(new_job).expect("insert job");
        if state != JobState::Pending {
            store
                .mark_state(
                    &record.id,
                    StateTransition {
                        state,
                        started_at: Some(created_at + 10),
                        ended_at,
                        last_error: None,
                    },
                )
                .expect("mark state");
        }
    }

    #[test]
    fn in_memory_history_lists_terminal_states_desc() {
        let store = InMemoryJobStore::new();
        let base = 1_700_000_000_000i64;
        insert_demo_job(
            &store,
            "job-completed",
            JobState::Completed,
            base,
            Some(base + 1_000),
        );
        insert_demo_job(
            &store,
            "job-failed",
            JobState::Failed,
            base + 1_000,
            Some(base + 2_000),
        );
        insert_demo_job(
            &store,
            "job-canceled",
            JobState::Canceled,
            base + 2_000,
            Some(base + 3_000),
        );
        insert_demo_job(
            &store,
            "job-running",
            JobState::Running,
            base + 3_000,
            None,
        );

        let items = store
            .list_history(0, 10, None)
            .expect("history listing should work");
        let ids: Vec<_> = items.iter().map(|job| job.id.as_str()).collect();
        assert_eq!(ids, ["job-canceled", "job-failed", "job-completed"]);

        let total = store.count_history(None).expect("count history");
        assert_eq!(total, 3);
    }

    #[test]
    fn in_memory_history_respects_filters_and_pagination() {
        let store = InMemoryJobStore::new();
        let base = 1_700_100_000_000i64;
        let mut offset_idx: i64 = 0;
        for (id, state) in [
            ("job-a", JobState::Completed),
            ("job-b", JobState::Failed),
            ("job-c", JobState::Canceled),
            ("job-d", JobState::Completed),
        ] {
            insert_demo_job(
                &store,
                id,
                state,
                base + (offset_idx * 1_000),
                Some(base + (offset_idx * 1_000) + 500),
            );
            offset_idx += 1;
        }

        let page_one = store
            .list_history(0, 2, None)
            .expect("page one should load");
        assert_eq!(page_one.len(), 2);
        assert_eq!(page_one[0].id, "job-d");
        assert_eq!(page_one[1].id, "job-c");

        let page_two = store
            .list_history(2, 2, None)
            .expect("page two should load");
        assert_eq!(page_two.len(), 2);
        assert_eq!(page_two[0].id, "job-b");
        assert_eq!(page_two[1].id, "job-a");

        let failed_only = store
            .list_history(0, 10, Some(&[JobState::Failed]))
            .expect("filter failed");
        assert_eq!(failed_only.len(), 1);
        assert_eq!(failed_only[0].id, "job-b");

        let total = store.count_history(Some(&[JobState::Completed, JobState::Failed]));
        assert_eq!(total.expect("count filtered"), 3);
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    pub created_at: i64,
    pub state: JobState,
    pub progress: JobProgress,
    pub config_snapshot_json: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub next_offset: usize,
    pub rps: Option<f64>,
    pub last_error: Option<String>,
    pub last_heartbeat: Option<i64>,
    pub priority: i32,
    pub lease_expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ImportJobRowRecord {
    pub job_id: String,
    pub row_index: usize,
    pub status: ImportJobRowStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub error_payload_json: Option<String>,
    pub conflict_type: Option<String>,
    pub previous_snapshot_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImportCheckpoint {
    pub job_id: String,
    pub row_index: usize,
    pub file_offset: u64,
    pub data_hash: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    pub job_id: String,
    pub row_index: usize,
    pub file_offset: u64,
    pub data_hash: String,
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
    pub priority: i32,
    pub lease_expires_at: Option<i64>,
    pub conflict_total: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressUpdate {
    pub total: Option<usize>,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub conflict_total: Option<usize>,
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
    fn touch_lease(&self, job_id: &str, lease_expires_at: Option<i64>) -> Result<(), String>;
    fn set_priority(&self, job_id: &str, priority: i32) -> Result<(), String>;
    fn append_row_results(&self, rows: Vec<ImportJobRowRecord>) -> Result<(), String>;
    fn load_job(&self, job_id: &str) -> Result<Option<ImportJobRecord>, String>;
    fn list_pending_jobs(&self) -> Result<Vec<ImportJobRecord>, String>;
    fn list_recent_failures(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportJobRowRecord>, String>;
    fn list_failed_rows(&self, job_id: &str) -> Result<Vec<ImportJobRowRecord>, String>;
    fn write_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), String>;
    fn recent_checkpoints(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportCheckpoint>, String>;
    fn clear_checkpoints(&self, job_id: &str) -> Result<(), String>;
    fn list_history(
        &self,
        offset: usize,
        limit: usize,
        states: Option<&[JobState]>,
    ) -> Result<Vec<ImportJobRecord>, String>;
    fn count_history(&self, states: Option<&[JobState]>) -> Result<usize, String>;
}

fn job_state_to_str(state: JobState) -> &'static str {
    match state {
        JobState::Pending => "pending",
        JobState::Queued => "queued",
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
        "queued" => JobState::Queued,
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

fn extract_timestamp_from_id(job_id: &str) -> i64 {
    job_id
        .rsplit('-')
        .next()
        .and_then(|fragment| fragment.parse::<i64>().ok())
        .unwrap_or(0)
}

fn history_states_vec(states: Option<&[JobState]>) -> Vec<JobState> {
    match states {
        Some(list) if !list.is_empty() => list.to_vec(),
        _ => vec![JobState::Completed, JobState::Failed, JobState::Canceled],
    }
}

fn resolve_history_timestamp(record: &ImportJobRecord) -> i64 {
    if let Some(ended) = record.ended_at {
        return ended;
    }
    if let Some(started) = record.started_at {
        return started;
    }
    if record.created_at != 0 {
        return record.created_at;
    }
    extract_timestamp_from_id(&record.id)
}

#[derive(Default)]
pub struct InMemoryJobStore {
    inner: Mutex<InMemoryJobState>,
}

#[derive(Default)]
struct InMemoryJobState {
    jobs: HashMap<String, ImportJobRecord>,
    rows: HashMap<String, Vec<ImportJobRowRecord>>,
    checkpoints: HashMap<String, Vec<ImportCheckpoint>>,
}

impl InMemoryJobStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn collect_history_jobs(
        &self,
        states: Option<&[JobState]>,
    ) -> Result<Vec<ImportJobRecord>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let allowed = history_states_vec(states);
        let mut jobs: Vec<ImportJobRecord> = guard
            .jobs
            .values()
            .filter(|job| allowed.iter().any(|state| state == &job.state))
            .cloned()
            .collect();
        drop(guard);
        jobs.sort_by(|a, b| {
            let ta = resolve_history_timestamp(a);
            let tb = resolve_history_timestamp(b);
            tb.cmp(&ta)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(jobs)
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
            created_at: job.created_at,
            state: JobState::Pending,
            progress: JobProgress {
                total: job.total,
                done: 0,
                failed: 0,
                skipped: 0,
                conflict_total: job.conflict_total,
            },
            config_snapshot_json: job.config_snapshot_json.clone(),
            started_at: None,
            ended_at: None,
            next_offset: 0,
            rps: None,
            last_error: None,
            last_heartbeat: Some(job.created_at),
            priority: job.priority,
            lease_expires_at: job.lease_expires_at,
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
        if update.conflicts > 0 {
            let entry = job.progress.conflict_total.get_or_insert(0);
            *entry += update.conflicts;
        }
        if let Some(conflict_total) = update.conflict_total {
            job.progress.conflict_total = Some(conflict_total);
        }
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

    fn touch_lease(&self, job_id: &str, lease_expires_at: Option<i64>) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let job = guard
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| "job not found".to_string())?;
        job.lease_expires_at = lease_expires_at;
        Ok(())
    }

    fn set_priority(&self, job_id: &str, priority: i32) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let job = guard
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| "job not found".to_string())?;
        job.priority = priority;
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
            .filter(|job| {
                matches!(
                    job.state,
                    JobState::Pending | JobState::Queued | JobState::Running
                )
            })
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

    fn list_failed_rows(&self, job_id: &str) -> Result<Vec<ImportJobRowRecord>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let mut rows = guard.rows.get(job_id).cloned().unwrap_or_default();
        rows.retain(|row| matches!(row.status, ImportJobRowStatus::Failed));
        rows.sort_by(|a, b| a.row_index.cmp(&b.row_index));
        Ok(rows)
    }

    fn write_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let slots = guard
            .checkpoints
            .entry(checkpoint.job_id.clone())
            .or_default();
        let entry = ImportCheckpoint {
            job_id: checkpoint.job_id,
            row_index: checkpoint.row_index,
            file_offset: checkpoint.file_offset,
            data_hash: checkpoint.data_hash,
            updated_at: now_ms(),
        };
        slots.push(entry);
        slots.sort_by(|a, b| b.row_index.cmp(&a.row_index));
        if slots.len() > 10 {
            slots.truncate(10);
        }
        Ok(())
    }

    fn recent_checkpoints(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportCheckpoint>, String> {
        let guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        let entries = guard
            .checkpoints
            .get(job_id)
            .cloned()
            .unwrap_or_default();
        if entries.len() > limit {
            Ok(entries.into_iter().take(limit).collect())
        } else {
            Ok(entries)
        }
    }

    fn clear_checkpoints(&self, job_id: &str) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        guard.checkpoints.remove(job_id);
        Ok(())
    }

    fn list_history(
        &self,
        offset: usize,
        limit: usize,
        states: Option<&[JobState]>,
    ) -> Result<Vec<ImportJobRecord>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let jobs = self.collect_history_jobs(states)?;
        if offset >= jobs.len() {
            return Ok(Vec::new());
        }
        let end = std::cmp::min(offset.saturating_add(limit), jobs.len());
        Ok(jobs[offset..end].to_vec())
    }

    fn count_history(&self, states: Option<&[JobState]>) -> Result<usize, String> {
        let jobs = self.collect_history_jobs(states)?;
        Ok(jobs.len())
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
    has_priority: bool,
    has_lease_expires_at: bool,
    has_conflict_total: bool,
    has_created_at: bool,
    has_error_payload_json: bool,
    has_conflict_type: bool,
    has_previous_snapshot_json: bool,
    has_checkpoints_table: bool,
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
    caps.has_priority = column_names.iter().any(|c| c == "priority");
    caps.has_lease_expires_at = column_names.iter().any(|c| c == "lease_expires_at");
    caps.has_conflict_total = column_names.iter().any(|c| c == "conflict_total");
    caps.has_created_at = column_names.iter().any(|c| c == "created_at");

    let mut row_stmt = conn
        .prepare("PRAGMA table_info(notion_import_job_rows)")
        .map_err(|e| e.to_string())?;
    let row_columns: Vec<String> = row_stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    caps.has_error_payload_json = row_columns.iter().any(|c| c == "error_payload_json");
    caps.has_conflict_type = row_columns.iter().any(|c| c == "conflict_type");
    caps.has_previous_snapshot_json = row_columns.iter().any(|c| c == "previous_snapshot_json");

    let mut cp_stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'notion_import_checkpoints'",
        )
        .map_err(|e| e.to_string())?;
    caps.has_checkpoints_table = cp_stmt.exists([]).map_err(|e| e.to_string())?;
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
        if self.caps.has_created_at {
            conn.execute(
                "UPDATE notion_import_jobs SET created_at = ?2 WHERE id = ?1",
                params![job.id.as_str(), job.created_at],
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
        if self.caps.has_priority {
            conn.execute(
                "UPDATE notion_import_jobs SET priority = ?2 WHERE id = ?1",
                params![job.id.as_str(), job.priority],
            )
            .map_err(|e| e.to_string())?;
        }
        if self.caps.has_lease_expires_at {
            conn.execute(
                "UPDATE notion_import_jobs SET lease_expires_at = ?2 WHERE id = ?1",
                params![job.id.as_str(), job.lease_expires_at],
            )
            .map_err(|e| e.to_string())?;
        }
        if self.caps.has_conflict_total {
            conn.execute(
                "UPDATE notion_import_jobs SET conflict_total = ?2 WHERE id = ?1",
                params![job.id.as_str(), job.conflict_total.unwrap_or(0) as i64],
            )
            .map_err(|e| e.to_string())?;
        }

        self.load_job(&job.id)?
            .ok_or_else(|| "job insert failed".into())
    }

    fn update_progress(&self, job_id: &str, update: ProgressUpdate) -> Result<(), String> {
        use rusqlite::{params_from_iter, types::Value, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut sql = String::from(
            "UPDATE notion_import_jobs SET done = done + ?2, failed = failed + ?3, skipped = skipped + ?4",
        );
        let mut params: Vec<Value> = vec![
            Value::from(job_id.to_string()),
            Value::from(update.done as i64),
            Value::from(update.failed as i64),
            Value::from(update.skipped as i64),
        ];

        let mut append_value = |fragment: &str, value: Value| {
            let index = params.len() + 1;
            let clause = fragment.replace("{}", &index.to_string());
            sql.push_str(&clause);
            params.push(value);
        };

        if let Some(total) = update.total {
            append_value(", total = ?{}", Value::from(total as i64));
        }

        if self.caps.has_conflict_total {
            if let Some(total) = update.conflict_total {
                append_value(", conflict_total = ?{}", Value::from(total as i64));
            } else if update.conflicts > 0 {
                append_value(
                    ", conflict_total = COALESCE(conflict_total, 0) + ?{}",
                    Value::from(update.conflicts as i64),
                );
            }
        }

        if self.caps.has_next_offset {
            if let Some(offset) = update.next_offset {
                append_value(", next_offset = ?{}", Value::from(offset as i64));
            }
        }

        if self.caps.has_rps {
            if let Some(rps) = update.rps {
                append_value(", rps = ?{}", Value::from(rps));
            }
        }

        if self.caps.has_last_error {
            if let Some(err) = update.last_error {
                append_value(", last_error = ?{}", Value::from(err));
            }
        }

        if self.caps.has_last_heartbeat {
            let hb = update.heartbeat_at.unwrap_or_else(now_ms);
            append_value(", last_heartbeat = ?{}", Value::from(hb));
        }

        sql.push_str(" WHERE id = ?1");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        stmt.execute(params_from_iter(params.into_iter()))
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

    fn touch_lease(&self, job_id: &str, lease_expires_at: Option<i64>) -> Result<(), String> {
        use rusqlite::Connection;
        if !self.caps.has_lease_expires_at {
            return Ok(());
        }
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE notion_import_jobs SET lease_expires_at = ?2 WHERE id = ?1",
            params![job_id, lease_expires_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn set_priority(&self, job_id: &str, priority: i32) -> Result<(), String> {
        use rusqlite::Connection;
        if !self.caps.has_priority {
            return Ok(());
        }
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE notion_import_jobs SET priority = ?2 WHERE id = ?1",
            (job_id, priority),
        )
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
            if self.caps.has_error_payload_json {
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
                        row.error_payload_json.clone(),
                    ],
                )
                .map_err(|e| e.to_string())?;
            } else {
                tx.execute(
                    "INSERT OR REPLACE INTO notion_import_job_rows (
                        job_id, row_index, status, error_code, error_message
                    ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        row.job_id,
                        row.row_index as i64,
                        row.status.as_str(),
                        row.error_code,
                        row.error_message,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }

            if self.caps.has_conflict_type {
                tx.execute(
                    "UPDATE notion_import_job_rows SET conflict_type = ?3 WHERE job_id = ?1 AND row_index = ?2",
                    params![row.job_id, row.row_index as i64, row.conflict_type.clone()],
                )
                .map_err(|e| e.to_string())?;
            }
            if self.caps.has_previous_snapshot_json {
                tx.execute(
                    "UPDATE notion_import_job_rows SET previous_snapshot_json = ?3 WHERE job_id = ?1 AND row_index = ?2",
                    params![
                        row.job_id,
                        row.row_index as i64,
                        row.previous_snapshot_json.clone()
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
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
        if self.caps.has_created_at {
            columns.push_str(", created_at");
        }
        if self.caps.has_created_at {
            columns.push_str(", created_at");
        }
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
        if self.caps.has_priority {
            columns.push_str(", priority");
        }
        if self.caps.has_lease_expires_at {
            columns.push_str(", lease_expires_at");
        }
        if self.caps.has_conflict_total {
            columns.push_str(", conflict_total");
        }
        if self.caps.has_priority {
            columns.push_str(", priority");
        }
        if self.caps.has_lease_expires_at {
            columns.push_str(", lease_expires_at");
        }
        if self.caps.has_conflict_total {
            columns.push_str(", conflict_total");
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
                    let created_at = if self.caps.has_created_at {
                        let val: i64 = row.get(col_index)?;
                        col_index += 1;
                        val
                    } else {
                        extract_timestamp_from_id(&id)
                    };
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
                        col_index += 1;
                        val
                    } else {
                        None
                    };
                    let priority = if self.caps.has_priority {
                        let val: i64 = row.get(col_index)?;
                        col_index += 1;
                        val as i32
                    } else {
                        0
                    };
                    let lease_expires_at = if self.caps.has_lease_expires_at {
                        let val: Option<i64> = row.get(col_index)?;
                        col_index += 1;
                        val
                    } else {
                        None
                    };
                    let conflict_total = if self.caps.has_conflict_total {
                        let val: Option<i64> = row.get(col_index)?;
                        col_index += 1;
                        val.map(|v| v.max(0) as usize)
                    } else {
                        None
                    };

                    Ok(ImportJobRecord {
                        id,
                        token_id,
                        database_id,
                        source_file_path,
                        created_at,
                        state: job_state_from_str(&status),
                        progress: JobProgress {
                            total: total.map(|v| v as usize),
                            done: done.max(0) as usize,
                            failed: failed.max(0) as usize,
                            skipped: skipped.max(0) as usize,
                            conflict_total,
                        },
                        config_snapshot_json,
                        started_at,
                        ended_at,
                        next_offset,
                        rps,
                        last_error,
                        last_heartbeat,
                        priority,
                        lease_expires_at,
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
            "SELECT {} FROM notion_import_jobs WHERE status IN ('pending','queued','running','paused')",
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
                let created_at = if self.caps.has_created_at {
                    let val: i64 = row.get(col_index)?;
                    col_index += 1;
                    val
                } else {
                    extract_timestamp_from_id(&id)
                };
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
                    col_index += 1;
                    val
                } else {
                    None
                };
                let priority = if self.caps.has_priority {
                    let val: i64 = row.get(col_index)?;
                    col_index += 1;
                    val as i32
                } else {
                    0
                };
                let lease_expires_at = if self.caps.has_lease_expires_at {
                    let val: Option<i64> = row.get(col_index)?;
                    col_index += 1;
                    val
                } else {
                    None
                };
                let conflict_total = if self.caps.has_conflict_total {
                    let val: Option<i64> = row.get(col_index)?;
                    col_index += 1;
                    val.map(|v| v.max(0) as usize)
                } else {
                    None
                };
                Ok(ImportJobRecord {
                    id,
                    token_id,
                    database_id,
                    source_file_path,
                    created_at,
                    state: job_state_from_str(&status),
                    progress: JobProgress {
                        total: total.map(|v| v as usize),
                        done: done.max(0) as usize,
                        failed: failed.max(0) as usize,
                        skipped: skipped.max(0) as usize,
                        conflict_total,
                    },
                    config_snapshot_json,
                    started_at,
                    ended_at,
                    next_offset,
                    rps,
                    last_error,
                    last_heartbeat,
                    priority,
                    lease_expires_at,
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
        if self.caps.has_conflict_type {
            columns.push_str(", conflict_type");
        }
        if self.caps.has_previous_snapshot_json {
            columns.push_str(", previous_snapshot_json");
        }
        if self.caps.has_conflict_type {
            columns.push_str(", conflict_type");
        }
        if self.caps.has_previous_snapshot_json {
            columns.push_str(", previous_snapshot_json");
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
                        col_index += 1;
                        payload
                    } else {
                        None
                    };
                    let conflict_type = if self.caps.has_conflict_type {
                        let value: Option<String> = row.get(col_index)?;
                        col_index += 1;
                        value
                    } else {
                        None
                    };
                    let previous_snapshot_json = if self.caps.has_previous_snapshot_json {
                        let value: Option<String> = row.get(col_index)?;
                        col_index += 1;
                        value
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
                        conflict_type,
                        previous_snapshot_json,
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

    fn list_failed_rows(&self, job_id: &str) -> Result<Vec<ImportJobRowRecord>, String> {
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut columns = String::from("job_id, row_index, status, error_code, error_message");
        if self.caps.has_error_payload_json {
            columns.push_str(", error_payload_json");
        }
        let sql = format!(
            "SELECT {} FROM notion_import_job_rows WHERE job_id = ?1 AND status = 'failed' ORDER BY row_index",
            columns
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![job_id],
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
                        col_index += 1;
                        payload
                    } else {
                        None
                    };
                    let conflict_type = if self.caps.has_conflict_type {
                        let value: Option<String> = row.get(col_index)?;
                        col_index += 1;
                        value
                    } else {
                        None
                    };
                    let previous_snapshot_json = if self.caps.has_previous_snapshot_json {
                        let value: Option<String> = row.get(col_index)?;
                        col_index += 1;
                        value
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
                        conflict_type,
                        previous_snapshot_json,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    fn write_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), String> {
        if !self.caps.has_checkpoints_table {
            return Ok(());
        }
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO notion_import_checkpoints (job_id, row_index, file_offset, data_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(job_id, row_index)
             DO UPDATE SET file_offset = excluded.file_offset, data_hash = excluded.data_hash, updated_at = excluded.updated_at",
            params![
                checkpoint.job_id,
                checkpoint.row_index as i64,
                checkpoint.file_offset as i64,
                checkpoint.data_hash,
                now_ms(),
            ],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM notion_import_checkpoints
             WHERE job_id = ?1
               AND row_index NOT IN (
                 SELECT row_index FROM notion_import_checkpoints
                 WHERE job_id = ?1
                 ORDER BY row_index DESC
                 LIMIT 10
               )",
            params![checkpoint.job_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn recent_checkpoints(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<ImportCheckpoint>, String> {
        if !self.caps.has_checkpoints_table {
            return Ok(Vec::new());
        }
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT job_id, row_index, file_offset, data_hash, updated_at
                 FROM notion_import_checkpoints
                 WHERE job_id = ?1
                 ORDER BY row_index DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![job_id, limit as i64],
                |row| -> rusqlite::Result<ImportCheckpoint> {
                    Ok(ImportCheckpoint {
                        job_id: row.get(0)?,
                        row_index: row.get::<_, i64>(1)? as usize,
                        file_offset: row.get::<_, i64>(2)? as u64,
                        data_hash: row.get(3)?,
                        updated_at: row.get::<_, i64>(4)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    fn clear_checkpoints(&self, job_id: &str) -> Result<(), String> {
        if !self.caps.has_checkpoints_table {
            return Ok(());
        }
        use rusqlite::{params, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM notion_import_checkpoints WHERE job_id = ?1",
            params![job_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn list_history(
        &self,
        offset: usize,
        limit: usize,
        states: Option<&[JobState]>,
    ) -> Result<Vec<ImportJobRecord>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        use rusqlite::{params_from_iter, types::Value, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let states_vec = history_states_vec(states);
        let status_values: Vec<String> = states_vec
            .iter()
            .map(|state| job_state_to_str(state.clone()).to_string())
            .collect();
        if status_values.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..status_values.len())
            .map(|idx| format!("?{}", idx + 1))
            .collect::<Vec<_>>()
            .join(",");

        let mut sql = format!(
            "SELECT id FROM notion_import_jobs WHERE status IN ({})",
            placeholders
        );
        let order_clause = if self.caps.has_created_at {
            " ORDER BY COALESCE(ended_at, started_at, created_at, 0) DESC, id DESC"
        } else {
            " ORDER BY COALESCE(ended_at, started_at, 0) DESC, id DESC"
        };
        sql.push_str(order_clause);
        sql.push_str(&format!(
            " LIMIT ?{} OFFSET ?{}",
            status_values.len() + 1,
            status_values.len() + 2
        ));

        let mut params: Vec<Value> = status_values
            .into_iter()
            .map(Value::from)
            .collect();
        params.push(Value::from(limit as i64));
        params.push(Value::from(offset as i64));

        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let ids: Vec<String> = stmt
            .query_map(params_from_iter(params), |row| row.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();

        drop(stmt);

        let mut records: Vec<ImportJobRecord> = Vec::new();
        for id in ids {
            if let Some(record) = self.load_job(&id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    fn count_history(&self, states: Option<&[JobState]>) -> Result<usize, String> {
        use rusqlite::{params_from_iter, types::Value, Connection};
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let states_vec = history_states_vec(states);
        let status_values: Vec<String> = states_vec
            .iter()
            .map(|state| job_state_to_str(state.clone()).to_string())
            .collect();
        if status_values.is_empty() {
            return Ok(0);
        }
        let placeholders = (0..status_values.len())
            .map(|idx| format!("?{}", idx + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(1) FROM notion_import_jobs WHERE status IN ({})",
            placeholders
        );
        let mut params: Vec<Value> = status_values
            .into_iter()
            .map(Value::from)
            .collect();
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let count: i64 = stmt
            .query_row(params_from_iter(params), |row| row.get(0))
            .map_err(|e| e.to_string())?;
        Ok(count.max(0) as usize)
    }
}
