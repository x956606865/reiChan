use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Pending,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub state: JobState,
    pub progress: JobProgress,
}

#[derive(Debug, Default)]
pub struct JobRunner {
    inner: Arc<Mutex<HashMap<String, JobSnapshot>>>,
}

impl JobRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_job(&self, job_id: impl Into<String>) {
        let job_id = job_id.into();
        let mut guard = self.inner.lock().expect("poisoned");
        guard.insert(
            job_id,
            JobSnapshot {
                state: JobState::Pending,
                progress: JobProgress::default(),
            },
        );
    }

    pub fn mark_running(&self, _job_id: &str) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(snapshot) = guard.get_mut(_job_id) {
            snapshot.state = JobState::Running;
        }
    }

    pub fn pause(&self, _job_id: &str) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(snapshot) = guard.get_mut(_job_id) {
            if snapshot.state == JobState::Running {
                snapshot.state = JobState::Paused;
            }
        }
    }

    pub fn resume(&self, _job_id: &str) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(snapshot) = guard.get_mut(_job_id) {
            if snapshot.state == JobState::Paused {
                snapshot.state = JobState::Running;
            }
        }
    }

    pub fn update_progress(&self, _job_id: &str, _update: JobProgress) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(snapshot) = guard.get_mut(_job_id) {
            if snapshot.state == JobState::Pending {
                snapshot.state = JobState::Running;
            }
            if let Some(total) = _update.total {
                snapshot.progress.total = Some(total);
            }
            snapshot.progress.done += _update.done;
            snapshot.progress.failed += _update.failed;
            snapshot.progress.skipped += _update.skipped;
        }
    }

    pub fn snapshot(&self, job_id: &str) -> Option<JobSnapshot> {
        self.inner
            .lock()
            .ok()
            .and_then(|map| map.get(job_id).cloned())
    }

    pub fn cancel(&self, job_id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(snapshot) = guard.get_mut(job_id) {
                snapshot.state = JobState::Canceled;
            }
        }
    }

    pub fn list(&self) -> Vec<(String, JobSnapshot)> {
        self.inner
            .lock()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_state_transitions_follow_expected_flow() {
        let runner = JobRunner::new();
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
        let runner = JobRunner::new();
        runner.register_job("job-2");
        runner.mark_running("job-2");

        runner.update_progress(
            "job-2",
            JobProgress {
                total: Some(200),
                done: 50,
                failed: 1,
                skipped: 0,
            },
        );
        runner.update_progress(
            "job-2",
            JobProgress {
                total: Some(200),
                done: 30,
                failed: 0,
                skipped: 5,
            },
        );

        let snapshot = runner.snapshot("job-2").expect("progress snapshot");
        assert_eq!(snapshot.state, JobState::Running);
        assert_eq!(snapshot.progress.total, Some(200));
        assert_eq!(snapshot.progress.done, 80);
        assert_eq!(snapshot.progress.failed, 1);
        assert_eq!(snapshot.progress.skipped, 5);
    }
}
