//! Persistent background job queue.
//!
//! Every job is a row in the `jobs` table plus a log file
//! `{data_dir}/jobs/<id>.log`. Scheduling rules:
//! - **heavy** jobs (dumps, installs, builds) run one at a time (a semaphore);
//!   light jobs run concurrently;
//! - **named resource locks** (`nginx`, `site:<id>`, `db:<engine>/<name>`,
//!   `apt`, …): two jobs that name the same lock never overlap. A job takes
//!   the heavy permit first (if heavy), then all of its locks at once, so the
//!   queue cannot deadlock;
//! - a per-job **timeout** and **cancel** (the job future is dropped, which
//!   kills the process groups started through
//!   [`Cmd::run_streaming`](crate::util::cmd::Cmd::run_streaming));
//! - jobs that are `queued`/`running` when the agent starts are marked
//!   `interrupted` ([`Jobs::mark_interrupted_at_startup`], `serve` only).
//!
//! Later phases submit work with [`Jobs::submit`]:
//! ```ignore
//! let id = state.jobs.submit(
//!     JobSpec::new("site_deploy", "Deploy shop").heavy().lock("site:shop").timeout(secs(1800)),
//!     move |ctx| async move {
//!         ctx.log("cloning…");
//!         let out = Cmd::new("git").args(["clone", …]).run_streaming(&*ctx.log).await;
//!         anyhow::ensure!(out.ok, "git clone failed");
//!         Ok(())
//!     },
//! )?;
//! ```

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::util::cmd::LineSink;
use crate::util::fmt_ts;

/// Default number of jobs returned by `GET /api/jobs`.
pub const LIST_LIMIT: usize = 50;
/// `GET /api/jobs/{id}` returns at most this much of the log (the tail).
pub const MAX_LOG_RESPONSE: u64 = 256 * 1024;
/// The list endpoint returns at most this much of each log (the tail).
const MAX_LIST_LOG: u64 = 16 * 1024;
/// Stop writing a job log beyond this size.
const MAX_LOG_FILE: usize = 16 * 1024 * 1024;
/// Finished jobs kept (rows + log files); older ones are pruned at startup.
const KEEP_JOBS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Ok,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Ok => "ok",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> JobStatus {
        match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "ok" => JobStatus::Ok,
            "failed" => JobStatus::Failed,
            "cancelled" => JobStatus::Cancelled,
            _ => JobStatus::Interrupted,
        }
    }

    pub fn is_finished(self) -> bool {
        !matches!(self, JobStatus::Queued | JobStatus::Running)
    }
}

/// A job as returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: JobStatus,
    /// Log text (the tail if long).
    pub log: String,
    pub created_at: String,
    /// When it started running (null while queued).
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub heavy: bool,
    pub locks: Vec<String>,
    pub timeout_secs: Option<u64>,
    /// Whether `POST /api/jobs/{id}/cancel` may stop it while running.
    pub cancellable: bool,
    /// Failure / cancel reason.
    pub error: Option<String>,
    /// The change this job applies or rolls back, if any.
    pub change_id: Option<String>,
    /// Client IP that started it (`cli` for the command line, null for the system).
    pub actor_ip: Option<String>,
}

/// What to run and under which constraints. Built with chained setters.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: String,
    pub title: String,
    pub heavy: bool,
    pub locks: Vec<String>,
    pub timeout: Option<Duration>,
    pub cancellable: bool,
    pub change_id: Option<String>,
    pub actor_ip: Option<String>,
}

impl JobSpec {
    pub fn new(kind: &str, title: &str) -> Self {
        JobSpec {
            kind: kind.to_string(),
            title: title.to_string(),
            heavy: false,
            locks: Vec::new(),
            timeout: None,
            cancellable: true,
            change_id: None,
            actor_ip: None,
        }
    }

    /// Only one heavy job runs at a time.
    pub fn heavy(mut self) -> Self {
        self.heavy = true;
        self
    }

    /// Hold the named resource lock while running.
    pub fn lock(mut self, name: impl Into<String>) -> Self {
        let n = name.into();
        if !self.locks.contains(&n) {
            self.locks.push(n);
        }
        self
    }

    pub fn locks<I: IntoIterator<Item = String>>(mut self, names: I) -> Self {
        for n in names {
            self = self.lock(n);
        }
        self
    }

    /// Fail (and kill its processes) if still running after `d`. Ignored for
    /// jobs that are not cancellable while running: those must bound every
    /// step themselves (e.g. per-command timeouts in the change pipeline).
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// The job must not be interrupted half-way (e.g. applying a change, which
    /// must finish or auto-revert). It can still be cancelled while queued.
    pub fn not_cancellable_while_running(mut self) -> Self {
        self.cancellable = false;
        self
    }

    pub fn change(mut self, change_id: &str) -> Self {
        self.change_id = Some(change_id.to_string());
        self
    }

    pub fn actor(mut self, ip: Option<String>) -> Self {
        self.actor_ip = ip;
        self
    }
}

// ------------------------------------------------------------------ logs

/// A job's log: lines go to `{data_dir}/jobs/<id>.log` (and/or stdout for the
/// CLI), each prefixed with `[HH:MM:SS]`.
pub struct JobLog {
    file: Option<Mutex<File>>,
    echo: bool,
    written: AtomicUsize,
}

impl JobLog {
    /// Append to (creating, mode 0600) the given log file.
    pub fn open(path: &Path) -> Result<JobLog> {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening job log {}", path.display()))?;
        let len = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
        Ok(JobLog {
            file: Some(Mutex::new(f)),
            echo: false,
            written: AtomicUsize::new(len),
        })
    }

    /// Print lines to stdout (CLI commands).
    pub fn stdout() -> JobLog {
        JobLog {
            file: None,
            echo: true,
            written: AtomicUsize::new(0),
        }
    }

    /// Discard everything (tests, best-effort internal work).
    #[cfg(test)]
    pub fn sink() -> JobLog {
        JobLog {
            file: None,
            echo: false,
            written: AtomicUsize::new(0),
        }
    }

    pub fn line(&self, line: &str) {
        let text = format!(
            "[{}] {}\n",
            Utc::now().format("%H:%M:%S"),
            line.trim_end_matches(['\r', '\n'])
        );
        if self.echo {
            print!("{text}");
        }
        if let Some(f) = &self.file {
            let before = self.written.fetch_add(text.len(), Ordering::Relaxed);
            let mut f = f.lock().unwrap_or_else(|e| e.into_inner());
            if before + text.len() <= MAX_LOG_FILE {
                let _ = f.write_all(text.as_bytes());
            } else if before <= MAX_LOG_FILE {
                let _ = f.write_all(b"[log truncated: size limit reached]\n");
            }
        }
    }
}

impl LineSink for JobLog {
    fn line(&self, line: &str) {
        JobLog::line(self, line)
    }
}

/// Handed to the job's body.
pub struct JobCtx {
    #[allow(dead_code)] // for job bodies that reference their own job (phase 1+)
    pub id: String,
    pub log: Arc<JobLog>,
}

impl JobCtx {
    pub fn log(&self, line: &str) {
        self.log.line(line)
    }
}

/// Read up to `max` bytes from the end of a log file ("" if missing).
pub fn read_log_tail(path: &Path, max: u64) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(max).read_to_end(&mut buf);
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // Drop the partial first line.
        if let Some(i) = s.find('\n') {
            s = format!("[… earlier output omitted]\n{}", &s[i + 1..]);
        }
    }
    s
}

// ------------------------------------------------------------------ locks

/// Named resource locks. All names of a job are taken atomically.
#[derive(Default)]
pub struct LockManager {
    held: Mutex<HashSet<String>>,
    notify: Notify,
}

pub struct LockGuard {
    mgr: Arc<LockManager>,
    names: Vec<String>,
}

impl LockManager {
    /// Wait until none of `names` is held, then hold all of them.
    pub async fn acquire(self: &Arc<Self>, names: &[String]) -> LockGuard {
        let mut names: Vec<String> = names.to_vec();
        names.sort();
        names.dedup();
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
                if names.iter().all(|n| !held.contains(n)) {
                    for n in &names {
                        held.insert(n.clone());
                    }
                    return LockGuard {
                        mgr: self.clone(),
                        names,
                    };
                }
            }
            notified.await;
        }
    }

    /// Names currently held (diagnostics).
    #[cfg(test)]
    pub fn held(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        v.sort();
        v
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        {
            let mut held = self.mgr.held.lock().unwrap_or_else(|e| e.into_inner());
            for n in &self.names {
                held.remove(n);
            }
        }
        self.mgr.notify.notify_waiters();
    }
}

// ------------------------------------------------------------------ queue

#[derive(Debug, PartialEq, Eq)]
pub enum CancelError {
    NotFound,
    AlreadyFinished,
    NotCancellable,
}

pub struct Jobs {
    db: Db,
    dir: PathBuf,
    heavy: Arc<Semaphore>,
    locks: Arc<LockManager>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
    /// Set when the agent is stopping: jobs that end now (their processes
    /// were killed by the shutdown) are left `running`, so the next start
    /// marks them `interrupted` instead of `failed`.
    shutting_down: AtomicBool,
}

const JOB_COLS: &str = "id, kind, title, status, heavy, locks, timeout_secs, cancellable, \
                        created_at, started_at, finished_at, error, change_id, actor_ip";

impl Jobs {
    /// `dir` is `{data_dir}/jobs` (created if missing).
    pub fn new(db: Db, dir: PathBuf) -> Arc<Jobs> {
        let _ = crate::util::create_private_dir(&dir);
        Arc::new(Jobs {
            db,
            dir,
            heavy: Arc::new(Semaphore::new(1)),
            locks: Arc::new(LockManager::default()),
            cancels: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
        })
    }

    /// The agent received SIGTERM/Ctrl-C.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    pub fn log_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.log"))
    }

    /// Called once by `serve` before accepting requests: anything still
    /// `queued`/`running` belongs to a previous process and will never finish.
    /// Also prunes old finished jobs. Returns how many were interrupted.
    pub fn mark_interrupted_at_startup(&self) -> Result<usize> {
        let now = fmt_ts(Utc::now());
        let ids: Vec<String> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT id FROM jobs WHERE status IN ('queued','running')")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            if let Ok(log) = JobLog::open(&self.log_path(id)) {
                log.line("Interrupted: the agent stopped before this job finished.");
            }
        }
        let n = self.db.conn().execute(
            "UPDATE jobs SET status='interrupted', finished_at=?1,
                 error=COALESCE(error, 'the agent stopped before this job finished')
             WHERE status IN ('queued','running')",
            [&now],
        )?;
        self.prune()?;
        Ok(n)
    }

    fn prune(&self) -> Result<()> {
        let old: Vec<String> = {
            let c = self.db.conn();
            let mut st = c.prepare(
                "SELECT id FROM jobs WHERE status NOT IN ('queued','running')
                 ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?1",
            )?;
            let rows = st.query_map([KEEP_JOBS as i64], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for id in old {
            let _ = std::fs::remove_file(self.log_path(&id));
            self.db
                .conn()
                .execute("DELETE FROM jobs WHERE id=?1", [&id])?;
        }
        Ok(())
    }

    /// Queue a job. Returns its id immediately; `body` runs in the background
    /// once the heavy permit (if any) and all locks are available.
    pub fn submit<F, Fut>(self: &Arc<Self>, spec: JobSpec, body: F) -> Result<String>
    where
        F: FnOnce(JobCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let id = uuid::Uuid::new_v4().to_string();
        self.insert(&id, &spec)?;
        let log = Arc::new(JobLog::open(&self.log_path(&id))?);
        if !spec.locks.is_empty() || spec.heavy {
            log.line(&format!(
                "Queued{}{}",
                if spec.heavy { " (heavy job)" } else { "" },
                if spec.locks.is_empty() {
                    String::new()
                } else {
                    format!(", locks: {}", spec.locks.join(", "))
                }
            ));
        }
        let token = CancellationToken::new();
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), token.clone());
        let this = self.clone();
        let job_id = id.clone();
        tokio::spawn(async move {
            this.drive(job_id.clone(), spec, log, token, body).await;
            this.cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&job_id);
        });
        Ok(id)
    }

    async fn drive<F, Fut>(
        &self,
        id: String,
        spec: JobSpec,
        log: Arc<JobLog>,
        token: CancellationToken,
        body: F,
    ) where
        F: FnOnce(JobCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        // Wait for the heavy permit (first) and the locks (all at once).
        let acquire = async {
            let permit = if spec.heavy {
                Some(self.heavy.clone().acquire_owned().await)
            } else {
                None
            };
            let guard = self.locks.acquire(&spec.locks).await;
            (permit, guard)
        };
        let held = tokio::select! {
            _ = token.cancelled() => None,
            h = acquire => Some(h),
        };
        let Some(_held) = held else {
            // `cancel()` normally records this itself; only log if we did.
            if self.finish(
                &id,
                JobStatus::Cancelled,
                Some("cancelled before it started"),
                &["queued"],
            ) {
                log.line("Cancelled before it started.");
            }
            return;
        };
        if !self.set_running(&id) {
            // Cancelled between acquiring and starting.
            return;
        }
        log.line(&format!("Started: {}", spec.title));

        let ctx = JobCtx {
            id: id.clone(),
            log: log.clone(),
        };
        // Run the body as its own task so a panic is caught and an abort
        // drops it (killing any process groups it started).
        let mut handle = tokio::spawn(body(ctx));
        let timeout = if spec.cancellable { spec.timeout } else { None };
        let sleep = async {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        let cancel = async {
            if spec.cancellable {
                token.cancelled().await
            } else {
                std::future::pending::<()>().await
            }
        };
        enum End {
            Done(std::result::Result<Result<()>, tokio::task::JoinError>),
            Cancelled,
            TimedOut,
        }
        let end = tokio::select! {
            r = &mut handle => End::Done(r),
            _ = cancel => End::Cancelled,
            _ = sleep => End::TimedOut,
        };
        let (status, error) = match end {
            End::Done(Ok(Ok(()))) => (JobStatus::Ok, None),
            End::Done(Ok(Err(e))) => (JobStatus::Failed, Some(format!("{e:#}"))),
            End::Done(Err(e)) => (JobStatus::Failed, Some(format!("internal error: {e}"))),
            End::Cancelled => {
                handle.abort();
                let _ = handle.await;
                (JobStatus::Cancelled, Some("cancelled".to_string()))
            }
            End::TimedOut => {
                handle.abort();
                let _ = handle.await;
                let secs = timeout.map(|d| d.as_secs()).unwrap_or(0);
                (JobStatus::Failed, Some(format!("timed out after {secs}s")))
            }
        };
        if status != JobStatus::Ok && self.shutting_down.load(Ordering::SeqCst) {
            log.line("The agent is stopping; this job will be marked interrupted.");
            return;
        }
        match (&status, &error) {
            (JobStatus::Ok, _) => log.line("Finished: ok"),
            (_, Some(e)) => log.line(&format!("{}: {e}", status_label(status))),
            _ => log.line(status_label(status)),
        }
        self.finish(&id, status, error.as_deref(), &["running"]);
    }

    fn insert(&self, id: &str, spec: &JobSpec) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO jobs (id, kind, title, status, heavy, locks, timeout_secs, cancellable,
                               created_at, change_id, actor_ip)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                spec.kind,
                spec.title,
                spec.heavy,
                serde_json::to_string(&spec.locks)?,
                spec.timeout.map(|d| d.as_secs() as i64),
                spec.cancellable,
                fmt_ts(Utc::now()),
                spec.change_id,
                spec.actor_ip,
            ],
        )?;
        Ok(())
    }

    fn set_running(&self, id: &str) -> bool {
        self.db
            .conn()
            .execute(
                "UPDATE jobs SET status='running', started_at=?2 WHERE id=?1 AND status='queued'",
                params![id, fmt_ts(Utc::now())],
            )
            .map(|n| n == 1)
            .unwrap_or(false)
    }

    /// Set a final status, only if the job is still in one of `from`.
    fn finish(&self, id: &str, status: JobStatus, error: Option<&str>, from: &[&str]) -> bool {
        let from = from
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE jobs SET status=?2, finished_at=?3, error=?4 WHERE id=?1 AND status IN ({from})"
        );
        match self.db.conn().execute(
            &sql,
            params![id, status.as_str(), fmt_ts(Utc::now()), error],
        ) {
            Ok(n) => n == 1,
            Err(e) => {
                tracing::error!("job {id}: cannot record status: {e}");
                false
            }
        }
    }

    /// Request cancellation. Queued jobs are cancelled at once; running jobs
    /// are stopped (process groups killed) if they are cancellable.
    pub fn cancel(&self, id: &str) -> std::result::Result<(), CancelError> {
        let job = self.row(id).ok().flatten().ok_or(CancelError::NotFound)?;
        let token = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned();
        match job.status {
            JobStatus::Queued => {
                self.finish(
                    id,
                    JobStatus::Cancelled,
                    Some("cancelled before it started"),
                    &["queued"],
                );
                if let Ok(log) = JobLog::open(&self.log_path(id)) {
                    log.line("Cancelled before it started.");
                }
                if let Some(t) = token {
                    t.cancel();
                }
                Ok(())
            }
            JobStatus::Running if !job.cancellable => Err(CancelError::NotCancellable),
            JobStatus::Running => {
                match token {
                    Some(t) => t.cancel(),
                    None => {
                        // Not driven by this process (should not happen after
                        // the startup sweep); record it as cancelled.
                        self.finish(id, JobStatus::Cancelled, Some("cancelled"), &["running"]);
                    }
                }
                Ok(())
            }
            _ => Err(CancelError::AlreadyFinished),
        }
    }

    fn row(&self, id: &str) -> Result<Option<Job>> {
        let c = self.db.conn();
        let sql = format!("SELECT {JOB_COLS} FROM jobs WHERE id=?1");
        Ok(c.query_row(&sql, [id], job_from_row).optional()?)
    }

    /// One job with (the tail of) its log.
    pub fn get(&self, id: &str) -> Result<Option<Job>> {
        Ok(self.row(id)?.map(|mut j| {
            j.log = read_log_tail(&self.log_path(&j.id), MAX_LOG_RESPONSE);
            j
        }))
    }

    /// Status only (cheap; used by the SSE follower).
    pub fn status(&self, id: &str) -> Option<(JobStatus, Option<String>)> {
        self.row(id).ok().flatten().map(|j| (j.status, j.error))
    }

    /// Newest first.
    pub fn list(&self, limit: usize) -> Result<Vec<Job>> {
        let mut jobs: Vec<Job> = {
            let c = self.db.conn();
            let sql = format!(
                "SELECT {JOB_COLS} FROM jobs ORDER BY created_at DESC, rowid DESC LIMIT ?1"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map([limit as i64], job_from_row)?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for j in &mut jobs {
            j.log = read_log_tail(&self.log_path(&j.id), MAX_LIST_LOG);
        }
        Ok(jobs)
    }

    /// Wait until the job reaches a final status (tests and the CLI).
    #[cfg(test)]
    pub async fn wait(&self, id: &str) -> JobStatus {
        loop {
            if let Some((s, _)) = self.status(id) {
                if s.is_finished() {
                    return s;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn status_label(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Ok => "Finished: ok",
        JobStatus::Failed => "FAILED",
        JobStatus::Cancelled => "Cancelled",
        JobStatus::Interrupted => "Interrupted",
        JobStatus::Queued => "Queued",
        JobStatus::Running => "Running",
    }
}

fn job_from_row(r: &Row) -> rusqlite::Result<Job> {
    let locks: String = r.get(5)?;
    Ok(Job {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        status: JobStatus::parse(&r.get::<_, String>(3)?),
        heavy: r.get(4)?,
        locks: serde_json::from_str(&locks).unwrap_or_default(),
        timeout_secs: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
        cancellable: r.get(7)?,
        created_at: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
        error: r.get(11)?,
        change_id: r.get(12)?,
        actor_ip: r.get(13)?,
        log: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn setup() -> (Arc<Jobs>, Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nexacloud-jobs-{}", uuid::Uuid::new_v4()));
        let db = Db::open_in_memory().unwrap();
        (Jobs::new(db.clone(), dir.clone()), db, dir)
    }

    #[tokio::test]
    async fn state_transitions_and_log() {
        let (jobs, _db, dir) = setup();
        let ok = jobs
            .submit(JobSpec::new("t", "ok job"), |ctx| async move {
                ctx.log("hello");
                Ok(())
            })
            .unwrap();
        let bad = jobs
            .submit(JobSpec::new("t", "bad job"), |_ctx| async move {
                anyhow::bail!("boom")
            })
            .unwrap();
        assert_eq!(jobs.wait(&ok).await, JobStatus::Ok);
        assert_eq!(jobs.wait(&bad).await, JobStatus::Failed);
        let j = jobs.get(&ok).unwrap().unwrap();
        assert!(j.log.contains("hello"));
        assert!(j.started_at.is_some() && j.finished_at.is_some());
        let b = jobs.get(&bad).unwrap().unwrap();
        assert_eq!(b.error.as_deref(), Some("boom"));
        let list = jobs.list(10).unwrap();
        assert_eq!(list.len(), 2);
        let v = serde_json::to_value(&list[0]).unwrap();
        assert!(v["status"].is_string());
        // Finished jobs cannot be cancelled.
        assert_eq!(jobs.cancel(&ok), Err(CancelError::AlreadyFinished));
        assert_eq!(jobs.cancel("nope"), Err(CancelError::NotFound));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn panic_marks_failed() {
        let (jobs, _db, dir) = setup();
        let id = jobs
            .submit(JobSpec::new("t", "panics"), |_ctx| async move {
                if true {
                    panic!("kaboom");
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(jobs.wait(&id).await, JobStatus::Failed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn interrupted_at_startup() {
        let dir = std::env::temp_dir().join(format!("nexacloud-jobs-{}", uuid::Uuid::new_v4()));
        let db = Db::open_in_memory().unwrap();
        {
            // A previous process left one running and one queued job behind.
            let jobs = Jobs::new(db.clone(), dir.clone());
            jobs.insert("a", &JobSpec::new("t", "was running")).unwrap();
            jobs.insert("b", &JobSpec::new("t", "was queued")).unwrap();
            jobs.insert("c", &JobSpec::new("t", "finished")).unwrap();
            assert!(jobs.set_running("a"));
            assert!(jobs.finish("c", JobStatus::Ok, None, &["queued"]));
        }
        let jobs = Jobs::new(db, dir.clone());
        assert_eq!(jobs.mark_interrupted_at_startup().unwrap(), 2);
        assert_eq!(jobs.status("a").unwrap().0, JobStatus::Interrupted);
        assert_eq!(jobs.status("b").unwrap().0, JobStatus::Interrupted);
        assert_eq!(jobs.status("c").unwrap().0, JobStatus::Ok);
        assert!(jobs.get("a").unwrap().unwrap().log.contains("Interrupted"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn same_lock_never_overlaps_and_other_locks_run_concurrently() {
        let (jobs, _db, dir) = setup();
        let busy = Arc::new(AtomicBool::new(false));
        let overlap = Arc::new(AtomicBool::new(false));
        let mut ids = Vec::new();
        for i in 0..4 {
            let busy = busy.clone();
            let overlap = overlap.clone();
            ids.push(
                jobs.submit(
                    JobSpec::new("t", &format!("nginx {i}")).lock("nginx"),
                    move |_ctx| async move {
                        if busy.swap(true, Ordering::SeqCst) {
                            overlap.store(true, Ordering::SeqCst);
                        }
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        busy.store(false, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .unwrap(),
            );
        }
        // A job on another resource is not blocked by the nginx queue.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let other = jobs
            .submit(JobSpec::new("t", "site"), move |_ctx| async move {
                let _ = tx.send(());
                Ok(())
            })
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), rx)
            .await
            .expect("unrelated job should start immediately")
            .unwrap();
        for id in &ids {
            assert_eq!(jobs.wait(id).await, JobStatus::Ok);
        }
        assert_eq!(jobs.wait(&other).await, JobStatus::Ok);
        assert!(!overlap.load(Ordering::SeqCst), "two nginx jobs overlapped");
        assert!(jobs.locks.held().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn heavy_jobs_run_one_at_a_time() {
        let (jobs, _db, dir) = setup();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let first = jobs
            .submit(
                JobSpec::new("t", "heavy 1").heavy(),
                move |_ctx| async move {
                    let _ = release_rx.await;
                    Ok(())
                },
            )
            .unwrap();
        let second = jobs
            .submit(JobSpec::new("t", "heavy 2").heavy(), |_ctx| async move {
                Ok(())
            })
            .unwrap();
        let light = jobs
            .submit(JobSpec::new("t", "light"), |_ctx| async move { Ok(()) })
            .unwrap();
        assert_eq!(jobs.wait(&light).await, JobStatus::Ok);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(jobs.status(&first).unwrap().0, JobStatus::Running);
        assert_eq!(jobs.status(&second).unwrap().0, JobStatus::Queued);
        // Cancel the queued heavy job; release the first.
        jobs.cancel(&second).unwrap();
        assert_eq!(jobs.status(&second).unwrap().0, JobStatus::Cancelled);
        release_tx.send(()).unwrap();
        assert_eq!(jobs.wait(&first).await, JobStatus::Ok);
        assert_eq!(jobs.wait(&second).await, JobStatus::Cancelled);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancel_running_kills_process_and_timeout_fails() {
        let (jobs, _db, dir) = setup();
        let id = jobs
            .submit(JobSpec::new("t", "sleeper"), |ctx| async move {
                let out = crate::util::cmd::Cmd::new("sleep")
                    .arg("30")
                    .timeout(Duration::from_secs(60))
                    .run_streaming(&*ctx.log)
                    .await;
                anyhow::ensure!(out.ok, "sleep failed");
                Ok(())
            })
            .unwrap();
        while jobs.status(&id).unwrap().0 != JobStatus::Running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        jobs.cancel(&id).unwrap();
        assert_eq!(jobs.wait(&id).await, JobStatus::Cancelled);

        let t = jobs
            .submit(
                JobSpec::new("t", "slow").timeout(Duration::from_millis(100)),
                |_ctx| async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(jobs.wait(&t).await, JobStatus::Failed);
        assert!(jobs.status(&t).unwrap().1.unwrap().contains("timed out"));

        // Non-cancellable jobs refuse a cancel while running.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let nc = jobs
            .submit(
                JobSpec::new("t", "apply").not_cancellable_while_running(),
                move |_ctx| async move {
                    let _ = rx.await;
                    Ok(())
                },
            )
            .unwrap();
        while jobs.status(&nc).unwrap().0 != JobStatus::Running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(jobs.cancel(&nc), Err(CancelError::NotCancellable));
        tx.send(()).unwrap();
        assert_eq!(jobs.wait(&nc).await, JobStatus::Ok);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn log_tail() {
        let p = std::env::temp_dir().join(format!("nc-log-{}", uuid::Uuid::new_v4()));
        std::fs::write(&p, "line one\nline two\nline three\n").unwrap();
        assert_eq!(read_log_tail(&p, 1000), "line one\nline two\nline three\n");
        let t = read_log_tail(&p, 12);
        assert!(t.ends_with("line three\n") && !t.contains("one"), "{t}");
        assert_eq!(read_log_tail(Path::new("/nonexistent/x"), 10), "");
        let _ = std::fs::remove_file(p);
    }
}
