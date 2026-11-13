//! `nexacloud rollback` and `nexacloud doctor`. Both work with the service
//! stopped: they open the same state database and snapshot directory.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::audit::{self, AuditEntry};
use crate::changes::{self, ChangeEnv, ChangeStatus, RollbackError};
use crate::config::Config;
use crate::db::{Db, SCHEMA_VERSION};
use crate::jobs::JobLog;
use crate::util::cmd::Cmd;

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting runtime")
}

async fn service_active() -> Option<bool> {
    let out = Cmd::new("systemctl")
        .args(["is-active", "nexacloud"])
        .timeout(Duration::from_secs(10))
        .run()
        .await;
    if out.missing {
        None
    } else {
        Some(out.stdout.trim() == "active")
    }
}

// ------------------------------------------------------------------ rollback

/// `nexacloud rollback (--last | <change_id>) [--force]`.
pub fn rollback(config: &Path, id: Option<String>, last: bool, force: bool) -> Result<()> {
    let cfg = Config::load(config)?;
    cfg.ensure_data_dirs()?;
    let db = Db::open(&cfg.state_db_path())?;
    let env = ChangeEnv::system(cfg.snapshots_dir());
    runtime()?.block_on(async move {
        let id = match (id, last) {
            (Some(id), false) => id,
            (None, true) => match changes::last_rollbackable(&env, &db)? {
                Some(r) => {
                    println!("Last applied change: {} — {} ({})", r.id, r.plan.title, r.created_at);
                    r.id
                }
                None => bail!("there is no applied change to roll back"),
            },
            _ => bail!("give either --last or a change id"),
        };
        if service_active().await == Some(true) {
            eprintln!("note: the nexacloud service is running; the rollback runs here, outside its job queue.");
        }
        let plan = changes::plan_rollback(&env, &db, &id, force).map_err(|e| match e {
            RollbackError::Drift(_) => anyhow::anyhow!("{e}\nRe-run with --force to overwrite those edits."),
            other => anyhow::anyhow!("{other}"),
        })?;
        let pv = changes::preview(&plan);
        println!("{}", plan.title);
        for f in &pv.files {
            println!("  {} {}", f.action, f.path);
        }
        for c in &pv.commands {
            println!("  run {}", changes::display_argv(c));
        }
        let hash = plan.hash();
        let log = JobLog::stdout();
        let res = changes::apply_now(&db, &env, plan, "cli", &log).await;
        audit::record(
            &db,
            &AuditEntry {
                ip: Some("cli".into()),
                action: "change.rollback".into(),
                target: Some(id.clone()),
                change_id: Some(id.clone()),
                plan_hash: Some(hash),
                result: match &res {
                    Ok(_) => "ok".into(),
                    Err(e) => format!("{e:#}"),
                },
                ..Default::default()
            },
        );
        let rb = res?;
        println!("Rolled back {id} (rollback recorded as change {rb}).");
        Ok(())
    })
}

// ------------------------------------------------------------------ doctor

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Report {
    rows: Vec<(Level, String)>,
}

impl Report {
    fn add(&mut self, level: Level, msg: impl Into<String>) {
        self.rows.push((level, msg.into()));
    }
    fn print(&self) {
        for (l, m) in &self.rows {
            let tag = match l {
                Level::Ok => "  ok  ",
                Level::Info => "  info",
                Level::Warn => "  WARN",
                Level::Fail => "  FAIL",
            };
            println!("{tag}  {m}");
        }
    }
    fn failures(&self) -> usize {
        self.rows.iter().filter(|(l, _)| *l == Level::Fail).count()
    }
    fn warnings(&self) -> usize {
        self.rows.iter().filter(|(l, _)| *l == Level::Warn).count()
    }
}

fn is_root() -> bool {
    // SAFETY: plain getter syscall.
    unsafe { libc::geteuid() == 0 }
}

fn check_private_file(r: &mut Report, what: &str, p: &Path) {
    match std::fs::metadata(p) {
        Err(e) => r.add(Level::Fail, format!("{what} {}: {e}", p.display())),
        Ok(m) => {
            let mode = m.mode() & 0o777;
            if mode & 0o077 != 0 {
                r.add(
                    Level::Fail,
                    format!(
                        "{what} {} is mode {mode:03o}; others can read it (run: chmod 600 {})",
                        p.display(),
                        p.display()
                    ),
                );
            } else if is_root() && m.uid() != 0 {
                r.add(
                    Level::Fail,
                    format!(
                        "{what} {} is not owned by root (uid {})",
                        p.display(),
                        m.uid()
                    ),
                );
            } else {
                r.add(
                    Level::Ok,
                    format!("{what} {} (mode {mode:03o})", p.display()),
                );
            }
        }
    }
}

/// (total, available) bytes of the filesystem holding `p`.
#[allow(clippy::unnecessary_cast)] // statvfs field widths differ per platform
fn fs_space(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter; c is NUL-terminated.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    Some((s.f_blocks as u64 * frsize, s.f_bavail as u64 * frsize))
}

/// `nexacloud doctor`: readable health report; exit 1 when something fails.
pub fn doctor(config: &Path) -> ExitCode {
    let mut r = Report { rows: Vec::new() };
    println!(
        "NexaCloud doctor (nexacloud {})\n",
        env!("CARGO_PKG_VERSION")
    );

    check_private_file(&mut r, "config", config);
    let cfg = match Config::load(config) {
        Ok(c) => {
            r.add(Level::Ok, "config parses and has a valid password hash");
            Some(c)
        }
        Err(e) => {
            r.add(Level::Fail, format!("config: {e:#}"));
            None
        }
    };

    if let Some(cfg) = &cfg {
        if cfg.tls {
            if let Some(k) = &cfg.tls_key {
                check_private_file(&mut r, "TLS key", k);
            }
            if let Some(c) = &cfg.tls_cert {
                if c.is_file() {
                    r.add(Level::Ok, format!("TLS certificate {}", c.display()));
                } else {
                    r.add(
                        Level::Fail,
                        format!("TLS certificate {} is missing", c.display()),
                    );
                }
            }
        } else {
            r.add(
                Level::Warn,
                "TLS is disabled (tls = false): development only",
            );
        }

        match std::fs::metadata(&cfg.data_dir) {
            Ok(m) if m.is_dir() => {
                let mode = m.mode() & 0o777;
                if mode & 0o077 != 0 {
                    r.add(
                        Level::Fail,
                        format!(
                            "data dir {} is mode {mode:03o} (should be 700)",
                            cfg.data_dir.display()
                        ),
                    );
                } else {
                    r.add(
                        Level::Ok,
                        format!("data dir {} (mode {mode:03o})", cfg.data_dir.display()),
                    );
                }
            }
            Ok(_) => r.add(
                Level::Fail,
                format!("data dir {} is not a directory", cfg.data_dir.display()),
            ),
            Err(e) => r.add(
                Level::Fail,
                format!("data dir {}: {e}", cfg.data_dir.display()),
            ),
        }

        let db_path = cfg.state_db_path();
        if !db_path.exists() {
            r.add(
                Level::Warn,
                format!(
                    "state database {} does not exist yet (created when the agent starts)",
                    db_path.display()
                ),
            );
        } else {
            match Db::open(&db_path) {
                Err(e) => r.add(
                    Level::Fail,
                    format!("state database {}: {e:#}", db_path.display()),
                ),
                Ok(db) => {
                    match db.integrity_check() {
                        Ok(()) => r.add(
                            Level::Ok,
                            format!(
                                "state database {}: integrity ok, schema v{}",
                                db_path.display(),
                                db.schema_version().unwrap_or(0)
                            ),
                        ),
                        Err(e) => {
                            r.add(Level::Fail, format!("state database integrity check: {e}"))
                        }
                    }
                    if db.schema_version().unwrap_or(0) != SCHEMA_VERSION {
                        r.add(
                            Level::Fail,
                            "state database schema version does not match this binary",
                        );
                    }
                    let env = ChangeEnv::system(cfg.snapshots_dir());
                    if let Ok(list) = changes::list_changes(&db, 500) {
                        let broken: Vec<_> = list
                            .iter()
                            .filter(|c| c.status == ChangeStatus::RevertFailed)
                            .collect();
                        for c in &broken {
                            r.add(
                                Level::Fail,
                                format!(
                                    "change {} ({}) failed AND could not be reverted automatically: {}",
                                    c.id,
                                    c.plan.title,
                                    c.error.as_deref().unwrap_or("")
                                ),
                            );
                        }
                        let interrupted = list
                            .iter()
                            .filter(|c| {
                                c.status == ChangeStatus::Interrupted
                                    && changes::can_rollback(&env, c)
                            })
                            .count();
                        if interrupted > 0 {
                            r.add(
                                Level::Warn,
                                format!("{interrupted} change(s) were interrupted mid-apply; review them on the Activity page (they can be rolled back)"),
                            );
                        }
                        let applied = list
                            .iter()
                            .filter(|c| c.status == ChangeStatus::Applied)
                            .count();
                        r.add(
                            Level::Info,
                            format!("{} change(s) recorded, {applied} applied", list.len()),
                        );
                    }
                }
            }
        }

        let mut seen = Vec::new();
        for p in [
            Path::new("/"),
            cfg.data_dir.as_path(),
            cfg.backup_dir.as_path(),
        ] {
            let Some((total, avail)) = fs_space(p) else {
                continue;
            };
            if total == 0 || seen.contains(&(total, avail)) {
                continue;
            }
            seen.push((total, avail));
            let used_pct = 100.0 - (avail as f64 / total as f64 * 100.0);
            let gib = avail as f64 / 1024.0 / 1024.0 / 1024.0;
            let msg = format!(
                "disk at {}: {used_pct:.0}% used, {gib:.1} GiB free",
                p.display()
            );
            if avail < 200 * 1024 * 1024 || used_pct > 95.0 {
                r.add(Level::Fail, msg);
            } else if used_pct > 85.0 {
                r.add(Level::Warn, msg);
            } else {
                r.add(Level::Ok, msg);
            }
        }
    }

    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            r.add(Level::Fail, format!("{e:#}"));
            r.print();
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async {
        let t = Cmd::new("nginx")
            .arg("-t")
            .timeout(Duration::from_secs(30))
            .run()
            .await;
        if t.missing {
            r.add(Level::Info, "nginx is not installed");
        } else if t.ok {
            r.add(Level::Ok, "nginx -t: configuration is valid");
        } else {
            r.add(
                Level::Fail,
                format!("nginx -t: {}", t.error_summary("nginx")),
            );
        }
        match service_active().await {
            None => r.add(
                Level::Info,
                "systemd not available; service status not checked",
            ),
            Some(true) => r.add(Level::Ok, "service nexacloud is active"),
            Some(false) => r.add(
                Level::Fail,
                "service nexacloud is not running (systemctl start nexacloud)",
            ),
        }
    });

    r.print();
    let (f, w) = (r.failures(), r.warnings());
    println!();
    if f == 0 {
        println!(
            "All checks passed{}.",
            if w > 0 {
                format!(" ({w} warning{})", if w == 1 { "" } else { "s" })
            } else {
                String::new()
            }
        );
        ExitCode::SUCCESS
    } else {
        println!("{f} problem{} found.", if f == 1 { "" } else { "s" });
        ExitCode::FAILURE
    }
}
