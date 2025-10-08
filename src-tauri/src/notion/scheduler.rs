use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::notion::adapter::NotionAdapter;
use crate::notion::import::{ImportEngine, StartContext};
use crate::notion::job_runner::{JobLogLevel, JobRunner, JobState};
use crate::notion::storage::{ImportJobRecord, ImportJobStore, StateTransition, TokenStore};

const DEFAULT_MAX_PARALLEL: usize = 2;
const DEFAULT_POLL_INTERVAL_MS: u64 = 200;
const DEFAULT_LEASE_EXTENSION_MS: i64 = 30_000;
const DEFAULT_HEARTBEAT_TIMEOUT_MS: i64 = 60_000;

#[derive(Clone)]
pub struct SchedulerConfig {
    pub max_parallel_jobs: usize,
    pub poll_interval: Duration,
    pub lease_extension_ms: i64,
    pub heartbeat_timeout_ms: i64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_parallel_jobs: DEFAULT_MAX_PARALLEL,
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            lease_extension_ms: DEFAULT_LEASE_EXTENSION_MS,
            heartbeat_timeout_ms: DEFAULT_HEARTBEAT_TIMEOUT_MS,
        }
    }
}

pub struct SchedulerDeps {
    pub token_store: Arc<dyn TokenStore>,
    pub job_store: Arc<dyn ImportJobStore>,
    pub job_runner: Arc<JobRunner>,
    pub adapter: Arc<dyn NotionAdapter>,
}

enum SchedulerCommand {
    Enqueue(String),
    Promote(String),
    Requeue(String),
    SetPriority { job_id: String, priority: i32 },
    Shutdown,
}

struct ActiveJob {
    last_heartbeat: i64,
    lease_expires_at: Option<i64>,
}

struct SchedulerCore {
    config: SchedulerConfig,
    deps: SchedulerDeps,
    active: HashMap<String, ActiveJob>,
    pending_hint: HashSet<String>,
}

impl SchedulerCore {
    fn new(config: SchedulerConfig, deps: SchedulerDeps) -> Self {
        Self {
            config,
            deps,
            active: HashMap::new(),
            pending_hint: HashSet::new(),
        }
    }

    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn handle_command(&mut self, command: SchedulerCommand) -> bool {
        match command {
            SchedulerCommand::Enqueue(job_id) => {
                self.pending_hint.insert(job_id);
                false
            }
            SchedulerCommand::Promote(job_id) => {
                self.handle_promote(job_id);
                false
            }
            SchedulerCommand::Requeue(job_id) => {
                self.handle_requeue(job_id);
                false
            }
            SchedulerCommand::SetPriority { job_id, priority } => {
                self.handle_set_priority(job_id, priority);
                false
            }
            SchedulerCommand::Shutdown => true,
        }
    }

    fn handle_promote(&mut self, job_id: String) {
        let Some(record) = self.load_job(&job_id) else {
            return;
        };
        let max_priority = self
            .deps
            .job_store
            .list_pending_jobs()
            .ok()
            .and_then(|jobs| jobs.iter().map(|job| job.priority).max())
            .unwrap_or(record.priority);
        let target = max_priority.saturating_add(1);
        if target == record.priority {
            self.pending_hint.insert(job_id);
            return;
        }
        if let Err(err) = self.deps.job_store.set_priority(&job_id, target) {
            eprintln!("[scheduler] failed to promote job {}: {}", job_id, err);
            return;
        }
        self.deps.job_runner.emit_log(
            &job_id,
            JobLogLevel::Info,
            format!("promoted to priority {}", target),
        );
        self.pending_hint.insert(job_id);
    }

    fn handle_set_priority(&mut self, job_id: String, priority: i32) {
        if let Err(err) = self.deps.job_store.set_priority(&job_id, priority) {
            eprintln!("[scheduler] failed to set priority for {}: {}", job_id, err);
            return;
        }
        self.deps.job_runner.emit_log(
            &job_id,
            JobLogLevel::Info,
            format!("priority updated to {}", priority),
        );
        self.pending_hint.insert(job_id);
    }

    fn handle_requeue(&mut self, job_id: String) {
        let Some(record) = self.load_job(&job_id) else {
            return;
        };
        if record.state == JobState::Running {
            self.deps.job_runner.emit_log(
                &job_id,
                JobLogLevel::Warn,
                "job is running; wait for completion before requeue",
            );
            return;
        }
        let transition = StateTransition {
            state: JobState::Queued,
            started_at: record.started_at,
            ended_at: None,
            last_error: record.last_error.clone(),
        };
        if let Err(err) = self.deps.job_store.mark_state(&job_id, transition) {
            eprintln!("[scheduler] failed to requeue job {}: {}", job_id, err);
            return;
        }
        if let Err(err) = self.deps.job_store.touch_lease(&job_id, None) {
            eprintln!("[scheduler] failed to clear lease for {}: {}", job_id, err);
        }
        self.active.remove(&job_id);
        self.deps.job_runner.set_state(&job_id, JobState::Queued);
        self.pending_hint.insert(job_id);
    }

    fn load_job(&self, job_id: &str) -> Option<ImportJobRecord> {
        match self.deps.job_store.load_job(job_id) {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                eprintln!("[scheduler] job {} not found", job_id);
                None
            }
            Err(err) => {
                eprintln!("[scheduler] failed to load job {}: {}", job_id, err);
                None
            }
        }
    }

    fn tick(&mut self) {
        let now = Self::now_ms();
        let jobs = match self.deps.job_store.list_pending_jobs() {
            Ok(list) => list,
            Err(err) => {
                eprintln!("[scheduler] failed to list jobs: {}", err);
                return;
            }
        };

        let mut running_ids = HashSet::new();
        for job in jobs.iter().filter(|job| job.state == JobState::Running) {
            running_ids.insert(job.id.clone());
            self.ensure_registered(job);
            self.observe_heartbeat(job, now);
        }

        self.active.retain(|job_id, _| running_ids.contains(job_id));

        let capacity = self
            .config
            .max_parallel_jobs
            .saturating_sub(self.active.len());
        if capacity == 0 {
            return;
        }

        let mut candidates: Vec<_> = jobs
            .into_iter()
            .filter(|job| matches!(job.state, JobState::Pending | JobState::Queued))
            .collect();
        if candidates.is_empty() {
            return;
        }

        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });

        for job in candidates.into_iter().take(capacity) {
            if self.active.contains_key(&job.id) {
                continue;
            }
            if !self.pending_hint.is_empty() && !self.pending_hint.contains(&job.id) {
                continue;
            }
            if let Err(err) = self.start_job(job) {
                eprintln!("[scheduler] failed to start job: {}", err);
            }
        }
    }

    fn ensure_registered(&self, job: &ImportJobRecord) {
        if self.deps.job_runner.snapshot(&job.id).is_none() {
            self.deps.job_runner.register_job(job.id.clone());
            self.deps
                .job_runner
                .update_progress(&job.id, job.progress.clone());
        }
        self.deps.job_runner.set_state(&job.id, job.state.clone());
    }

    fn observe_heartbeat(&mut self, job: &ImportJobRecord, now: i64) {
        let heartbeat = job.last_heartbeat.unwrap_or(job.created_at);
        let mut remove = false;
        if let Some(entry) = self.active.get_mut(&job.id) {
            if heartbeat > entry.last_heartbeat {
                entry.last_heartbeat = heartbeat;
                let lease_deadline = heartbeat + self.config.lease_extension_ms;
                entry.lease_expires_at = Some(lease_deadline);
                let _ = self
                    .deps
                    .job_store
                    .touch_lease(&job.id, Some(lease_deadline));
            } else if let Some(expiry) = entry.lease_expires_at {
                if now > expiry + self.config.heartbeat_timeout_ms {
                    remove = true;
                }
            }
        } else {
            self.active.insert(
                job.id.clone(),
                ActiveJob {
                    last_heartbeat: heartbeat,
                    lease_expires_at: job.lease_expires_at,
                },
            );
        }

        if remove {
            self.requeue_job(job, "lease expired; re-queueing orphaned job");
        }
    }

    fn requeue_job(&mut self, job: &ImportJobRecord, message: &str) {
        self.deps
            .job_runner
            .emit_log(&job.id, JobLogLevel::Warn, message);
        let _ = self.deps.job_store.mark_state(
            &job.id,
            StateTransition {
                state: JobState::Queued,
                started_at: job.started_at,
                ended_at: None,
                last_error: job.last_error.clone(),
            },
        );
        let _ = self.deps.job_store.touch_lease(&job.id, None);
        self.deps.job_runner.set_state(&job.id, JobState::Queued);
        self.active.remove(&job.id);
        self.pending_hint.insert(job.id.clone());
    }

    fn start_job(&mut self, job: ImportJobRecord) -> Result<(), String> {
        let token = self
            .deps
            .token_store
            .get_token(&job.token_id)
            .ok_or_else(|| format!("token {} missing for job", job.token_id))?;
        if !Path::new(&job.source_file_path).exists() {
            return Err(format!("source file missing: {}", job.source_file_path));
        }

        self.ensure_registered(&job);
        let now = Self::now_ms();
        self.deps.job_runner.set_state(&job.id, JobState::Running);
        let _ = self.deps.job_store.mark_state(
            &job.id,
            StateTransition {
                state: JobState::Running,
                started_at: job.started_at.or(Some(now)),
                ended_at: None,
                last_error: job.last_error.clone(),
            },
        );

        let lease_deadline = now + self.config.lease_extension_ms;
        let _ = self
            .deps
            .job_store
            .touch_lease(&job.id, Some(lease_deadline));

        let engine = ImportEngine::new(
            Arc::clone(&self.deps.adapter),
            Arc::clone(&self.deps.job_store),
            Arc::clone(&self.deps.job_runner),
        );
        if let Err(err) = engine.spawn_job(StartContext {
            job_id: job.id.clone(),
            token: Some(token),
        }) {
            self.deps
                .job_runner
                .emit_log(&job.id, JobLogLevel::Error, err.clone());
            let _ = self.deps.job_store.mark_state(
                &job.id,
                StateTransition {
                    state: JobState::Failed,
                    started_at: job.started_at.or(Some(now)),
                    ended_at: Some(Self::now_ms()),
                    last_error: Some(err.clone()),
                },
            );
            self.deps.job_runner.set_state(&job.id, JobState::Failed);
            return Err(err);
        }

        self.active.insert(
            job.id.clone(),
            ActiveJob {
                last_heartbeat: job.last_heartbeat.unwrap_or(now),
                lease_expires_at: Some(lease_deadline),
            },
        );
        self.pending_hint.remove(&job.id);
        Ok(())
    }
}

pub struct Scheduler {
    sender: mpsc::Sender<SchedulerCommand>,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Scheduler {
    pub fn spawn(config: SchedulerConfig, deps: SchedulerDeps) -> Self {
        let (tx, rx) = mpsc::channel::<SchedulerCommand>();
        let mut core = SchedulerCore::new(config.clone(), deps);
        let handle = thread::Builder::new()
            .name("notion-scheduler".into())
            .spawn(move || loop {
                match rx.recv_timeout(config.poll_interval) {
                    Ok(cmd) => {
                        if core.handle_command(cmd) {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                core.tick();
            })
            .expect("spawn scheduler thread");
        Self {
            sender: tx,
            join_handle: Mutex::new(Some(handle)),
        }
    }

    pub fn enqueue(&self, job_id: String) -> Result<(), String> {
        self.sender
            .send(SchedulerCommand::Enqueue(job_id))
            .map_err(|_| "scheduler stopped".into())
    }

    pub fn promote(&self, job_id: String) -> Result<(), String> {
        self.sender
            .send(SchedulerCommand::Promote(job_id))
            .map_err(|_| "scheduler stopped".into())
    }

    pub fn requeue(&self, job_id: String) -> Result<(), String> {
        self.sender
            .send(SchedulerCommand::Requeue(job_id))
            .map_err(|_| "scheduler stopped".into())
    }

    pub fn set_priority(&self, job_id: String, priority: i32) -> Result<(), String> {
        self.sender
            .send(SchedulerCommand::SetPriority { job_id, priority })
            .map_err(|_| "scheduler stopped".into())
    }

    pub fn shutdown(&self) {
        if self.sender.send(SchedulerCommand::Shutdown).is_ok() {
            if let Ok(mut guard) = self.join_handle.lock() {
                if let Some(handle) = guard.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notion::adapter::MockNotionAdapter;
    use crate::notion::storage::{InMemoryJobStore, InMemoryTokenStore, NewImportJob};
    use serde_json::json;
    use std::time::Instant;
    use tempfile::Builder;

    fn write_sample_records() -> tempfile::NamedTempFile {
        let mut file = Builder::new()
            .suffix(".json")
            .tempfile()
            .expect("temp file");
        let records = json!([
            { "name": "Alpha" },
            { "name": "Beta" }
        ]);
        serde_json::to_writer(file.as_file_mut(), &records).expect("write json");
        file
    }

    fn build_snapshot(path: &str, token_id: &str) -> String {
        json!({
            "version": 1,
            "tokenId": token_id,
            "databaseId": "db_mock",
            "sourceFilePath": path,
            "fileType": "json",
            "mappings": [{
                "include": true,
                "sourceField": "name",
                "targetProperty": "Name",
                "targetType": "title",
                "transformCode": serde_json::Value::Null
            }],
            "defaults": serde_json::Value::Null,
            "rateLimit": serde_json::Value::Null,
            "batchSize": 1,
            "priority": 0,
            "upsert": serde_json::Value::Null
        })
        .to_string()
    }

    fn insert_job(job_store: &Arc<dyn ImportJobStore>, new_job: NewImportJob) -> ImportJobRecord {
        job_store.insert_job(new_job).expect("insert job")
    }

    fn wait_for_state(
        job_store: &Arc<dyn ImportJobStore>,
        job_id: &str,
        expected: JobState,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(Some(record)) = job_store.load_job(job_id) {
                if record.state == expected {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn dispatches_jobs_with_parallel_limit() {
        let token_store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let token = token_store.save("default", "secret", Some("Workspace".into()));
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter::new());

        let records_file1 = write_sample_records();
        let records_file2 = write_sample_records();

        let job1_id = "job-1";
        let job2_id = "job-2";
        let created_at = chrono::Utc::now().timestamp_millis();

        insert_job(
            &job_store,
            NewImportJob {
                id: job1_id.into(),
                token_id: token.id.clone(),
                database_id: "db_mock".into(),
                source_file_path: records_file1.path().to_string_lossy().to_string(),
                config_snapshot_json: build_snapshot(
                    records_file1.path().to_string_lossy().as_ref(),
                    &token.id,
                ),
                total: Some(2),
                created_at,
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            },
        );
        insert_job(
            &job_store,
            NewImportJob {
                id: job2_id.into(),
                token_id: token.id.clone(),
                database_id: "db_mock".into(),
                source_file_path: records_file2.path().to_string_lossy().to_string(),
                config_snapshot_json: build_snapshot(
                    records_file2.path().to_string_lossy().as_ref(),
                    &token.id,
                ),
                total: Some(2),
                created_at: created_at + 1,
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            },
        );

        let scheduler = Scheduler::spawn(
            SchedulerConfig {
                max_parallel_jobs: 1,
                poll_interval: Duration::from_millis(20),
                lease_extension_ms: 5_000,
                heartbeat_timeout_ms: 10_000,
            },
            SchedulerDeps {
                token_store: Arc::clone(&token_store),
                job_store: Arc::clone(&job_store),
                job_runner: Arc::clone(&job_runner),
                adapter: Arc::clone(&adapter),
            },
        );

        scheduler.enqueue(job1_id.into()).expect("enqueue job1");
        scheduler.enqueue(job2_id.into()).expect("enqueue job2");

        assert!(
            wait_for_state(
                &job_store,
                job1_id,
                JobState::Completed,
                Duration::from_secs(5)
            ),
            "job1 should complete"
        );
        assert!(
            wait_for_state(
                &job_store,
                job2_id,
                JobState::Completed,
                Duration::from_secs(5)
            ),
            "job2 should complete"
        );

        let job1 = job_store.load_job(job1_id).unwrap().unwrap();
        let job2 = job_store.load_job(job2_id).unwrap().unwrap();
        assert!(
            job2.started_at.unwrap_or_default() >= job1.ended_at.unwrap_or_default(),
            "job2 should start after job1 finishes"
        );

        scheduler.shutdown();
    }

    #[test]
    fn promote_moves_job_to_front() {
        let token_store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
        let token = token_store.save("default", "secret", Some("Workspace".into()));
        let job_store: Arc<dyn ImportJobStore> = Arc::new(InMemoryJobStore::new());
        let job_runner = Arc::new(JobRunner::new());
        let adapter: Arc<dyn NotionAdapter> = Arc::new(MockNotionAdapter::new());

        let records_file1 = write_sample_records();
        let records_file2 = write_sample_records();

        let job1_id = "job-promote-1";
        let job2_id = "job-promote-2";
        let created_at = chrono::Utc::now().timestamp_millis();

        insert_job(
            &job_store,
            NewImportJob {
                id: job1_id.into(),
                token_id: token.id.clone(),
                database_id: "db_mock".into(),
                source_file_path: records_file1.path().to_string_lossy().to_string(),
                config_snapshot_json: build_snapshot(
                    records_file1.path().to_string_lossy().as_ref(),
                    &token.id,
                ),
                total: Some(2),
                created_at,
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            },
        );
        insert_job(
            &job_store,
            NewImportJob {
                id: job2_id.into(),
                token_id: token.id.clone(),
                database_id: "db_mock".into(),
                source_file_path: records_file2.path().to_string_lossy().to_string(),
                config_snapshot_json: build_snapshot(
                    records_file2.path().to_string_lossy().as_ref(),
                    &token.id,
                ),
                total: Some(2),
                created_at: created_at + 1,
                priority: 0,
                lease_expires_at: None,
                conflict_total: Some(0),
            },
        );

        let scheduler = Scheduler::spawn(
            SchedulerConfig {
                max_parallel_jobs: 1,
                poll_interval: Duration::from_millis(25),
                lease_extension_ms: 5_000,
                heartbeat_timeout_ms: 10_000,
            },
            SchedulerDeps {
                token_store: Arc::clone(&token_store),
                job_store: Arc::clone(&job_store),
                job_runner: Arc::clone(&job_runner),
                adapter: Arc::clone(&adapter),
            },
        );

        scheduler
            .promote(job2_id.into())
            .expect("promote job2 should succeed");
        thread::sleep(Duration::from_millis(50));

        scheduler.enqueue(job1_id.into()).expect("enqueue job1");
        scheduler.enqueue(job2_id.into()).expect("enqueue job2");

        assert!(
            wait_for_state(
                &job_store,
                job2_id,
                JobState::Completed,
                Duration::from_secs(5)
            ),
            "job2 should complete"
        );
        assert!(
            wait_for_state(
                &job_store,
                job1_id,
                JobState::Completed,
                Duration::from_secs(5)
            ),
            "job1 should complete"
        );

        let job1 = job_store.load_job(job1_id).unwrap().unwrap();
        let job2 = job_store.load_job(job2_id).unwrap().unwrap();
        assert!(
            job1.started_at.unwrap_or_default() >= job2.ended_at.unwrap_or_default(),
            "job1 should start after promoted job2 finishes"
        );

        scheduler.shutdown();
    }
}
