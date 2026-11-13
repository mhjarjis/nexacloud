//! The change pipeline: every mutation of the server goes through here.
//!
//! A [`ChangePlan`] lists exactly what will happen — file writes/deletes
//! (with the state each file had when the plan was made), commands (argv
//! arrays, never shell strings), verify steps and `on_revert` commands.
//!
//! 1. **Plan** with [`ChangePlan::builder`] … [`ChangePlanBuilder::build`],
//!    which records each file's current hash/content/mode/owner and drops
//!    writes that would change nothing.
//! 2. **Preview** with [`preview`]: unified diffs + command list + a
//!    `plan_hash` the client can send back to apply exactly what it saw.
//! 3. **Apply** with [`submit_apply`] (a job under the plan's locks) or
//!    [`apply`] directly (CLI). Apply re-checks that no file moved since the
//!    plan, **snapshots** every touched file into
//!    `{data_dir}/snapshots/<change_id>/` (`manifest.json` + original bytes,
//!    mode, owner, or "did not exist"), writes files atomically, runs the
//!    commands, then the verify steps. On any failure it **restores the
//!    snapshot**, runs `on_revert`, and marks the change `reverted`.
//! 4. **Roll back** with [`plan_rollback`] + apply: restores an applied
//!    change's snapshot (refusing if a file drifted since, unless `force`),
//!    runs `on_revert`, and is itself recorded as a change of kind
//!    `rollback` (so it has its own snapshot and can be undone too).
//!
//! Template for later phases (see `advisor::nginx_hardening`):
//! ```ignore
//! let plan = ChangePlan::builder("php_ini", "Raise upload limit", "php8.3")
//!     .lock("php:8.3")
//!     .write_file("/etc/php/8.3/fpm/conf.d/99-nexacloud.ini", ini_text)
//!     .command(["php-fpm8.3", "-t"])
//!     .command(["systemctl", "reload", "php8.3-fpm"])
//!     .verify_http("http://127.0.0.1/", 200)
//!     .on_revert(["systemctl", "reload", "php8.3-fpm"])
//!     .build(&state.changes_env)?;
//! let (change_id, job_id) = changes::submit_apply(&state.jobs, &state.db, &state.changes_env, plan, ip)?;
//! ```

pub mod runner;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::audit::{self, AuditEntry};
use crate::db::Db;
use crate::jobs::{JobLog, JobSpec, Jobs};
use crate::util::{atomic_write, create_private_dir, fmt_ts, sha256_hex, write_private_file};

pub use runner::{validate_plan_path, ChangeEnv};

/// Default timeout for plan commands and verify steps.
pub const DEFAULT_STEP_TIMEOUT_SECS: u64 = 120;
/// Old contents larger than this are not kept in the plan (no diff shown).
const MAX_DIFF_BYTES: usize = 1024 * 1024;
/// Default number of changes returned by `GET /api/changes`.
pub const LIST_LIMIT: usize = 50;

// ------------------------------------------------------------------ content

/// File content stored in a plan: text when it is UTF-8, hex otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "data", rename_all = "lowercase")]
pub enum Content {
    Text(String),
    Hex(String),
}

impl Content {
    pub fn from_bytes(b: Vec<u8>) -> Content {
        match String::from_utf8(b) {
            Ok(s) => Content::Text(s),
            Err(e) => Content::Hex(hex::encode(e.into_bytes())),
        }
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Content::Text(s) => s.as_bytes().to_vec(),
            Content::Hex(h) => hex::decode(h).unwrap_or_default(),
        }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Hex(_) => None,
        }
    }
    pub fn sha256(&self) -> String {
        sha256_hex(&self.to_bytes())
    }
}

// ------------------------------------------------------------------ plan

/// A file's state on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    pub exists: bool,
    pub sha256: Option<String>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// One planned file write (or delete when `content` is None).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedFile {
    /// Absolute path on the server.
    pub path: String,
    /// New content; None deletes the file.
    pub content: Option<Content>,
    /// New mode; None keeps the existing mode (0644 for new files).
    pub mode: Option<u32>,
    /// New (uid, gid); None keeps the existing owner (the agent's for new files).
    pub owner: Option<(u32, u32)>,
    /// State when the plan was made.
    pub old: FileState,
    /// Content when the plan was made (for diffs; None if missing or large).
    pub old_content: Option<Content>,
    /// SHA-256 of the real new content when `content` was redacted for
    /// storage (sensitive plans); used by the rollback drift check.
    #[serde(default)]
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedCommand {
    pub argv: Vec<String>,
    pub timeout_secs: u64,
}

/// A check that must pass after the commands ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum VerifyStep {
    /// The command must exit 0.
    Command {
        argv: Vec<String>,
        timeout_secs: u64,
    },
    /// A GET to `url` must answer with `expect_status`.
    Http {
        url: String,
        expect_status: u16,
        timeout_secs: u64,
    },
}

impl VerifyStep {
    pub fn describe(&self) -> String {
        match self {
            VerifyStep::Command { argv, .. } => display_argv(argv),
            VerifyStep::Http {
                url, expect_status, ..
            } => format!("GET {url} → {expect_status}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyResult {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

/// Everything a change will do. Build with [`ChangePlan::builder`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangePlan {
    /// Machine name, e.g. `nginx_hardening`, `rollback`.
    pub kind: String,
    pub title: String,
    /// What it affects, e.g. `nginx`, `site:shop.example.com`.
    pub target: String,
    /// Job locks held while applying.
    pub locks: Vec<String>,
    pub files: Vec<PlannedFile>,
    pub commands: Vec<PlannedCommand>,
    pub verify: Vec<VerifyStep>,
    /// Run after restoring the snapshot (auto-revert and rollback).
    pub on_revert: Vec<PlannedCommand>,
    /// Human note shown in the preview (e.g. why there is nothing to do).
    pub note: Option<String>,
    /// Set on `rollback` plans: the change being rolled back.
    pub rollback_of: Option<String>,
    /// Commands run *before* files are written (e.g. `apt-get install`,
    /// `useradd`, `install -d`). Not undone by revert except via `on_revert`.
    #[serde(default)]
    pub pre_commands: Vec<PlannedCommand>,
    /// Create missing parent directories of planned files (they are left in
    /// place on revert).
    #[serde(default)]
    pub create_parents: bool,
    /// Files hold secrets (e.g. `.env`): previews and the stored record mask
    /// secret values.
    #[serde(default)]
    pub sensitive: bool,
}

struct FileSpec {
    path: String,
    content: Option<Content>,
    mode: Option<u32>,
    owner: Option<(u32, u32)>,
}

/// Builder for [`ChangePlan`].
pub struct ChangePlanBuilder {
    plan: ChangePlan,
    files: Vec<FileSpec>,
}

fn argv_of<I, S>(argv: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    argv.into_iter().map(|a| a.as_ref().to_string()).collect()
}

impl ChangePlan {
    pub fn builder(kind: &str, title: &str, target: &str) -> ChangePlanBuilder {
        ChangePlanBuilder {
            plan: ChangePlan {
                kind: kind.to_string(),
                title: title.to_string(),
                target: target.to_string(),
                locks: Vec::new(),
                files: Vec::new(),
                commands: Vec::new(),
                verify: Vec::new(),
                on_revert: Vec::new(),
                note: None,
                rollback_of: None,
                pre_commands: Vec::new(),
                create_parents: false,
                sensitive: false,
            },
            files: Vec::new(),
        }
    }

    /// No file changes and no commands.
    pub fn is_noop(&self) -> bool {
        self.files.is_empty() && self.commands.is_empty() && self.pre_commands.is_empty()
    }

    /// SHA-256 over everything that would be done (paths, old states, new
    /// content hashes, modes, owners, commands, verify, on_revert).
    pub fn hash(&self) -> String {
        #[derive(Serialize)]
        struct F<'a> {
            path: &'a str,
            old: &'a FileState,
            new: Option<String>,
            mode: Option<u32>,
            owner: Option<(u32, u32)>,
        }
        #[derive(Serialize)]
        struct H<'a> {
            kind: &'a str,
            target: &'a str,
            files: Vec<F<'a>>,
            commands: &'a [PlannedCommand],
            verify: &'a [VerifyStep],
            on_revert: &'a [PlannedCommand],
            pre_commands: &'a [PlannedCommand],
            create_parents: bool,
        }
        let h = H {
            kind: &self.kind,
            target: &self.target,
            files: self
                .files
                .iter()
                .map(|f| F {
                    path: &f.path,
                    old: &f.old,
                    new: f.content.as_ref().map(Content::sha256),
                    mode: f.mode,
                    owner: f.owner,
                })
                .collect(),
            commands: &self.commands,
            verify: &self.verify,
            on_revert: &self.on_revert,
            pre_commands: &self.pre_commands,
            create_parents: self.create_parents,
        };
        sha256_hex(&serde_json::to_vec(&h).unwrap_or_default())
    }
}

impl ChangePlanBuilder {
    /// Hold this job lock while applying (e.g. `nginx`, `site:<id>`).
    pub fn lock(mut self, name: &str) -> Self {
        if !self.plan.locks.iter().any(|l| l == name) {
            self.plan.locks.push(name.to_string());
        }
        self
    }

    /// Write a text file (mode/owner kept if it exists; 0644 if new).
    pub fn write_file(self, path: &str, content: impl Into<String>) -> Self {
        self.write_file_with(path, Content::Text(content.into()), None, None)
    }

    /// Write a file with an explicit mode and/or owner (uid, gid).
    pub fn write_file_with(
        mut self,
        path: &str,
        content: Content,
        mode: Option<u32>,
        owner: Option<(u32, u32)>,
    ) -> Self {
        self.files.push(FileSpec {
            path: path.to_string(),
            content: Some(content),
            mode,
            owner,
        });
        self
    }

    /// Delete a file (no-op if it does not exist).
    pub fn delete_file(mut self, path: &str) -> Self {
        self.files.push(FileSpec {
            path: path.to_string(),
            content: None,
            mode: None,
            owner: None,
        });
        self
    }

    /// Run a command after the files are written; it must exit 0.
    pub fn command<I, S>(self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.command_timeout(argv, DEFAULT_STEP_TIMEOUT_SECS)
    }

    pub fn command_timeout<I, S>(mut self, argv: I, timeout_secs: u64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.plan.commands.push(PlannedCommand {
            argv: argv_of(argv),
            timeout_secs,
        });
        self
    }

    /// Command run before any file is written.
    pub fn pre_command<I, S>(self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.pre_command_timeout(argv, DEFAULT_STEP_TIMEOUT_SECS)
    }

    pub fn pre_command_timeout<I, S>(mut self, argv: I, timeout_secs: u64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.plan.pre_commands.push(PlannedCommand {
            argv: argv_of(argv),
            timeout_secs,
        });
        self
    }

    /// Mask secret values (KEY/SECRET/PASSWORD/TOKEN) in previews and storage.
    pub fn sensitive(mut self) -> Self {
        self.plan.sensitive = true;
        self
    }

    /// Create missing parent directories of planned files.
    pub fn create_parents(mut self) -> Self {
        self.plan.create_parents = true;
        self
    }

    /// Verify step: the command must exit 0.
    pub fn verify_command<I, S>(mut self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.plan.verify.push(VerifyStep::Command {
            argv: argv_of(argv),
            timeout_secs: DEFAULT_STEP_TIMEOUT_SECS,
        });
        self
    }

    /// Verify step: GET `url` must answer `expect_status`.
    #[allow(dead_code)] // first used by site changes in phase 1
    pub fn verify_http(mut self, url: &str, expect_status: u16) -> Self {
        self.plan.verify.push(VerifyStep::Http {
            url: url.to_string(),
            expect_status,
            timeout_secs: 15,
        });
        self
    }

    /// Run after a snapshot restore (auto-revert or rollback).
    pub fn on_revert<I, S>(mut self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.plan.on_revert.push(PlannedCommand {
            argv: argv_of(argv),
            timeout_secs: DEFAULT_STEP_TIMEOUT_SECS,
        });
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.plan.note = Some(note.into());
        self
    }

    fn rollback_of(mut self, id: &str) -> Self {
        self.plan.rollback_of = Some(id.to_string());
        self
    }

    /// Read the current state of every file, drop writes that would change
    /// nothing, and validate paths/commands.
    pub fn build(mut self, env: &ChangeEnv) -> Result<ChangePlan> {
        for c in self
            .plan
            .commands
            .iter()
            .chain(&self.plan.on_revert)
            .chain(&self.plan.pre_commands)
        {
            if c.argv.is_empty() || c.argv[0].is_empty() {
                bail!("empty command in plan");
            }
        }
        for spec in std::mem::take(&mut self.files) {
            validate_plan_path(&spec.path).map_err(|e| anyhow!(e))?;
            if self.plan.files.iter().any(|f| f.path == spec.path) {
                bail!("{} appears twice in the plan", spec.path);
            }
            let (old, bytes) = read_current(env, &spec.path)?;
            let unchanged = match &spec.content {
                Some(c) => {
                    old.exists
                        && old.sha256.as_deref() == Some(c.sha256().as_str())
                        && spec.mode.is_none_or(|m| Some(m) == old.mode)
                        && spec
                            .owner
                            .is_none_or(|(u, g)| Some(u) == old.uid && Some(g) == old.gid)
                }
                None => !old.exists,
            };
            if unchanged {
                continue;
            }
            if spec.content.is_some() && !self.plan.create_parents {
                let parent = env.host_path(&spec.path);
                let parent = parent.parent().map(PathBuf::from).unwrap_or_default();
                if !parent.is_dir() {
                    bail!(
                        "directory for {} does not exist (NexaCloud does not create it)",
                        spec.path
                    );
                }
            }
            let old_content = bytes
                .filter(|b| b.len() <= MAX_DIFF_BYTES)
                .map(Content::from_bytes);
            self.plan.files.push(PlannedFile {
                content_sha256: None,
                path: spec.path,
                content: spec.content,
                mode: spec.mode,
                owner: spec.owner,
                old,
                old_content,
            });
        }
        Ok(self.plan)
    }
}

/// Current state (and bytes) of a plan path under `env.root`. Refuses
/// symlinks and non-regular files: we never write through them.
pub fn read_current(env: &ChangeEnv, path: &str) -> Result<(FileState, Option<Vec<u8>>)> {
    let hp = env.host_path(path);
    match std::fs::symlink_metadata(&hp) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((FileState::default(), None)),
        Err(e) => Err(anyhow!("cannot inspect {path}: {e}")),
        Ok(m) if m.file_type().is_symlink() => {
            bail!("{path} is a symlink; NexaCloud does not write through symlinks")
        }
        Ok(m) if !m.is_file() => bail!("{path} is not a regular file"),
        Ok(m) => {
            let bytes = std::fs::read(&hp).with_context(|| format!("reading {path}"))?;
            Ok((
                FileState {
                    exists: true,
                    sha256: Some(sha256_hex(&bytes)),
                    mode: Some(m.mode() & 0o7777),
                    uid: Some(m.uid()),
                    gid: Some(m.gid()),
                },
                Some(bytes),
            ))
        }
    }
}

/// `nginx -s reload` → `nginx -s reload`; args with spaces are quoted.
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains('\'') {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------ preview

#[derive(Debug, Clone, Serialize)]
pub struct FilePreview {
    pub path: String,
    /// `create` | `modify` | `delete` | `permissions`
    pub action: &'static str,
    /// Unified diff (empty for permission-only changes).
    pub diff: String,
    /// Octal mode after the change, e.g. `0644`.
    pub mode: Option<String>,
    /// `uid:gid` after the change.
    pub owner: Option<String>,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub kind: String,
    pub title: String,
    pub target: String,
    pub nothing_to_do: bool,
    pub note: Option<String>,
    pub plan_hash: String,
    pub locks: Vec<String>,
    pub files: Vec<FilePreview>,
    pub commands: Vec<Vec<String>>,
    pub verify: Vec<VerifyStep>,
    pub on_revert: Vec<Vec<String>>,
}

/// `KEY=value` lines whose key looks secret get their value masked.
pub fn redact_secrets(text: &str) -> String {
    text.lines()
        .map(|l| match l.split_once('=') {
            Some((k, _))
                if !l.trim_start().starts_with('#')
                    && ["PASSWORD", "SECRET", "KEY", "TOKEN", "PASS"]
                        .iter()
                        .any(|w| k.to_ascii_uppercase().contains(w)) =>
            {
                format!("{k}=••••••")
            }
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copy of a sensitive plan with secrets masked (for previews and storage).
pub fn redacted_plan(plan: &ChangePlan) -> ChangePlan {
    let mut p = plan.clone();
    if !p.sensitive {
        return p;
    }
    for f in p.files.iter_mut() {
        if let Some(c) = &f.content {
            f.content_sha256 = Some(c.sha256());
            if let Some(t) = c.as_text() {
                f.content = Some(Content::Text(redact_secrets(t)));
            }
        }
        if let Some(t) = f
            .old_content
            .as_ref()
            .and_then(|c| c.as_text().map(str::to_string))
        {
            f.old_content = Some(Content::Text(redact_secrets(&t)));
        }
    }
    p
}

fn file_preview(f: &PlannedFile) -> FilePreview {
    let action = match (&f.content, f.old.exists) {
        (None, _) => "delete",
        (Some(_), false) => "create",
        (Some(c), true) if f.old.sha256.as_deref() == Some(c.sha256().as_str()) => "permissions",
        (Some(_), true) => "modify",
    };
    let old_text = if !f.old.exists {
        Some(String::new())
    } else {
        f.old_content
            .as_ref()
            .and_then(|c| c.as_text().map(str::to_string))
    };
    let new_text = match &f.content {
        None => Some(String::new()),
        Some(c) => c.as_text().map(str::to_string),
    };
    let diff = match (old_text, new_text) {
        _ if action == "permissions" => String::new(),
        (Some(o), Some(n)) => {
            let a = if f.old.exists {
                format!("a{}", f.path)
            } else {
                "/dev/null".to_string()
            };
            let b = if f.content.is_some() {
                format!("b{}", f.path)
            } else {
                "/dev/null".to_string()
            };
            similar::udiff::unified_diff(similar::Algorithm::Myers, &o, &n, 3, Some((&a, &b)))
        }
        _ => format!("Binary or large file {} (no text diff)\n", f.path),
    };
    let mode = f
        .content
        .as_ref()
        .map(|_| format!("{:04o}", f.mode.or(f.old.mode).unwrap_or(0o644)));
    let owner = f
        .owner
        .or(match (f.old.uid, f.old.gid) {
            (Some(u), Some(g)) => Some((u, g)),
            _ => None,
        })
        .filter(|_| f.content.is_some())
        .map(|(u, g)| format!("{u}:{g}"));
    FilePreview {
        path: f.path.clone(),
        action,
        diff,
        mode,
        owner,
        old_sha256: f.old.sha256.clone(),
        new_sha256: f.content.as_ref().map(Content::sha256),
    }
}

/// What the plan will do, for people: diffs per file + commands.
pub fn preview(plan: &ChangePlan) -> Preview {
    Preview {
        kind: plan.kind.clone(),
        title: plan.title.clone(),
        target: plan.target.clone(),
        nothing_to_do: plan.is_noop(),
        note: plan.note.clone(),
        plan_hash: plan.hash(),
        locks: plan.locks.clone(),
        files: redacted_plan(plan).files.iter().map(file_preview).collect(),
        commands: plan
            .pre_commands
            .iter()
            .chain(&plan.commands)
            .map(|c| c.argv.clone())
            .collect(),
        verify: plan.verify.clone(),
        on_revert: plan.on_revert.iter().map(|c| c.argv.clone()).collect(),
    }
}

// ------------------------------------------------------------------ store

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    /// Waiting for its job to start.
    Pending,
    Applying,
    Applied,
    /// Failed before anything was modified.
    Failed,
    /// Failed after modifying; the snapshot was restored automatically.
    Reverted,
    /// Failed and the automatic restore failed too: needs a person.
    RevertFailed,
    /// Undone later by a `rollback` change.
    RolledBack,
    /// The agent stopped while it was pending/applying.
    Interrupted,
    /// Its job was cancelled before it started.
    Cancelled,
}

impl ChangeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeStatus::Pending => "pending",
            ChangeStatus::Applying => "applying",
            ChangeStatus::Applied => "applied",
            ChangeStatus::Failed => "failed",
            ChangeStatus::Reverted => "reverted",
            ChangeStatus::RevertFailed => "revert_failed",
            ChangeStatus::RolledBack => "rolled_back",
            ChangeStatus::Interrupted => "interrupted",
            ChangeStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> ChangeStatus {
        match s {
            "pending" => ChangeStatus::Pending,
            "applying" => ChangeStatus::Applying,
            "applied" => ChangeStatus::Applied,
            "failed" => ChangeStatus::Failed,
            "reverted" => ChangeStatus::Reverted,
            "revert_failed" => ChangeStatus::RevertFailed,
            "rolled_back" => ChangeStatus::RolledBack,
            "cancelled" => ChangeStatus::Cancelled,
            _ => ChangeStatus::Interrupted,
        }
    }
}

/// A stored change.
#[derive(Debug, Clone)]
pub struct ChangeRecord {
    pub id: String,
    pub status: ChangeStatus,
    pub plan: ChangePlan,
    pub plan_hash: String,
    pub job_id: Option<String>,
    pub actor_ip: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub verify: Vec<VerifyResult>,
    pub rolled_back_by: Option<String>,
}

const CHANGE_COLS: &str = "id, status, plan, plan_hash, job_id, actor_ip, created_at, \
                           finished_at, error, verify, rolled_back_by";

fn change_from_row(r: &Row) -> rusqlite::Result<ChangeRecord> {
    let plan: String = r.get(2)?;
    let verify: String = r.get(9)?;
    let plan: ChangePlan = serde_json::from_str(&plan).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ChangeRecord {
        id: r.get(0)?,
        status: ChangeStatus::parse(&r.get::<_, String>(1)?),
        plan,
        plan_hash: r.get(3)?,
        job_id: r.get(4)?,
        actor_ip: r.get(5)?,
        created_at: r.get(6)?,
        finished_at: r.get(7)?,
        error: r.get(8)?,
        verify: serde_json::from_str(&verify).unwrap_or_default(),
        rolled_back_by: r.get(10)?,
    })
}

fn insert_change(db: &Db, id: &str, plan: &ChangePlan, actor_ip: Option<&str>) -> Result<()> {
    db.conn().execute(
        "INSERT INTO changes (id, kind, title, target, status, plan, plan_hash, actor_ip,
                              created_at, rollback_of)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            plan.kind,
            plan.title,
            plan.target,
            serde_json::to_string(&redacted_plan(plan))?,
            plan.hash(),
            actor_ip,
            fmt_ts(Utc::now()),
            plan.rollback_of,
        ],
    )?;
    Ok(())
}

fn set_status(
    db: &Db,
    id: &str,
    status: ChangeStatus,
    error: Option<&str>,
    verify: Option<&[VerifyResult]>,
) -> Result<()> {
    let finished = !matches!(status, ChangeStatus::Pending | ChangeStatus::Applying);
    db.conn().execute(
        "UPDATE changes SET status=?2, error=?3,
             finished_at=CASE WHEN ?4 THEN ?5 ELSE finished_at END,
             verify=COALESCE(?6, verify)
         WHERE id=?1",
        params![
            id,
            status.as_str(),
            error,
            finished,
            fmt_ts(Utc::now()),
            verify.map(|v| serde_json::to_string(v).unwrap_or_else(|_| "[]".into())),
        ],
    )?;
    Ok(())
}

pub fn get_change(db: &Db, id: &str) -> Result<Option<ChangeRecord>> {
    let c = db.conn();
    let sql = format!("SELECT {CHANGE_COLS} FROM changes WHERE id=?1");
    Ok(c.query_row(&sql, [id], change_from_row).optional()?)
}

/// Newest first.
pub fn list_changes(db: &Db, limit: usize) -> Result<Vec<ChangeRecord>> {
    let c = db.conn();
    let sql =
        format!("SELECT {CHANGE_COLS} FROM changes ORDER BY created_at DESC, rowid DESC LIMIT ?1");
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map([limit as i64], change_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `serve` startup: changes whose job died with the previous process.
pub fn mark_interrupted_at_startup(db: &Db) -> Result<usize> {
    Ok(db.conn().execute(
        "UPDATE changes SET status='interrupted', finished_at=?1,
             error=COALESCE(error, 'the agent stopped before this change finished')
         WHERE status IN ('pending','applying')",
        [fmt_ts(Utc::now())],
    )?)
}

/// The job of a pending change was cancelled before it started.
pub fn mark_cancelled_if_pending(db: &Db, change_id: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE changes SET status='cancelled', finished_at=?2, error='cancelled before it started'
         WHERE id=?1 AND status='pending'",
        params![change_id, fmt_ts(Utc::now())],
    )?;
    Ok(())
}

fn set_job_id(db: &Db, change_id: &str, job_id: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE changes SET job_id=?2 WHERE id=?1",
        params![change_id, job_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ API views

/// List item for `GET /api/changes`.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub target: String,
    pub status: ChangeStatus,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub job_id: Option<String>,
    pub actor_ip: Option<String>,
    pub rollback_of: Option<String>,
    pub rolled_back_by: Option<String>,
    pub plan_hash: String,
    pub files_count: usize,
    pub commands_count: usize,
    pub can_rollback: bool,
}

/// `GET /api/changes/{id}`.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeDetail {
    #[serde(flatten)]
    pub summary: ChangeSummary,
    pub note: Option<String>,
    pub files: Vec<FilePreview>,
    pub commands: Vec<Vec<String>>,
    pub verify: Vec<VerifyStep>,
    pub on_revert: Vec<Vec<String>>,
    pub verify_results: Vec<VerifyResult>,
}

pub fn summary(env: &ChangeEnv, r: &ChangeRecord) -> ChangeSummary {
    ChangeSummary {
        id: r.id.clone(),
        kind: r.plan.kind.clone(),
        title: r.plan.title.clone(),
        target: r.plan.target.clone(),
        status: r.status,
        created_at: r.created_at.clone(),
        finished_at: r.finished_at.clone(),
        error: r.error.clone(),
        job_id: r.job_id.clone(),
        actor_ip: r.actor_ip.clone(),
        rollback_of: r.plan.rollback_of.clone(),
        rolled_back_by: r.rolled_back_by.clone(),
        plan_hash: r.plan_hash.clone(),
        files_count: r.plan.files.len(),
        commands_count: r.plan.commands.len(),
        can_rollback: can_rollback(env, r),
    }
}

pub fn detail(env: &ChangeEnv, r: &ChangeRecord) -> ChangeDetail {
    let p = preview(&r.plan);
    ChangeDetail {
        summary: summary(env, r),
        note: r.plan.note.clone(),
        files: p.files,
        commands: p.commands,
        verify: p.verify,
        on_revert: p.on_revert,
        verify_results: r.verify.clone(),
    }
}

// ------------------------------------------------------------------ snapshots

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub path: String,
    pub existed: bool,
    /// File name of the original bytes inside the snapshot dir.
    pub blob: Option<String>,
    pub sha256: Option<String>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// `{snapshots_dir}/<change_id>/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub change_id: String,
    pub created_at: String,
    pub files: Vec<SnapshotEntry>,
}

fn snapshot_dir(env: &ChangeEnv, change_id: &str) -> PathBuf {
    env.snapshots_dir.join(change_id)
}

pub fn load_manifest(env: &ChangeEnv, change_id: &str) -> Result<Manifest> {
    let p = snapshot_dir(env, change_id).join("manifest.json");
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

fn has_snapshot(env: &ChangeEnv, change_id: &str) -> bool {
    snapshot_dir(env, change_id).join("manifest.json").is_file()
}

/// Applied changes can be rolled back; so can interrupted ones that had
/// already taken their snapshot.
pub fn can_rollback(env: &ChangeEnv, r: &ChangeRecord) -> bool {
    match r.status {
        ChangeStatus::Applied => true,
        ChangeStatus::Interrupted => has_snapshot(env, &r.id),
        _ => false,
    }
}

/// Save the current state of every file the plan touches.
fn take_snapshot(env: &ChangeEnv, change_id: &str, plan: &ChangePlan) -> Result<Manifest> {
    let dir = snapshot_dir(env, change_id);
    create_private_dir(&env.snapshots_dir)?;
    create_private_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut files = Vec::new();
    for (i, f) in plan.files.iter().enumerate() {
        let (state, bytes) = read_current(env, &f.path)?;
        let blob = match &bytes {
            Some(b) => {
                let name = format!("{i}.orig");
                write_private_file(&dir.join(&name), b)
                    .with_context(|| format!("saving original of {}", f.path))?;
                Some(name)
            }
            None => None,
        };
        files.push(SnapshotEntry {
            path: f.path.clone(),
            existed: state.exists,
            blob,
            sha256: state.sha256,
            mode: state.mode,
            uid: state.uid,
            gid: state.gid,
        });
    }
    let m = Manifest {
        change_id: change_id.to_string(),
        created_at: fmt_ts(Utc::now()),
        files,
    };
    write_private_file(
        &dir.join("manifest.json"),
        serde_json::to_string_pretty(&m)?.as_bytes(),
    )?;
    Ok(m)
}

/// Put every file back as recorded in the manifest. Continues past errors
/// and reports them all.
fn restore_snapshot(env: &ChangeEnv, m: &Manifest, log: &JobLog) -> Result<()> {
    let dir = snapshot_dir(env, &m.change_id);
    let mut errors = Vec::new();
    for e in m.files.iter().rev() {
        let hp = env.host_path(&e.path);
        let res: Result<()> = if e.existed {
            (|| {
                let blob = e.blob.as_ref().context("snapshot entry without data")?;
                let bytes = std::fs::read(dir.join(blob))?;
                if Some(sha256_hex(&bytes)) != e.sha256 {
                    bail!("snapshot data for {} is corrupt", e.path);
                }
                let owner = match (e.uid, e.gid) {
                    (Some(u), Some(g)) => Some((u, g)),
                    _ => None,
                };
                atomic_write(&hp, &bytes, e.mode.unwrap_or(0o644), owner)?;
                Ok(())
            })()
        } else {
            match std::fs::remove_file(&hp) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err.into()),
            }
        };
        match res {
            Ok(()) => log.line(&format!(
                "Restored {}{}",
                e.path,
                if e.existed {
                    ""
                } else {
                    " (removed; it did not exist before)"
                }
            )),
            Err(err) => {
                log.line(&format!("ERROR restoring {}: {err:#}", e.path));
                errors.push(format!("{}: {err:#}", e.path));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("could not restore: {}", errors.join("; "))
    }
}

// ------------------------------------------------------------------ apply

async fn run_verify(env: &ChangeEnv, step: &VerifyStep, log: &JobLog) -> VerifyResult {
    let desc = step.describe();
    log.line(&format!("Verify: {desc}"));
    match step {
        VerifyStep::Command { argv, timeout_secs } => {
            let out = env
                .runner
                .run(argv, Duration::from_secs(*timeout_secs), log)
                .await;
            VerifyResult {
                step: desc,
                ok: out.ok,
                detail: if out.ok {
                    "exit 0".into()
                } else {
                    out.error_summary(&argv[0])
                },
            }
        }
        VerifyStep::Http {
            url,
            expect_status,
            timeout_secs,
        } => match env
            .runner
            .http_status(url, Duration::from_secs(*timeout_secs))
            .await
        {
            Ok(code) => VerifyResult {
                step: desc,
                ok: code == *expect_status,
                detail: format!("HTTP {code}"),
            },
            Err(e) => VerifyResult {
                step: desc,
                ok: false,
                detail: e,
            },
        },
    }
}

async fn run_commands(
    env: &ChangeEnv,
    cmds: &[PlannedCommand],
    log: &JobLog,
) -> std::result::Result<(), String> {
    for c in cmds {
        log.line(&format!("$ {}", display_argv(&c.argv)));
        let out = env
            .runner
            .run(&c.argv, Duration::from_secs(c.timeout_secs), log)
            .await;
        if !out.ok {
            return Err(format!(
                "`{}` failed{}: {}",
                display_argv(&c.argv),
                out.exit_code
                    .map(|c| format!(" (exit {c})"))
                    .unwrap_or_default(),
                out.error_summary(&c.argv[0])
            ));
        }
    }
    Ok(())
}

/// Write files, run commands, run verify steps. Err = reason to revert.
async fn execute(
    env: &ChangeEnv,
    plan: &ChangePlan,
    log: &JobLog,
    results: &mut Vec<VerifyResult>,
) -> std::result::Result<(), String> {
    run_commands(env, &plan.pre_commands, log).await?;
    for f in &plan.files {
        let hp = env.host_path(&f.path);
        match &f.content {
            Some(c) => {
                if plan.create_parents {
                    if let Some(parent) = hp.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("creating directory for {}: {e}", f.path))?;
                    }
                }
                let bytes = c.to_bytes();
                let mode = f.mode.or(f.old.mode).unwrap_or(0o644);
                let owner = f.owner.or(match (f.old.uid, f.old.gid) {
                    (Some(u), Some(g)) if f.old.exists => Some((u, g)),
                    _ => None,
                });
                atomic_write(&hp, &bytes, mode, owner)
                    .map_err(|e| format!("writing {}: {e}", f.path))?;
                log.line(&format!(
                    "Wrote {} ({} bytes, mode {mode:04o})",
                    f.path,
                    bytes.len()
                ));
            }
            None => match std::fs::remove_file(&hp) {
                Ok(()) => log.line(&format!("Removed {}", f.path)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("removing {}: {e}", f.path)),
            },
        }
    }
    run_commands(env, &plan.commands, log).await?;
    for step in &plan.verify {
        let r = run_verify(env, step, log).await;
        log.line(&format!(
            "  {} {}",
            if r.ok { "ok:" } else { "FAILED:" },
            r.detail
        ));
        let failed = !r.ok;
        let reason = format!("verify failed: {} ({})", r.step, r.detail);
        results.push(r);
        if failed {
            return Err(reason);
        }
    }
    Ok(())
}

fn record_outcome(
    db: &Db,
    change_id: &str,
    plan: &ChangePlan,
    status: ChangeStatus,
    reason: Option<&str>,
) {
    let job_id = get_change(db, change_id)
        .ok()
        .flatten()
        .and_then(|r| r.job_id);
    audit::record(
        db,
        &AuditEntry {
            ip: None,
            action: format!("change.{}", status.as_str()),
            target: Some(plan.target.clone()),
            change_id: Some(change_id.to_string()),
            job_id,
            plan_hash: Some(plan.hash()),
            result: match reason {
                None => "ok".into(),
                Some(r) => r.to_string(),
            },
            ..Default::default()
        },
    );
}

/// Apply a stored change (status must be `pending`). Snapshot → write →
/// commands → verify; any failure restores the snapshot and runs
/// `on_revert`. Returns Err with the reason when the change did not stick.
pub async fn apply(
    env: &ChangeEnv,
    db: &Db,
    change_id: &str,
    plan: &ChangePlan,
    log: &JobLog,
) -> Result<()> {
    set_status(db, change_id, ChangeStatus::Applying, None, None)?;
    log.line(&format!("Change {change_id}: {}", plan.title));

    let fail = |status: ChangeStatus, reason: String| -> Result<()> {
        log.line(&format!("FAILED: {reason}"));
        set_status(db, change_id, status, Some(&reason), None)?;
        record_outcome(db, change_id, plan, status, Some(&reason));
        Err(anyhow!(reason))
    };

    // 1. Nothing may have moved since the plan was made.
    for f in &plan.files {
        match read_current(env, &f.path) {
            Ok((cur, _)) if cur.exists == f.old.exists && cur.sha256 == f.old.sha256 => {}
            Ok(_) => {
                return fail(
                    ChangeStatus::Failed,
                    format!(
                        "{} changed since this change was planned; preview it again",
                        f.path
                    ),
                )
            }
            Err(e) => return fail(ChangeStatus::Failed, format!("{e:#}")),
        }
    }

    // 2. Snapshot.
    let manifest = match take_snapshot(env, change_id, plan) {
        Ok(m) => {
            log.line(&format!(
                "Snapshot saved ({} file{})",
                m.files.len(),
                if m.files.len() == 1 { "" } else { "s" }
            ));
            m
        }
        Err(e) => return fail(ChangeStatus::Failed, format!("snapshot failed: {e:#}")),
    };

    // 3–5. Write, run, verify.
    let mut results = Vec::new();
    match execute(env, plan, log, &mut results).await {
        Ok(()) => {
            set_status(db, change_id, ChangeStatus::Applied, None, Some(&results))?;
            if let Some(orig) = &plan.rollback_of {
                db.conn().execute(
                    "UPDATE changes SET status='rolled_back', rolled_back_by=?2 WHERE id=?1",
                    params![orig, change_id],
                )?;
            }
            record_outcome(db, change_id, plan, ChangeStatus::Applied, None);
            log.line("Applied.");
            Ok(())
        }
        Err(reason) => {
            log.line(&format!("FAILED: {reason}"));
            log.line("Restoring the snapshot…");
            let restored = restore_snapshot(env, &manifest, log);
            let warnings = if restored.is_ok() {
                run_commands(env, &plan.on_revert, log).await.err()
            } else {
                None
            };
            let (status, msg) = match restored {
                Ok(()) => (
                    ChangeStatus::Reverted,
                    match warnings {
                        None => reason.clone(),
                        Some(w) => format!("{reason}; after restoring: {w}"),
                    },
                ),
                Err(e) => (
                    ChangeStatus::RevertFailed,
                    format!("{reason}; automatic restore FAILED: {e:#}"),
                ),
            };
            set_status(db, change_id, status, Some(&msg), Some(&results))?;
            record_outcome(db, change_id, plan, status, Some(&msg));
            if status == ChangeStatus::Reverted {
                log.line("Reverted: the server is back to how it was before this change.");
                Err(anyhow!("{reason} (changes were reverted)"))
            } else {
                log.line("Automatic restore failed: check the files listed above.");
                Err(anyhow!(msg))
            }
        }
    }
}

/// Store the plan and queue a job (under the plan's locks) that applies it.
/// Returns `(change_id, job_id)`.
pub fn submit_apply(
    jobs: &Arc<Jobs>,
    db: &Db,
    env: &ChangeEnv,
    plan: ChangePlan,
    actor_ip: Option<String>,
) -> Result<(String, String)> {
    if plan.is_noop() {
        bail!("nothing to do");
    }
    let change_id = uuid::Uuid::new_v4().to_string();
    insert_change(db, &change_id, &plan, actor_ip.as_deref())?;
    let kind = if plan.rollback_of.is_some() {
        "change_rollback"
    } else {
        "change_apply"
    };
    let spec = JobSpec::new(kind, &plan.title)
        .locks(plan.locks.clone())
        .change(&change_id)
        .actor(actor_ip)
        .not_cancellable_while_running();
    let (env2, db2, id2) = (env.clone(), db.clone(), change_id.clone());
    let job_id = jobs.submit(spec, move |ctx| async move {
        apply(&env2, &db2, &id2, &plan, &ctx.log).await
    })?;
    set_job_id(db, &change_id, &job_id)?;
    Ok((change_id, job_id))
}

/// Store and apply right now, without the job queue (CLI). Returns the change id.
pub async fn apply_now(
    db: &Db,
    env: &ChangeEnv,
    plan: ChangePlan,
    actor: &str,
    log: &JobLog,
) -> Result<String> {
    let change_id = uuid::Uuid::new_v4().to_string();
    insert_change(db, &change_id, &plan, Some(actor))?;
    apply(env, db, &change_id, &plan, log).await?;
    Ok(change_id)
}

// ------------------------------------------------------------------ rollback

#[derive(Debug)]
pub enum RollbackError {
    NotFound,
    NotRollbackable(String),
    /// Files changed since the change was applied (paths).
    Drift(Vec<String>),
    Other(anyhow::Error),
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RollbackError::NotFound => write!(f, "change not found"),
            RollbackError::NotRollbackable(s) => write!(f, "{s}"),
            RollbackError::Drift(paths) => write!(
                f,
                "these files changed since the change was applied: {} (roll back with force to overwrite them)",
                paths.join(", ")
            ),
            RollbackError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<anyhow::Error> for RollbackError {
    fn from(e: anyhow::Error) -> Self {
        RollbackError::Other(e)
    }
}

/// Build the plan that undoes `change_id`: restore every file from its
/// snapshot, then run the original `on_revert` and verify steps. Refuses on
/// drift (a file no longer matches what the change wrote) unless `force`.
pub fn plan_rollback(
    env: &ChangeEnv,
    db: &Db,
    change_id: &str,
    force: bool,
) -> std::result::Result<ChangePlan, RollbackError> {
    let rec = get_change(db, change_id)?.ok_or(RollbackError::NotFound)?;
    if !can_rollback(env, &rec) {
        return Err(RollbackError::NotRollbackable(format!(
            "this change is {}; only applied changes can be rolled back",
            rec.status.as_str().replace('_', " ")
        )));
    }
    let manifest = load_manifest(env, change_id)?;
    let mut drift = Vec::new();
    for f in &rec.plan.files {
        let (cur, _) = read_current(env, &f.path)?;
        let expected = f
            .content_sha256
            .clone()
            .or_else(|| f.content.as_ref().map(Content::sha256));
        let ok = cur.sha256 == expected
            || (rec.status == ChangeStatus::Interrupted && cur.sha256 == f.old.sha256);
        if !ok {
            drift.push(f.path.clone());
        }
    }
    if !drift.is_empty() && !force {
        return Err(RollbackError::Drift(drift));
    }
    let dir = snapshot_dir(env, change_id);
    let mut b = ChangePlan::builder(
        "rollback",
        &format!("Roll back: {}", rec.plan.title),
        &rec.plan.target,
    )
    .rollback_of(change_id);
    for l in &rec.plan.locks {
        b = b.lock(l);
    }
    for e in &manifest.files {
        if e.existed {
            let blob = e
                .blob
                .as_ref()
                .ok_or_else(|| anyhow!("snapshot entry for {} has no data", e.path))?;
            let bytes = std::fs::read(dir.join(blob))
                .with_context(|| format!("reading snapshot of {}", e.path))?;
            let owner = match (e.uid, e.gid) {
                (Some(u), Some(g)) => Some((u, g)),
                _ => None,
            };
            b = b.write_file_with(&e.path, Content::from_bytes(bytes), e.mode, owner);
        } else {
            b = b.delete_file(&e.path);
        }
    }
    for c in &rec.plan.on_revert {
        b = b.command_timeout(&c.argv, c.timeout_secs);
        b.plan.on_revert.push(c.clone());
    }
    b.plan.verify = rec.plan.verify.clone();
    if force && !drift.is_empty() {
        b = b.note(format!(
            "Forced: overwrites later edits to {}",
            drift.join(", ")
        ));
    }
    Ok(b.build(env)?)
}

/// Most recent change (not itself a rollback) that can be rolled back.
pub fn last_rollbackable(env: &ChangeEnv, db: &Db) -> Result<Option<ChangeRecord>> {
    Ok(list_changes(db, 500)?
        .into_iter()
        .find(|r| r.plan.kind != "rollback" && can_rollback(env, r)))
}

#[cfg(test)]
mod tests {
    use super::runner::fake::FakeRunner;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct T {
        env: ChangeEnv,
        db: Db,
        fake: Arc<FakeRunner>,
        root: PathBuf,
    }

    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn setup() -> T {
        let root = std::env::temp_dir().join(format!("nexacloud-chg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("fs/etc/nginx/conf.d")).unwrap();
        let fake = Arc::new(FakeRunner::default());
        let env = ChangeEnv {
            root: root.join("fs"),
            snapshots_dir: root.join("snapshots"),
            runner: fake.clone(),
        };
        T {
            env,
            db: Db::open_in_memory().unwrap(),
            fake,
            root,
        }
    }

    const DROPIN: &str = "/etc/nginx/conf.d/zz-test.conf";

    fn plan(t: &T, content: &str) -> ChangePlan {
        ChangePlan::builder("test", "Test change", "nginx")
            .lock("nginx")
            .write_file(DROPIN, content)
            .command(["nginx", "-t"])
            .command(["systemctl", "reload", "nginx"])
            .verify_command(["systemctl", "is-active", "nginx"])
            .on_revert(["systemctl", "reload", "nginx"])
            .build(&t.env)
            .unwrap()
    }

    fn host(t: &T, p: &str) -> PathBuf {
        t.env.host_path(p)
    }

    fn status(t: &T, id: &str) -> ChangeStatus {
        get_change(&t.db, id).unwrap().unwrap().status
    }

    #[test]
    fn preview_shows_diff_and_noop() {
        let t = setup();
        let p = plan(&t, "server_tokens off;\n");
        let pv = preview(&p);
        assert!(!pv.nothing_to_do);
        assert_eq!(pv.files.len(), 1);
        assert_eq!(pv.files[0].action, "create");
        assert!(
            pv.files[0].diff.contains("--- /dev/null"),
            "{}",
            pv.files[0].diff
        );
        assert!(pv.files[0].diff.contains("+server_tokens off;"));
        assert_eq!(pv.files[0].mode.as_deref(), Some("0644"));
        assert_eq!(pv.commands[0], vec!["nginx", "-t"]);
        assert_eq!(pv.plan_hash, p.hash());
        assert_ne!(p.hash(), plan(&t, "server_tokens on;\n").hash());

        // Same content already on disk → the write is dropped.
        std::fs::write(host(&t, DROPIN), "server_tokens off;\n").unwrap();
        let p2 = ChangePlan::builder("test", "t", "nginx")
            .write_file(DROPIN, "server_tokens off;\n")
            .build(&t.env)
            .unwrap();
        assert!(p2.files.is_empty() && preview(&p2).nothing_to_do);

        // Modify shows - and + lines.
        let p3 = plan(&t, "server_tokens on;\n");
        let d = &preview(&p3).files[0];
        assert_eq!(d.action, "modify");
        assert!(d.diff.contains("-server_tokens off;") && d.diff.contains("+server_tokens on;"));

        // Bad paths are refused.
        assert!(ChangePlan::builder("t", "t", "t")
            .write_file("/etc/../passwd", "x")
            .build(&t.env)
            .is_err());
        assert!(ChangePlan::builder("t", "t", "t")
            .write_file("/nonexistent-dir/x.conf", "x")
            .build(&t.env)
            .is_err());
    }

    #[tokio::test]
    async fn apply_success_snapshots_and_writes() {
        let t = setup();
        let p = plan(&t, "server_tokens off;\n");
        let id = apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .unwrap();
        assert_eq!(status(&t, &id), ChangeStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(host(&t, DROPIN)).unwrap(),
            "server_tokens off;\n"
        );
        let m = load_manifest(&t.env, &id).unwrap();
        assert_eq!(m.files.len(), 1);
        assert!(!m.files[0].existed);
        assert_eq!(
            t.fake.calls(),
            vec![
                "nginx -t",
                "systemctl reload nginx",
                "systemctl is-active nginx"
            ]
        );
        let rec = get_change(&t.db, &id).unwrap().unwrap();
        assert!(rec.verify[0].ok);
        assert!(summary(&t.env, &rec).can_rollback);
        // Audit has the outcome.
        let a = audit::list(&t.db, 10).unwrap();
        assert_eq!(a[0].action, "change.applied");
        assert_eq!(a[0].change_id.as_deref(), Some(id.as_str()));
    }

    #[tokio::test]
    async fn verify_failure_auto_reverts_existing_file() {
        let t = setup();
        let path = host(&t, DROPIN);
        std::fs::write(&path, "original\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        t.fake.script("systemctl is-active nginx", 3, "");
        let p = plan(&t, "broken;\n");
        let err = apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("verify failed"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        let calls = t.fake.calls();
        assert_eq!(
            calls.last().unwrap(),
            "systemctl reload nginx",
            "on_revert ran"
        );
        let rec = list_changes(&t.db, 1).unwrap().remove(0);
        assert_eq!(rec.status, ChangeStatus::Reverted);
        assert!(rec.error.as_deref().unwrap().contains("verify failed"));
        assert!(!rec.verify[0].ok);
        assert!(!can_rollback(&t.env, &rec));
    }

    #[tokio::test]
    async fn command_failure_auto_reverts_new_file() {
        let t = setup();
        t.fake.script("nginx -t", 1, "");
        let p = plan(&t, "garbage\n");
        assert!(apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .is_err());
        assert!(!host(&t, DROPIN).exists(), "created file removed on revert");
        // nginx -t failed → reload never ran; on_revert reload ran after restore.
        assert_eq!(t.fake.calls(), vec!["nginx -t", "systemctl reload nginx"]);
        assert_eq!(
            list_changes(&t.db, 1).unwrap()[0].status,
            ChangeStatus::Reverted
        );
    }

    #[tokio::test]
    async fn http_verify_step() {
        let t = setup();
        t.fake
            .http
            .lock()
            .unwrap()
            .insert("http://127.0.0.1/".into(), 502);
        let p = ChangePlan::builder("test", "t", "site")
            .write_file(DROPIN, "x\n")
            .verify_http("http://127.0.0.1/", 200)
            .build(&t.env)
            .unwrap();
        assert!(apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .is_err());
        let rec = list_changes(&t.db, 1).unwrap().remove(0);
        assert_eq!(rec.status, ChangeStatus::Reverted);
        assert_eq!(rec.verify[0].detail, "HTTP 502");
        assert!(!host(&t, DROPIN).exists());
    }

    #[tokio::test]
    async fn file_changed_after_plan_fails_without_touching() {
        let t = setup();
        let p = plan(&t, "new\n");
        std::fs::write(host(&t, DROPIN), "someone else\n").unwrap();
        let err = apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed since"), "{err}");
        assert_eq!(
            std::fs::read_to_string(host(&t, DROPIN)).unwrap(),
            "someone else\n"
        );
        assert_eq!(
            list_changes(&t.db, 1).unwrap()[0].status,
            ChangeStatus::Failed
        );
        assert!(t.fake.calls().is_empty());
    }

    #[tokio::test]
    async fn rollback_restores_and_records_a_rollback_change() {
        let t = setup();
        let other = "/etc/nginx/conf.d/existing.conf";
        std::fs::write(host(&t, other), "keep me\n").unwrap();
        let p = ChangePlan::builder("test", "Two files", "nginx")
            .lock("nginx")
            .write_file(DROPIN, "server_tokens off;\n")
            .write_file(other, "changed\n")
            .on_revert(["systemctl", "reload", "nginx"])
            .build(&t.env)
            .unwrap();
        let id = apply_now(&t.db, &t.env, p, "cli", &JobLog::sink())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(host(&t, other)).unwrap(),
            "changed\n"
        );

        let rb = plan_rollback(&t.env, &t.db, &id, false).unwrap();
        assert_eq!(rb.kind, "rollback");
        assert_eq!(rb.rollback_of.as_deref(), Some(id.as_str()));
        assert_eq!(rb.locks, vec!["nginx"]);
        let pv = preview(&rb);
        assert!(pv.files.iter().any(|f| f.action == "delete"));
        let rb_id = apply_now(&t.db, &t.env, rb, "cli", &JobLog::sink())
            .await
            .unwrap();

        assert!(!host(&t, DROPIN).exists());
        assert_eq!(
            std::fs::read_to_string(host(&t, other)).unwrap(),
            "keep me\n"
        );
        assert_eq!(t.fake.calls().last().unwrap(), "systemctl reload nginx");
        let orig = get_change(&t.db, &id).unwrap().unwrap();
        assert_eq!(orig.status, ChangeStatus::RolledBack);
        assert_eq!(orig.rolled_back_by.as_deref(), Some(rb_id.as_str()));
        assert!(!can_rollback(&t.env, &orig));
        // Rolling back twice is refused.
        assert!(matches!(
            plan_rollback(&t.env, &t.db, &id, false),
            Err(RollbackError::NotRollbackable(_))
        ));
        // `--last` skips rollbacks and rolled-back changes.
        assert!(last_rollbackable(&t.env, &t.db).unwrap().is_none());
        // The rollback itself is an applied change with its own snapshot.
        assert_eq!(status(&t, &rb_id), ChangeStatus::Applied);
        assert!(has_snapshot(&t.env, &rb_id));
    }

    #[tokio::test]
    async fn rollback_refuses_drift_unless_forced() {
        let t = setup();
        let id = apply_now(&t.db, &t.env, plan(&t, "mine\n"), "cli", &JobLog::sink())
            .await
            .unwrap();
        assert_eq!(last_rollbackable(&t.env, &t.db).unwrap().unwrap().id, id);
        std::fs::write(host(&t, DROPIN), "edited by hand\n").unwrap();
        match plan_rollback(&t.env, &t.db, &id, false) {
            Err(RollbackError::Drift(p)) => assert_eq!(p, vec![DROPIN.to_string()]),
            other => panic!("expected drift, got {other:?}"),
        }
        assert!(host(&t, DROPIN).exists());
        let rb = plan_rollback(&t.env, &t.db, &id, true).unwrap();
        assert!(rb.note.unwrap().contains("Forced"));
        let rb = plan_rollback(&t.env, &t.db, &id, true).unwrap();
        apply_now(&t.db, &t.env, rb, "cli", &JobLog::sink())
            .await
            .unwrap();
        assert!(!host(&t, DROPIN).exists());
        assert_eq!(status(&t, &id), ChangeStatus::RolledBack);
        assert!(matches!(
            plan_rollback(&t.env, &t.db, "nope", false),
            Err(RollbackError::NotFound)
        ));
    }

    #[tokio::test]
    async fn submit_apply_runs_as_a_locked_job() {
        let t = setup();
        let jobs = Jobs::new(t.db.clone(), t.root.join("jobs"));
        let (cid, jid) = submit_apply(
            &jobs,
            &t.db,
            &t.env,
            plan(&t, "a\n"),
            Some("203.0.113.9".into()),
        )
        .unwrap();
        assert_eq!(jobs.wait(&jid).await, crate::jobs::JobStatus::Ok);
        let rec = get_change(&t.db, &cid).unwrap().unwrap();
        assert_eq!(rec.status, ChangeStatus::Applied);
        assert_eq!(rec.job_id.as_deref(), Some(jid.as_str()));
        assert_eq!(rec.actor_ip.as_deref(), Some("203.0.113.9"));
        let job = jobs.get(&jid).unwrap().unwrap();
        assert_eq!(job.locks, vec!["nginx"]);
        assert!(!job.cancellable);
        assert_eq!(job.change_id.as_deref(), Some(cid.as_str()));
        assert!(job.log.contains("Snapshot saved"));

        // A failing change fails its job too.
        t.fake.script("nginx -t", 1, "");
        let (cid2, jid2) = submit_apply(&jobs, &t.db, &t.env, plan(&t, "b\n"), None).unwrap();
        assert_eq!(jobs.wait(&jid2).await, crate::jobs::JobStatus::Failed);
        assert_eq!(status(&t, &cid2), ChangeStatus::Reverted);
        assert_eq!(std::fs::read_to_string(host(&t, DROPIN)).unwrap(), "a\n");
    }

    #[test]
    fn interrupted_changes_at_startup() {
        let t = setup();
        let p = plan(&t, "x\n");
        insert_change(&t.db, "c1", &p, None).unwrap();
        insert_change(&t.db, "c2", &p, None).unwrap();
        set_status(&t.db, "c2", ChangeStatus::Applying, None, None).unwrap();
        assert_eq!(mark_interrupted_at_startup(&t.db).unwrap(), 2);
        assert_eq!(status(&t, "c1"), ChangeStatus::Interrupted);
        // No snapshot → nothing to roll back.
        let rec = get_change(&t.db, "c2").unwrap().unwrap();
        assert!(!can_rollback(&t.env, &rec));
    }

    #[test]
    fn content_roundtrip_and_argv_display() {
        let bin = vec![0xff, 0x00, 0x41];
        let c = Content::from_bytes(bin.clone());
        assert!(matches!(c, Content::Hex(_)));
        assert_eq!(c.to_bytes(), bin);
        let j = serde_json::to_string(&Content::Text("a".into())).unwrap();
        assert_eq!(j, r#"{"encoding":"text","data":"a"}"#);
        assert_eq!(display_argv(&["echo".into(), "a b".into()]), "echo 'a b'");
    }
}
