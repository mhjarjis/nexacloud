//! In-memory job registry (lost on restart in v0.1).

use std::collections::VecDeque;
use std::sync::RwLock;

use chrono::Utc;
use serde::Serialize;

use crate::util::fmt_ts;

const MAX_JOBS: usize = 200;
const LIST_LIMIT: usize = 50;
const MAX_LOG_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: JobStatus,
    pub log: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Default)]
pub struct Jobs {
    // Oldest at the front, newest at the back.
    inner: RwLock<VecDeque<Job>>,
}

impl Jobs {
    pub fn create(&self, kind: &str, title: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let job = Job {
            id: id.clone(),
            kind: kind.to_string(),
            title: title.to_string(),
            status: JobStatus::Queued,
            log: String::new(),
            started_at: fmt_ts(Utc::now()),
            finished_at: None,
        };
        let mut q = self.inner.write().unwrap_or_else(|e| e.into_inner());
        q.push_back(job);
        // Drop the oldest finished jobs beyond the cap.
        while q.len() > MAX_JOBS {
            match q
                .iter()
                .position(|j| matches!(j.status, JobStatus::Ok | JobStatus::Failed))
            {
                Some(pos) => {
                    q.remove(pos);
                }
                None => break,
            }
        }
        id
    }

    fn with_job(&self, id: &str, f: impl FnOnce(&mut Job)) {
        let mut q = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = q.iter_mut().find(|j| j.id == id) {
            f(j);
        }
    }

    pub fn set_running(&self, id: &str) {
        self.with_job(id, |j| j.status = JobStatus::Running);
    }

    pub fn log(&self, id: &str, line: &str) {
        let line = format!("[{}] {}\n", Utc::now().format("%H:%M:%S"), line.trim_end());
        self.with_job(id, |j| {
            if j.log.len() + line.len() <= MAX_LOG_BYTES {
                j.log.push_str(&line);
            }
        });
    }

    pub fn finish(&self, id: &str, ok: bool) {
        self.with_job(id, |j| {
            j.status = if ok { JobStatus::Ok } else { JobStatus::Failed };
            j.finished_at = Some(fmt_ts(Utc::now()));
        });
    }

    /// Last 50, newest first.
    pub fn list(&self) -> Vec<Job> {
        let q = self.inner.read().unwrap_or_else(|e| e.into_inner());
        q.iter().rev().take(LIST_LIMIT).cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        let q = self.inner.read().unwrap_or_else(|e| e.into_inner());
        q.iter().find(|j| j.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_ordering() {
        let jobs = Jobs::default();
        let a = jobs.create("db_backup", "Back up mysql/a");
        let b = jobs.create("db_backup", "Back up mysql/b");
        jobs.set_running(&a);
        jobs.log(&a, "hello");
        jobs.finish(&a, true);
        let ja = jobs.get(&a).unwrap();
        assert_eq!(ja.status, JobStatus::Ok);
        assert!(ja.log.contains("hello"));
        assert!(ja.finished_at.is_some());
        let list = jobs.list();
        assert_eq!(list[0].id, b);
        assert_eq!(list[1].id, a);
        let v = serde_json::to_value(&list[0]).unwrap();
        assert_eq!(v["status"], "queued");
        assert!(v["finished_at"].is_null());
    }
}
