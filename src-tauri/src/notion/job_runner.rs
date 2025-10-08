use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{error::SendError, UnboundedSender};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Pending,
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    pub total: Option<usize>,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub conflict_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub state: JobState,
    pub progress: JobProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobLogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobLogEvent {
    pub level: JobLogLevel,
    pub message: String,
    pub timestamp: i64,
}

pub trait JobEventEmitter: Send + Sync {
    fn on_snapshot(&self, job_id: &str, snapshot: &JobSnapshot);
    fn on_log(&self, _job_id: &str, _event: &JobLogEvent) {}
    fn on_done(&self, _job_id: &str, _snapshot: &JobSnapshot) {}
}

pub struct NoopJobEventEmitter;

impl JobEventEmitter for NoopJobEventEmitter {
    fn on_snapshot(&self, _job_id: &str, _snapshot: &JobSnapshot) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCommand {
    Start,
    Pause,
    Resume,
    Cancel,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct JobController {
    sender: UnboundedSender<JobCommand>,
}

impl JobController {
    pub fn new(sender: UnboundedSender<JobCommand>) -> Self {
        Self { sender }
    }

    pub fn send(&self, command: JobCommand) -> Result<(), SendError<JobCommand>> {
        self.sender.send(command)
    }
}

pub struct JobRunner {
    snapshots: Arc<Mutex<HashMap<String, JobSnapshot>>>,
    controllers: Arc<Mutex<HashMap<String, JobController>>>,
    event_listener: Arc<dyn JobEventEmitter>,
}

impl JobRunner {
    pub fn new() -> Self {
        Self::with_emitter(Arc::new(NoopJobEventEmitter))
    }

    pub fn with_emitter(listener: Arc<dyn JobEventEmitter>) -> Self {
        Self {
            snapshots: Arc::new(Mutex::new(HashMap::new())),
            controllers: Arc::new(Mutex::new(HashMap::new())),
            event_listener: listener,
        }
    }

    pub fn register_job(&self, job_id: impl Into<String>) {
        let job_id = job_id.into();
        let snapshot = JobSnapshot {
            state: JobState::Pending,
            progress: JobProgress::default(),
        };
        {
            let mut guard = self.snapshots.lock().expect("poisoned");
            guard.insert(job_id.clone(), snapshot.clone());
        }
        self.event_listener.on_snapshot(&job_id, &snapshot);
    }

    pub fn mark_running(&self, job_id: &str) {
        let (should_dispatch, event) = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let mut should_dispatch = false;
            let mut event = None;
            if let Some(snapshot) = guard.get_mut(job_id) {
                let prev_state = snapshot.state.clone();
                if snapshot.state != JobState::Running {
                    snapshot.state = JobState::Running;
                    should_dispatch = true;
                }
                event = Some((snapshot.clone(), prev_state));
            }
            (should_dispatch, event)
        };

        if should_dispatch {
            self.dispatch_command(job_id, JobCommand::Start);
        }
        if let Some((snapshot, prev_state)) = event {
            self.event_listener.on_snapshot(job_id, &snapshot);
            if !is_terminal(&prev_state) && is_terminal(&snapshot.state) {
                self.event_listener.on_done(job_id, &snapshot);
            }
        }
    }

    pub fn pause(&self, job_id: &str) {
        let (should_dispatch, event) = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let mut should_dispatch = false;
            let mut event = None;
            if let Some(snapshot) = guard.get_mut(job_id) {
                let prev_state = snapshot.state.clone();
                if snapshot.state == JobState::Running {
                    snapshot.state = JobState::Paused;
                    should_dispatch = true;
                }
                event = Some((snapshot.clone(), prev_state));
            }
            (should_dispatch, event)
        };

        if should_dispatch {
            self.dispatch_command(job_id, JobCommand::Pause);
        }
        if let Some((snapshot, prev_state)) = event {
            self.event_listener.on_snapshot(job_id, &snapshot);
            if !is_terminal(&prev_state) && is_terminal(&snapshot.state) {
                self.event_listener.on_done(job_id, &snapshot);
            }
        }
    }

    pub fn resume(&self, job_id: &str) {
        let (should_dispatch, event) = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let mut should_dispatch = false;
            let mut event = None;
            if let Some(snapshot) = guard.get_mut(job_id) {
                let prev_state = snapshot.state.clone();
                if snapshot.state == JobState::Paused {
                    snapshot.state = JobState::Running;
                    should_dispatch = true;
                }
                event = Some((snapshot.clone(), prev_state));
            }
            (should_dispatch, event)
        };

        if should_dispatch {
            self.dispatch_command(job_id, JobCommand::Resume);
        }
        if let Some((snapshot, prev_state)) = event {
            self.event_listener.on_snapshot(job_id, &snapshot);
            if !is_terminal(&prev_state) && is_terminal(&snapshot.state) {
                self.event_listener.on_done(job_id, &snapshot);
            }
        }
    }

    pub fn update_progress(&self, job_id: &str, update: JobProgress) {
        let snapshot_clone = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            if let Some(snapshot) = guard.get_mut(job_id) {
                if matches!(snapshot.state, JobState::Pending | JobState::Queued) {
                    snapshot.state = JobState::Running;
                }
                if let Some(total) = update.total {
                    snapshot.progress.total = Some(total);
                }
                snapshot.progress.done += update.done;
                snapshot.progress.failed += update.failed;
                snapshot.progress.skipped += update.skipped;
                if let Some(conflicts) = update.conflict_total {
                    let entry = snapshot.progress.conflict_total.get_or_insert(0);
                    *entry += conflicts;
                }
                Some(snapshot.clone())
            } else {
                None
            }
        };

        if let Some(snapshot) = snapshot_clone {
            self.event_listener.on_snapshot(job_id, &snapshot);
        }
    }

    pub fn emit_log(&self, job_id: &str, level: JobLogLevel, message: impl Into<String>) {
        let event = JobLogEvent {
            level,
            message: message.into(),
            timestamp: Utc::now().timestamp_millis(),
        };
        self.event_listener.on_log(job_id, &event);
    }

    pub fn snapshot(&self, job_id: &str) -> Option<JobSnapshot> {
        self.snapshots
            .lock()
            .ok()
            .and_then(|map| map.get(job_id).cloned())
    }

    pub fn cancel(&self, job_id: &str) {
        let (should_dispatch, event) = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let mut should_dispatch = false;
            let mut event = None;
            if let Some(snapshot) = guard.get_mut(job_id) {
                let prev_state = snapshot.state.clone();
                if snapshot.state != JobState::Canceled {
                    snapshot.state = JobState::Canceled;
                    should_dispatch = true;
                }
                event = Some((snapshot.clone(), prev_state));
            }
            (should_dispatch, event)
        };

        if should_dispatch {
            self.dispatch_command(job_id, JobCommand::Cancel);
        }
        if let Some((snapshot, prev_state)) = event {
            self.event_listener.on_snapshot(job_id, &snapshot);
            if !is_terminal(&prev_state) && is_terminal(&snapshot.state) {
                self.event_listener.on_done(job_id, &snapshot);
            }
        }
    }

    pub fn list(&self) -> Vec<(String, JobSnapshot)> {
        self.snapshots
            .lock()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn set_state(&self, job_id: &str, state: JobState) {
        let event = {
            let mut guard = match self.snapshots.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            if let Some(snapshot) = guard.get_mut(job_id) {
                let prev_state = snapshot.state.clone();
                snapshot.state = state;
                Some((snapshot.clone(), prev_state))
            } else {
                None
            }
        };

        if let Some((snapshot, prev_state)) = event {
            self.event_listener.on_snapshot(job_id, &snapshot);
            if !is_terminal(&prev_state) && is_terminal(&snapshot.state) {
                self.event_listener.on_done(job_id, &snapshot);
            }
        }
    }

    pub fn attach_controller(&self, job_id: &str, controller: JobController) -> Result<(), String> {
        let mut guard = self
            .controllers
            .lock()
            .map_err(|_| "poisoned".to_string())?;
        guard.insert(job_id.to_string(), controller);
        Ok(())
    }

    fn dispatch_command(&self, job_id: &str, command: JobCommand) {
        if let Ok(controllers) = self.controllers.lock() {
            if let Some(controller) = controllers.get(job_id) {
                let _ = controller.send(command);
            }
        }
    }
}

impl Default for JobRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for JobRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRunner")
            .field("snapshots", &"<hidden>")
            .finish()
    }
}

fn is_terminal(state: &JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Failed | JobState::Canceled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    struct RecordingEmitter {
        events: Arc<Mutex<Vec<(String, JobSnapshot)>>>,
        done: Arc<Mutex<Vec<(String, JobSnapshot)>>>,
        logs: Arc<Mutex<Vec<(String, JobLogEvent)>>>,
    }

    impl RecordingEmitter {
        fn new() -> (
            Arc<Self>,
            Arc<Mutex<Vec<(String, JobSnapshot)>>>,
            Arc<Mutex<Vec<(String, JobSnapshot)>>>,
            Arc<Mutex<Vec<(String, JobLogEvent)>>>,
        ) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let done = Arc::new(Mutex::new(Vec::new()));
            let logs = Arc::new(Mutex::new(Vec::new()));
            let emitter = Arc::new(Self {
                events: events.clone(),
                done: done.clone(),
                logs: logs.clone(),
            });
            (emitter, events, done, logs)
        }
    }

    impl JobEventEmitter for RecordingEmitter {
        fn on_snapshot(&self, job_id: &str, snapshot: &JobSnapshot) {
            self.events
                .lock()
                .expect("lock")
                .push((job_id.to_string(), snapshot.clone()));
        }

        fn on_log(&self, job_id: &str, event: &JobLogEvent) {
            self.logs
                .lock()
                .expect("lock logs")
                .push((job_id.to_string(), event.clone()));
        }

        fn on_done(&self, job_id: &str, snapshot: &JobSnapshot) {
            self.done
                .lock()
                .expect("lock done")
                .push((job_id.to_string(), snapshot.clone()));
        }
    }

    #[test]
    fn job_state_transitions_follow_expected_flow() {
        let (emitter, _events, _done, _logs) = RecordingEmitter::new();
        let runner = JobRunner::with_emitter(emitter);
        runner.register_job("job-1");

        let snapshot = runner.snapshot("job-1").expect("job snapshot");
        assert_eq!(snapshot.state, JobState::Pending);

        runner.mark_running("job-1");
        let running = runner.snapshot("job-1").expect("running snapshot");
        assert_eq!(running.state, JobState::Running);

        runner.pause("job-1");
        let paused = runner.snapshot("job-1").expect("paused snapshot");
        assert_eq!(paused.state, JobState::Paused);

        runner.resume("job-1");
        let resumed = runner.snapshot("job-1").expect("resumed snapshot");
        assert_eq!(resumed.state, JobState::Running);
    }

    #[test]
    fn progress_updates_accumulate_values() {
        let (emitter, _events, _done, _logs) = RecordingEmitter::new();
        let runner = JobRunner::with_emitter(emitter);
        runner.register_job("job-2");
        runner.mark_running("job-2");

        runner.update_progress(
            "job-2",
            JobProgress {
                total: Some(200),
                done: 50,
                failed: 1,
                skipped: 0,
                conflict_total: None,
            },
        );
        runner.update_progress(
            "job-2",
            JobProgress {
                total: Some(200),
                done: 30,
                failed: 0,
                skipped: 5,
                conflict_total: None,
            },
        );

        let snapshot = runner.snapshot("job-2").expect("progress snapshot");
        assert_eq!(snapshot.state, JobState::Running);
        assert_eq!(snapshot.progress.total, Some(200));
        assert_eq!(snapshot.progress.done, 80);
        assert_eq!(snapshot.progress.failed, 1);
        assert_eq!(snapshot.progress.skipped, 5);
    }

    #[test]
    fn pause_resume_cancel_forward_commands() {
        let (emitter, _events, _done, _logs) = RecordingEmitter::new();
        let runner = JobRunner::with_emitter(emitter);
        runner.register_job("job-ctrl");
        runner.mark_running("job-ctrl");

        let (tx, mut rx) = unbounded_channel();
        runner
            .attach_controller("job-ctrl", JobController::new(tx))
            .expect("attach controller");

        runner.pause("job-ctrl");
        runner.resume("job-ctrl");
        runner.cancel("job-ctrl");

        let first = rx.try_recv().expect("pause command");
        assert_eq!(first, JobCommand::Pause);
        let second = rx.try_recv().expect("resume command");
        assert_eq!(second, JobCommand::Resume);
        let third = rx.try_recv().expect("cancel command");
        assert_eq!(third, JobCommand::Cancel);
    }

    #[test]
    fn completed_state_triggers_done_event_once() {
        let (emitter, _events, done, _logs) = RecordingEmitter::new();
        let runner = JobRunner::with_emitter(emitter);
        runner.register_job("job-done");
        runner.mark_running("job-done");
        runner.update_progress(
            "job-done",
            JobProgress {
                total: Some(10),
                done: 10,
                failed: 0,
                skipped: 0,
                conflict_total: None,
            },
        );
        runner.set_state("job-done", JobState::Completed);

        let done_events = done.lock().expect("done lock");
        assert_eq!(done_events.len(), 1);
        assert_eq!(done_events[0].0, "job-done");
        assert_eq!(done_events[0].1.state, JobState::Completed);
    }

    #[test]
    fn emit_log_dispatches_to_listener() {
        let (emitter, _events, _done, logs) = RecordingEmitter::new();
        let runner = JobRunner::with_emitter(emitter);
        runner.register_job("job-log");
        runner.emit_log("job-log", JobLogLevel::Info, "hello");

        let log_events = logs.lock().expect("logs lock");
        assert_eq!(log_events.len(), 1);
        assert_eq!(log_events[0].0, "job-log");
        assert_eq!(log_events[0].1.message, "hello");
        assert_eq!(log_events[0].1.level, JobLogLevel::Info);
    }
}
