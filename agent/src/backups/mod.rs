//! Scheduled backups to local disk, SFTP (Synology / any SSH server) and
//! S3-compatible storage (AWS, Synology C2, MinIO, Backblaze B2), via rclone.
//!
//! A **plan** says what to back up (databases + folders), where to
//! (destination), when (schedule) and how long to keep it (retention
//! daily/weekly/monthly). A **run** dumps every database and tars every folder
//! into `{backup_dir}/runs/<plan>/<stamp>/`, copies that folder to the
//! destination with rclone, then prunes old runs on both sides.
//!
//! Secrets: destination secrets live only in the state DB (0600, root) and are
//! handed to rclone through environment variables — never on a command line.
//! SFTP uses a panel-owned ed25519 key (`/etc/nexacloud/backup_ed25519`) whose
//! public half you add to the NAS user, plus pinned host keys.

pub mod routes;
pub mod staging;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::Db;
use crate::jobs::{JobCtx, JobSpec};
use crate::routes::databases::{dump_into, mysql_info, postgres_info, Engine};
use crate::state::AppState;
use crate::util::cmd::Cmd;
use crate::util::create_private_dir;

pub const SSH_KEY: &str = "/etc/nexacloud/backup_ed25519";
pub const KNOWN_HOSTS: &str = "/etc/nexacloud/backup_known_hosts";
const RUN_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

// ------------------------------------------------------------------ models

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Destination {
    pub id: String,
    pub name: String,
    /// local | sftp | s3
    pub kind: String,
    pub config: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub destination_id: Option<String>,
    /// `["all"]` or `["mysql/shop_prod", "postgres/api_prod", ...]`
    pub databases: Vec<String>,
    /// Absolute folders to tar (e.g. `/var/www/shop/storage/app`).
    pub paths: Vec<String>,
    /// `daily 03:00` | `hourly` | `weekly sun 04:00` | `manual`
    pub schedule: String,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub plan_id: String,
    pub job_id: Option<String>,
    pub status: String,
    pub stamp: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub size_bytes: u64,
    pub items: Vec<RunItem>,
    pub local_path: Option<String>,
    pub remote_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunItem {
    /// database | files
    pub kind: String,
    /// `mysql/shop_prod` or the folder path
    pub source: String,
    pub file: String,
    pub size_bytes: u64,
}

// ------------------------------------------------------------------ validation

fn valid_name(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 60 && !s.contains(['\n', '\r'])
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() < 254
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
}

fn valid_remote_path(p: &str) -> bool {
    p.len() < 200
        && !p.contains("..")
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._- ".contains(&b))
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && u.len() < 64
        && u.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn valid_backup_folder(p: &str) -> bool {
    p.starts_with('/')
        && !p.contains("..")
        && p.len() < 200
        && (p.starts_with("/var/www/")
            || p.starts_with("/etc/")
            || p.starts_with("/home/")
            || p.starts_with("/srv/")
            || p.starts_with("/opt/"))
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}

pub fn validate_destination(kind: &str, c: &Value) -> Result<(), String> {
    let s = |k: &str| {
        c.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    match kind {
        "local" => {
            let p = s("path");
            if !(p.starts_with("/var/backups/")
                || p.starts_with("/mnt/")
                || p.starts_with("/srv/")
                || p.starts_with("/backup"))
                || p.contains("..")
            {
                return Err(
                    "local path must be under /var/backups/, /mnt/, /srv/ or /backup".into(),
                );
            }
        }
        "sftp" => {
            if !valid_host(&s("host")) {
                return Err("enter the NAS / server host name or IP".into());
            }
            if !valid_user(&s("user")) {
                return Err("enter the SSH user".into());
            }
            let port = c.get("port").and_then(Value::as_u64).unwrap_or(22);
            if !(1..=65535).contains(&port) {
                return Err("invalid port".into());
            }
            if !valid_remote_path(&s("path")) {
                return Err("remote folder may contain letters, digits, / . _ - and spaces".into());
            }
        }
        "s3" => {
            let ep = s("endpoint");
            if !ep.is_empty() && !(ep.starts_with("https://") || ep.starts_with("http://")) {
                return Err("endpoint must start with https://".into());
            }
            let bucket = s("bucket");
            if bucket.is_empty()
                || !bucket
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b))
            {
                return Err("enter a valid bucket name".into());
            }
            if s("access_key").is_empty() || s("secret_key").is_empty() {
                return Err("access key and secret key are required".into());
            }
            if !valid_remote_path(&s("path")) {
                return Err("invalid folder".into());
            }
        }
        _ => return Err("destination type must be local, sftp or s3".into()),
    }
    let pw = s("encrypt_password");
    if !pw.is_empty() {
        if kind == "local" {
            return Err("encryption is for off-site destinations (SFTP / S3)".into());
        }
        if pw.chars().count() < 12 {
            return Err("use an encryption password of at least 12 characters".into());
        }
    }
    Ok(())
}

/// Backups to this destination are encrypted (rclone crypt).
pub fn encrypted(d: &Destination) -> bool {
    d.kind != "local"
        && d.config
            .get("encrypt_password")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.trim().is_empty())
}

/// Obscure the encryption password for rclone (`rclone obscure -`, via
/// stdin so it never shows in the process list). Needs rclone installed.
pub async fn prepare(d: &Destination) -> Result<Destination> {
    let mut d = d.clone();
    if !encrypted(&d) {
        return Ok(d);
    }
    let pw = d.config["encrypt_password"].as_str().unwrap_or("").trim().to_string();
    let o = Cmd::new("rclone")
        .args(["obscure", "-"])
        .stdin(format!("{pw}\n").into_bytes())
        .timeout(Duration::from_secs(20))
        .run()
        .await;
    if !o.ok || o.stdout.trim().is_empty() {
        bail!("preparing encryption failed: {}", o.error_summary("rclone obscure"));
    }
    d.config["_obscured"] = Value::String(o.stdout.trim().to_string());
    Ok(d)
}

/// Destination as shown to the browser: secrets replaced by a marker.
pub fn redacted(d: &Destination) -> Destination {
    let mut d = d.clone();
    if let Some(o) = d.config.as_object_mut() {
        if o.contains_key("secret_key") {
            o.insert("secret_key".into(), Value::String("••••••".into()));
        }
        let enc = o
            .get("encrypt_password")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.trim().is_empty());
        o.remove("encrypt_password");
        o.remove("_obscured");
        o.insert("encrypted".into(), Value::Bool(enc));
    }
    d
}

pub fn validate_schedule(s: &str) -> Result<(), String> {
    parse_schedule(s).map(|_| ())
}

#[derive(Debug, Clone, PartialEq)]
enum Schedule {
    Manual,
    Hourly,
    Daily(u32, u32),
    Weekly(chrono::Weekday, u32, u32),
}

fn parse_hm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

fn parse_schedule(s: &str) -> Result<Schedule, String> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        ["manual"] => Ok(Schedule::Manual),
        ["hourly"] => Ok(Schedule::Hourly),
        ["daily", hm] => parse_hm(hm)
            .map(|(h, m)| Schedule::Daily(h, m))
            .ok_or("time must be HH:MM".into()),
        ["weekly", day, hm] => {
            let wd: chrono::Weekday = day
                .parse()
                .map_err(|_| "day must be mon..sun".to_string())?;
            parse_hm(hm)
                .map(|(h, m)| Schedule::Weekly(wd, h, m))
                .ok_or("time must be HH:MM".into())
        }
        _ => Err("schedule must be: manual | hourly | daily HH:MM | weekly sun HH:MM".into()),
    }
}

/// Most recent scheduled time at or before `now` (UTC). None for manual.
fn last_slot(s: &Schedule, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    let at = |d: chrono::NaiveDate, h: u32, m: u32| d.and_hms_opt(h, m, 0).map(|t| t.and_utc());
    match *s {
        Schedule::Manual => None,
        Schedule::Hourly => now
            .with_minute(0)
            .and_then(|t| t.with_second(0))
            .and_then(|t| t.with_nanosecond(0)),
        Schedule::Daily(h, m) => {
            let t = at(today, h, m)?;
            Some(if t <= now {
                t
            } else {
                t - chrono::Duration::days(1)
            })
        }
        Schedule::Weekly(wd, h, m) => {
            let back = (7 + now.weekday().num_days_from_monday() as i64
                - wd.num_days_from_monday() as i64)
                % 7;
            let t = at(today - chrono::Duration::days(back), h, m)?;
            Some(if t <= now {
                t
            } else {
                t - chrono::Duration::days(7)
            })
        }
    }
}

pub fn is_due(p: &Plan, now: DateTime<Utc>) -> bool {
    if !p.enabled {
        return false;
    }
    let Ok(s) = parse_schedule(&p.schedule) else {
        return false;
    };
    let Some(slot) = last_slot(&s, now) else {
        return false;
    };
    let created = DateTime::parse_from_rfc3339(&p.created_at)
        .map(|t| t.with_timezone(&Utc))
        .ok();
    let last = p
        .last_run_at
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    match last {
        Some(l) => l < slot,
        None => created.is_none_or(|c| c < slot),
    }
}

// ------------------------------------------------------------------ retention

/// Stamps (`YYYYmmdd-HHMMSS`) to keep: newest per day for `daily` days,
/// newest per ISO week for `weekly` weeks, newest per month for `monthly`.
pub fn retained(stamps: &[String], daily: u32, weekly: u32, monthly: u32) -> BTreeSet<String> {
    let mut parsed: Vec<(NaiveDateTime, &String)> = stamps
        .iter()
        .filter_map(|s| {
            NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S")
                .ok()
                .map(|t| (t, s))
        })
        .collect();
    parsed.sort_by_key(|p| std::cmp::Reverse(p.0));
    let mut keep = BTreeSet::new();
    let mut take = |key: &dyn Fn(&NaiveDateTime) -> String, n: u32| {
        let mut seen = BTreeSet::new();
        for (t, s) in &parsed {
            if seen.len() as u32 >= n {
                break;
            }
            if seen.insert(key(t)) {
                keep.insert((*s).clone());
            }
        }
    };
    take(&|t| t.format("%Y%m%d").to_string(), daily);
    take(
        &|t| format!("{}-{}", t.iso_week().year(), t.iso_week().week()),
        weekly,
    );
    take(&|t| t.format("%Y%m").to_string(), monthly);
    if let Some((_, newest)) = parsed.first() {
        keep.insert((*newest).clone());
    }
    keep
}

// ------------------------------------------------------------------ store

fn now() -> String {
    crate::util::fmt_ts(Utc::now())
}

pub fn list_destinations(db: &Db) -> Result<Vec<Destination>> {
    let c = db.conn();
    let mut st = c.prepare(
        "SELECT id, name, kind, config, created_at FROM backup_destinations ORDER BY created_at",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Destination {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            config: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn get_destination(db: &Db, id: &str) -> Result<Option<Destination>> {
    Ok(list_destinations(db)?.into_iter().find(|d| d.id == id))
}

pub fn insert_destination(db: &Db, d: &Destination) -> Result<()> {
    db.conn().execute(
        "INSERT INTO backup_destinations (id, name, kind, config, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![d.id, d.name, d.kind, d.config.to_string(), d.created_at],
    )?;
    Ok(())
}

pub fn delete_destination(db: &Db, id: &str) -> Result<bool> {
    let in_use: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM backup_plans WHERE destination_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    if in_use > 0 {
        bail!("a backup plan still uses this destination");
    }
    Ok(db
        .conn()
        .execute("DELETE FROM backup_destinations WHERE id = ?1", [id])?
        > 0)
}

fn plan_from_row(r: &rusqlite::Row) -> rusqlite::Result<Plan> {
    Ok(Plan {
        id: r.get(0)?,
        name: r.get(1)?,
        destination_id: r.get(2)?,
        databases: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
        paths: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
        schedule: r.get(5)?,
        keep_daily: r.get(6)?,
        keep_weekly: r.get(7)?,
        keep_monthly: r.get(8)?,
        enabled: r.get::<_, i64>(9)? != 0,
        last_run_at: r.get(10)?,
        created_at: r.get(11)?,
    })
}

const PLAN_COLS: &str = "id, name, destination_id, databases, paths, schedule, keep_daily, keep_weekly, keep_monthly, enabled, last_run_at, created_at";

pub fn list_plans(db: &Db) -> Result<Vec<Plan>> {
    let c = db.conn();
    let mut st = c.prepare(&format!(
        "SELECT {PLAN_COLS} FROM backup_plans ORDER BY created_at"
    ))?;
    let rows = st.query_map([], plan_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn get_plan(db: &Db, id: &str) -> Result<Option<Plan>> {
    Ok(list_plans(db)?.into_iter().find(|p| p.id == id))
}

pub fn upsert_plan(db: &Db, p: &Plan) -> Result<()> {
    db.conn().execute(
        &format!("INSERT INTO backup_plans ({PLAN_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(id) DO UPDATE SET name=?2, destination_id=?3, databases=?4, paths=?5, schedule=?6,
           keep_daily=?7, keep_weekly=?8, keep_monthly=?9, enabled=?10"),
        rusqlite::params![
            p.id, p.name, p.destination_id,
            serde_json::to_string(&p.databases)?, serde_json::to_string(&p.paths)?,
            p.schedule, p.keep_daily, p.keep_weekly, p.keep_monthly, p.enabled as i64,
            p.last_run_at, p.created_at
        ],
    )?;
    Ok(())
}

pub fn delete_plan(db: &Db, id: &str) -> Result<bool> {
    Ok(db
        .conn()
        .execute("DELETE FROM backup_plans WHERE id = ?1", [id])?
        > 0)
}

fn set_last_run(db: &Db, id: &str, at: &str) -> Result<()> {
    db.conn().execute(
        "UPDATE backup_plans SET last_run_at = ?2 WHERE id = ?1",
        rusqlite::params![id, at],
    )?;
    Ok(())
}

fn run_from_row(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        plan_id: r.get(1)?,
        job_id: r.get(2)?,
        status: r.get(3)?,
        stamp: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        size_bytes: r.get::<_, i64>(7)? as u64,
        items: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        local_path: r.get(9)?,
        remote_path: r.get(10)?,
        error: r.get(11)?,
    })
}

const RUN_COLS: &str = "id, plan_id, job_id, status, stamp, started_at, finished_at, size_bytes, items, local_path, remote_path, error";

pub fn list_runs(db: &Db, plan: Option<&str>, limit: usize) -> Result<Vec<Run>> {
    let c = db.conn();
    let sql = format!(
        "SELECT {RUN_COLS} FROM backup_runs {} ORDER BY started_at DESC LIMIT {}",
        if plan.is_some() {
            "WHERE plan_id = ?1"
        } else {
            ""
        },
        limit.min(500)
    );
    let mut st = c.prepare(&sql)?;
    let rows: Vec<Run> = match plan {
        Some(p) => st
            .query_map([p], run_from_row)?
            .collect::<std::result::Result<_, _>>()?,
        None => st
            .query_map([], run_from_row)?
            .collect::<std::result::Result<_, _>>()?,
    };
    Ok(rows)
}

pub fn get_run(db: &Db, id: &str) -> Result<Option<Run>> {
    let c = db.conn();
    let mut st = c.prepare(&format!("SELECT {RUN_COLS} FROM backup_runs WHERE id = ?1"))?;
    let mut rows = st.query_map([id], run_from_row)?;
    Ok(rows.next().transpose()?)
}

fn save_run(db: &Db, r: &Run) -> Result<()> {
    db.conn().execute(
        &format!("INSERT INTO backup_runs ({RUN_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(id) DO UPDATE SET job_id=?3, status=?4, finished_at=?7, size_bytes=?8, items=?9,
           local_path=?10, remote_path=?11, error=?12"),
        rusqlite::params![
            r.id, r.plan_id, r.job_id, r.status, r.stamp, r.started_at, r.finished_at,
            r.size_bytes as i64, serde_json::to_string(&r.items)?, r.local_path, r.remote_path, r.error
        ],
    )?;
    Ok(())
}

/// Mark runs left `running` by a restart as interrupted.
pub fn sweep_interrupted(db: &Db) -> Result<usize> {
    Ok(db.conn().execute(
        "UPDATE backup_runs SET status = 'interrupted', finished_at = ?1 WHERE status = 'running'",
        [now()],
    )?)
}

/// Advisor finding when backups are missing or stale.
pub fn freshness_finding(db: &Db) -> Option<crate::advisor::rules::Finding> {
    let plans = list_plans(db).ok()?;
    let enabled: Vec<&Plan> = plans
        .iter()
        .filter(|p| p.enabled && p.schedule != "manual")
        .collect();
    let mk = |sev: &'static str, title: &str, detail: &str| crate::advisor::rules::Finding {
        id: "backups".into(),
        severity: sev,
        category: "backups",
        title: title.into(),
        detail: detail.into(),
        current: None,
        recommended: Some("daily database backup with an off-site copy".into()),
        tier: "manual",
        fixable: false,
        link: Some("/backups".into()),
        dismissed_until: None,
    };
    if enabled.is_empty() {
        return Some(mk("warning", "No scheduled backups", "Nothing backs up your databases automatically. Create a backup plan (daily, kept 7 days / 4 weeks / 3 months), ideally with a copy on your Synology or S3."));
    }
    let runs = list_runs(db, None, 200).ok()?;
    let cutoff = Utc::now() - chrono::Duration::hours(26);
    let fresh = runs.iter().any(|r| {
        r.status == "ok"
            && DateTime::parse_from_rfc3339(&r.started_at)
                .is_ok_and(|t| t.with_timezone(&Utc) > cutoff)
    });
    (!fresh).then(|| {
        mk(
            "warning",
            "No successful backup in the last 24 hours",
            "Check the backup runs on the Backups page.",
        )
    })
}

// ------------------------------------------------------------------ rclone

/// rclone remote env for a destination, named `NCDEST`.
fn rclone_env(d: &Destination) -> Vec<(String, String)> {
    let s = |k: &str| {
        d.config
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let mut e: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: String| e.push((format!("RCLONE_CONFIG_NCDEST_{k}"), v));
    match d.kind.as_str() {
        "sftp" => {
            put("TYPE", "sftp".into());
            put("HOST", s("host"));
            put("USER", s("user"));
            put(
                "PORT",
                d.config
                    .get("port")
                    .and_then(Value::as_u64)
                    .unwrap_or(22)
                    .to_string(),
            );
            put("KEY_FILE", SSH_KEY.into());
            put("KNOWN_HOSTS_FILE", KNOWN_HOSTS.into());
            put("SHELL_TYPE", "unix".into());
            put("MD5SUM_COMMAND", "none".into());
            put("SHA1SUM_COMMAND", "none".into());
        }
        "s3" => {
            put("TYPE", "s3".into());
            let provider = s("provider");
            put(
                "PROVIDER",
                if provider.is_empty() {
                    "Other".into()
                } else {
                    provider
                },
            );
            put("ACCESS_KEY_ID", s("access_key"));
            put("SECRET_ACCESS_KEY", s("secret_key"));
            if !s("endpoint").is_empty() {
                put("ENDPOINT", s("endpoint"));
            }
            if !s("region").is_empty() {
                put("REGION", s("region"));
            }
            put("NO_CHECK_BUCKET", "true".into());
        }
        _ => {}
    }
    if let Some(ob) = d.config.get("_obscured").and_then(Value::as_str) {
        e.push(("RCLONE_CONFIG_NCCRYPT_TYPE".into(), "crypt".into()));
        e.push(("RCLONE_CONFIG_NCCRYPT_REMOTE".into(), raw_root(d)));
        e.push(("RCLONE_CONFIG_NCCRYPT_PASSWORD".into(), ob.to_string()));
        e.push(("RCLONE_CONFIG_NCCRYPT_FILENAME_ENCRYPTION".into(), "standard".into()));
    }
    e
}

/// `NCCRYPT:` (encrypted, after [`prepare`]), `NCDEST:<base>` for remote
/// destinations, or the local folder.
fn dest_root(d: &Destination) -> String {
    if d.config.get("_obscured").is_some() {
        return "NCCRYPT:".into();
    }
    raw_root(d)
}

/// `NCCRYPT:` + `a/b` → `NCCRYPT:a/b`; `/x` + `a` → `/x/a`.
fn join_remote(root: &str, rel: &str) -> String {
    if root.ends_with(':') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

fn raw_root(d: &Destination) -> String {
    let s = |k: &str| {
        d.config
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .trim_end_matches('/')
            .to_string()
    };
    match d.kind.as_str() {
        "local" => s("path"),
        "s3" => {
            let p = s("path");
            let p = p.trim_start_matches('/');
            if p.is_empty() {
                format!("NCDEST:{}", s("bucket"))
            } else {
                format!("NCDEST:{}/{p}", s("bucket"))
            }
        }
        _ => format!("NCDEST:{}", s("path")),
    }
}

fn rclone(d: &Destination, args: &[&str]) -> Cmd {
    let mut c = Cmd::new("rclone")
        .args(args)
        .env("RCLONE_CONFIG", "/dev/null");
    for (k, v) in rclone_env(d) {
        c = c.env(&k, &v);
    }
    c
}

/// Create the panel SSH key if missing; return the public key line.
pub async fn ensure_ssh_key() -> Result<String> {
    if !Path::new(SSH_KEY).exists() {
        let o = Cmd::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "nexacloud-backup",
                "-f",
                SSH_KEY,
                "-q",
            ])
            .timeout(Duration::from_secs(30))
            .run()
            .await;
        if !o.ok {
            bail!("ssh-keygen failed: {}", o.error_summary("ssh-keygen"));
        }
    }
    Ok(std::fs::read_to_string(format!("{SSH_KEY}.pub"))?
        .trim()
        .to_string())
}

/// Pin the SFTP host key (trust on first use) into KNOWN_HOSTS.
async fn pin_host_key(d: &Destination, job: &JobCtx) -> Result<()> {
    let host = d.config.get("host").and_then(Value::as_str).unwrap_or("");
    let port = d
        .config
        .get("port")
        .and_then(Value::as_u64)
        .unwrap_or(22)
        .to_string();
    let existing = std::fs::read_to_string(KNOWN_HOSTS).unwrap_or_default();
    let marker = if port == "22" {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    if existing.lines().any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|h| h.split(',').any(|x| x == marker))
    }) {
        return Ok(());
    }
    let o = Cmd::new("ssh-keyscan")
        .args(["-p", &port, "-t", "ed25519,ecdsa,rsa", host])
        .timeout(Duration::from_secs(20))
        .run()
        .await;
    if !o.ok || o.stdout.trim().is_empty() {
        bail!(
            "could not reach {host}:{port} over SSH ({})",
            o.error_summary("ssh-keyscan")
        );
    }
    job.log(&format!("Pinned SSH host key of {marker}"));
    let mut text = existing;
    text.push_str(&o.stdout);
    crate::util::write_private_file(Path::new(KNOWN_HOSTS), text.as_bytes())?;
    Ok(())
}

async fn ensure_rclone(job: &JobCtx) -> Result<()> {
    if Path::new("/usr/bin/rclone").exists() {
        return Ok(());
    }
    job.log("rclone is not installed — installing it (apt-get install rclone)");
    let _ = Cmd::new("apt-get")
        .args(["update", "-q"])
        .timeout(Duration::from_secs(600))
        .run_streaming(job.log.as_ref())
        .await;
    let o = Cmd::new("apt-get")
        .args(["install", "-y", "-q", "rclone"])
        .timeout(Duration::from_secs(900))
        .run_streaming(job.log.as_ref())
        .await;
    if !o.ok {
        bail!("installing rclone failed");
    }
    Ok(())
}

/// Connectivity test: create the folder and write+delete a marker file.
pub async fn test_destination(d: &Destination, job: &JobCtx) -> Result<()> {
    if d.kind == "local" {
        let root = PathBuf::from(dest_root(d));
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let marker = root.join(".nexacloud-test");
        std::fs::write(&marker, b"ok")?;
        std::fs::remove_file(&marker)?;
        job.log(&format!(
            "Wrote and removed a test file in {}",
            root.display()
        ));
        return Ok(());
    }
    ensure_rclone(job).await?;
    if d.kind == "sftp" {
        ensure_ssh_key().await?;
        pin_host_key(d, job).await?;
    }
    let d = &prepare(d).await?;
    if encrypted(d) {
        job.log("Encryption is on: files and names are encrypted before they leave this server.");
    }
    let root = dest_root(d);
    let tmp = std::env::temp_dir().join(format!("nexacloud-test-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, b"NexaCloud connection test\n")?;
    let target = join_remote(&root, ".nexacloud-test");
    for (what, args) in [
        ("mkdir", vec!["mkdir".to_string(), root.clone()]),
        (
            "upload",
            vec![
                "copyto".into(),
                tmp.to_string_lossy().into_owned(),
                target.clone(),
            ],
        ),
        ("delete", vec!["deletefile".into(), target.clone()]),
    ] {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = rclone(d, &a)
            .timeout(Duration::from_secs(120))
            .run_streaming(job.log.as_ref())
            .await;
        if !o.ok {
            let _ = std::fs::remove_file(&tmp);
            bail!("{what} failed: {}", o.error_summary("rclone"));
        }
        job.log(&format!("{what}: ok"));
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

// ------------------------------------------------------------------ run

fn dir_size(p: &Path) -> u64 {
    walk(p)
        .iter()
        .filter_map(|f| f.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn walk(p: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

async fn resolve_databases(spec: &[String]) -> Vec<(Engine, String)> {
    let mut out = Vec::new();
    let all = spec.iter().any(|s| s == "all");
    let my = mysql_info().await;
    let pg = postgres_info().await;
    for (engine, info) in [(Engine::Mysql, my), (Engine::Postgres, pg)] {
        if !info.available {
            continue;
        }
        for d in info.databases {
            let key = format!("{}/{}", engine.as_str(), d.name);
            if all || spec.contains(&key) {
                out.push((engine, d.name));
            }
        }
    }
    out
}

pub fn submit_run(state: &AppState, plan: &Plan, actor: Option<String>) -> Result<String> {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut run = Run {
        id: uuid::Uuid::new_v4().to_string(),
        plan_id: plan.id.clone(),
        job_id: None,
        status: "running".into(),
        stamp,
        started_at: now(),
        finished_at: None,
        size_bytes: 0,
        items: Vec::new(),
        local_path: None,
        remote_path: None,
        error: None,
    };
    save_run(&state.db, &run)?;
    set_last_run(&state.db, &plan.id, &run.started_at)?;
    let spec = JobSpec::new("backup_run", &format!("Backup: {}", plan.name))
        .heavy()
        .lock(format!("backup:{}", plan.id))
        .timeout(RUN_TIMEOUT)
        .actor(actor);
    let st = state.clone();
    let plan = plan.clone();
    let mut r2 = run.clone();
    let job_id = state.jobs.submit(spec, move |ctx| async move {
        let res = execute_run(&st, &ctx, &plan, &mut r2).await;
        r2.finished_at = Some(now());
        match &res {
            Ok(()) => r2.status = "ok".into(),
            Err(e) => {
                r2.status = "failed".into();
                r2.error = Some(format!("{e:#}"));
                crate::monitor::notify::send(
                    &st,
                    crate::monitor::notify::Event::BackupFailed,
                    &format!("backup_failed:{}", plan.id),
                    "critical",
                    &format!("Backup \"{}\" failed", plan.name),
                    &format!("{e:#}"),
                    None,
                );
            }
        }
        let _ = save_run(&st.db, &r2);
        res
    })?;
    run.job_id = Some(job_id.clone());
    save_run(&state.db, &run)?;
    Ok(job_id)
}

async fn execute_run(state: &AppState, job: &JobCtx, plan: &Plan, run: &mut Run) -> Result<()> {
    run.job_id = Some(job.id.clone());
    let base = state.config.backup_dir.join("runs").join(&plan.id);
    for d in [
        &state.config.backup_dir,
        &state.config.backup_dir.join("runs"),
        &base,
    ] {
        create_private_dir(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let dir = base.join(&run.stamp);
    create_private_dir(&dir)?;
    run.local_path = Some(dir.to_string_lossy().into_owned());

    // Databases.
    let dbs = resolve_databases(&plan.databases).await;
    if dbs.is_empty() && plan.paths.is_empty() {
        bail!("nothing to back up: no matching databases and no folders");
    }
    for (engine, name) in &dbs {
        let f = dump_into(job, *engine, name, &dir).await?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        job.log(&format!("  {}/{name}: {size} bytes", engine.as_str()));
        run.items.push(RunItem {
            kind: "database".into(),
            source: format!("{}/{name}", engine.as_str()),
            file: f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            size_bytes: size,
        });
    }
    // Folders.
    for (i, p) in plan.paths.iter().enumerate() {
        if !Path::new(p).exists() {
            job.log(&format!("  skip {p}: does not exist"));
            continue;
        }
        let file = format!(
            "files-{}-{}.tar.gz",
            i + 1,
            p.trim_matches('/').replace('/', "_")
        );
        let out = dir.join(&file);
        job.log(&format!("Archiving {p}"));
        let o = Cmd::new("tar")
            .args([
                "-czf",
                &out.to_string_lossy(),
                "--warning=no-file-changed",
                "-C",
                "/",
                p.trim_start_matches('/'),
            ])
            .timeout(Duration::from_secs(3 * 3600))
            .run_streaming(job.log.as_ref())
            .await;
        if !o.ok && !(o.stderr.is_empty() && out.exists()) {
            bail!("tar {p} failed: {}", o.error_summary("tar"));
        }
        let _ = std::fs::set_permissions(&out, std::os::unix::fs::PermissionsExt::from_mode(0o600));
        let size = out.metadata().map(|m| m.len()).unwrap_or(0);
        run.items.push(RunItem {
            kind: "files".into(),
            source: p.clone(),
            file,
            size_bytes: size,
        });
    }
    let manifest = serde_json::json!({"plan": plan.name, "stamp": run.stamp, "items": run.items});
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    run.size_bytes = dir_size(&dir);
    job.log(&format!(
        "Local copy: {} ({} bytes)",
        dir.display(),
        run.size_bytes
    ));

    // Copy to destination.
    if let Some(did) = &plan.destination_id {
        let d = get_destination(&state.db, did)?
            .ok_or_else(|| anyhow!("destination {did} no longer exists"))?;
        if d.kind != "local" {
            ensure_rclone(job).await?;
        }
        let d = prepare(&d).await?;
        let target = join_remote(
            &dest_root(&d),
            &format!("{}/{}", slug(&plan.name, &plan.id), run.stamp),
        );
        if d.kind == "local" {
            std::fs::create_dir_all(&target)?;
            for f in walk(&dir) {
                let to = Path::new(&target).join(f.file_name().unwrap_or_default());
                std::fs::copy(&f, &to).with_context(|| format!("copying to {}", to.display()))?;
            }
        } else {
            ensure_rclone(job).await?;
            if d.kind == "sftp" {
                pin_host_key(&d, job).await?;
            }
            job.log(&format!(
                "Uploading to {} ({}{})",
                d.name,
                d.kind,
                if encrypted(&d) { ", encrypted" } else { "" }
            ));
            let o = rclone(
                &d,
                &[
                    "copy",
                    &dir.to_string_lossy(),
                    &target,
                    "--stats-one-line",
                    "--stats",
                    "30s",
                    "-v",
                ],
            )
            .timeout(RUN_TIMEOUT)
            .run_streaming(job.log.as_ref())
            .await;
            if !o.ok {
                bail!("upload failed: {}", o.error_summary("rclone"));
            }
        }
        run.remote_path = Some(target.clone());
        job.log(&format!("Copied to {target}"));
        prune_remote(
            &d,
            &join_remote(&dest_root(&d), &slug(&plan.name, &plan.id)),
            plan,
            job,
        )
        .await;
    }
    prune_local(&base, plan, job);
    Ok(())
}

fn slug(name: &str, id: &str) -> String {
    let s: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        id.to_string()
    } else {
        s
    }
}

fn prune_local(base: &Path, plan: &Plan, job: &JobCtx) {
    let stamps: Vec<String> = std::fs::read_dir(base)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let keep = retained(
        &stamps,
        plan.keep_daily,
        plan.keep_weekly,
        plan.keep_monthly,
    );
    for s in stamps
        .iter()
        .filter(|s| !keep.contains(*s) && s.len() == 15)
    {
        if std::fs::remove_dir_all(base.join(s)).is_ok() {
            job.log(&format!("Pruned local run {s}"));
        }
    }
}

async fn prune_remote(d: &Destination, folder: &str, plan: &Plan, job: &JobCtx) {
    let stamps: Vec<String> = if d.kind == "local" {
        std::fs::read_dir(folder)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        let o = rclone(d, &["lsf", "--dirs-only", folder])
            .timeout(Duration::from_secs(120))
            .run()
            .await;
        if !o.ok {
            job.log("Could not list remote runs for pruning (will retry next run)");
            return;
        }
        o.stdout
            .lines()
            .map(|l| l.trim_end_matches('/').to_string())
            .collect()
    };
    let keep = retained(
        &stamps,
        plan.keep_daily,
        plan.keep_weekly,
        plan.keep_monthly,
    );
    for s in stamps
        .iter()
        .filter(|s| !keep.contains(*s) && s.len() == 15)
    {
        let target = format!("{folder}/{s}");
        let ok = if d.kind == "local" {
            std::fs::remove_dir_all(&target).is_ok()
        } else {
            rclone(d, &["purge", &target])
                .timeout(Duration::from_secs(600))
                .run()
                .await
                .ok
        };
        if ok {
            job.log(&format!("Pruned remote run {s}"));
        }
    }
}

// ------------------------------------------------------------------ restore

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    /// `mysql/shop_prod`
    pub source: String,
}

/// Restore one database from a run into `<name>_restore_<stamp>` (never over
/// the live database).
pub fn submit_restore(
    state: &AppState,
    run: &Run,
    source: &str,
    actor: Option<String>,
) -> Result<(String, String)> {
    let item = run
        .items
        .iter()
        .find(|i| i.kind == "database" && i.source == source)
        .ok_or_else(|| anyhow!("{source} is not in this backup"))?
        .clone();
    let (engine, name) = source
        .split_once('/')
        .ok_or_else(|| anyhow!("bad source"))?;
    let engine = match engine {
        "mysql" => Engine::Mysql,
        "postgres" => Engine::Postgres,
        _ => bail!("bad engine"),
    };
    let target = format!("{}_restore_{}", name, &run.stamp[..8]);
    if !crate::util::is_valid_db_name(&target) {
        bail!("restored database name would be invalid");
    }
    let spec = JobSpec::new("backup_restore", &format!("Restore {source} into {target}"))
        .heavy()
        .lock(format!("db:{}/{target}", engine.as_str()))
        .timeout(RUN_TIMEOUT)
        .actor(actor);
    let st = state.clone();
    let run = run.clone();
    let t2 = target.clone();
    let id = state.jobs.submit(spec, move |ctx| async move {
        restore(&st, &ctx, &run, &item, engine, &t2).await
    })?;
    Ok((id, target))
}

/// The backup file of one item: the local copy when it still exists,
/// otherwise downloaded (and decrypted) from the destination into a private
/// temp folder (returned second; delete it when done).
pub async fn fetch_item(
    state: &AppState,
    job: &JobCtx,
    run: &Run,
    item: &RunItem,
) -> Result<(PathBuf, Option<PathBuf>)> {
    if let Some(f) = run
        .local_path
        .as_ref()
        .map(|p| Path::new(p).join(&item.file))
        .filter(|p| p.exists())
    {
        return Ok((f, None));
    }
    let plan = get_plan(&state.db, &run.plan_id)?.ok_or_else(|| anyhow!("plan deleted"))?;
    let d = get_destination(&state.db, plan.destination_id.as_deref().unwrap_or(""))?
        .ok_or_else(|| anyhow!("the local copy was pruned and the destination is gone"))?;
    if d.kind != "local" {
        ensure_rclone(job).await?;
    }
    let d = prepare(&d).await?;
    let remote = run
        .remote_path
        .clone()
        .ok_or_else(|| anyhow!("no remote copy"))?;
    let td = state
        .config
        .backup_dir
        .join(format!(".restore-{}", uuid::Uuid::new_v4()));
    create_private_dir(&td)?;
    job.log(&format!("Downloading {} from {}", item.file, d.name));
    let src = format!("{remote}/{}", item.file);
    let ok = if d.kind == "local" {
        std::fs::copy(&src, td.join(&item.file)).is_ok()
    } else {
        let o = rclone(&d, &["copyto", &src, &td.join(&item.file).to_string_lossy()])
            .timeout(RUN_TIMEOUT)
            .run_streaming(job.log.as_ref())
            .await;
        o.ok
    };
    if !ok {
        let _ = std::fs::remove_dir_all(&td);
        bail!("download of {} failed", item.file);
    }
    Ok((td.join(&item.file), Some(td)))
}

async fn restore(
    state: &AppState,
    job: &JobCtx,
    run: &Run,
    item: &RunItem,
    engine: Engine,
    target: &str,
) -> Result<()> {
    let (file, tmpdir) = fetch_item(state, job, run, item).await?;

    let res = async {
        match engine {
            Engine::Mysql => {
                job.log(&format!("Creating MySQL database {target}"));
                let o = Cmd::new("mysql").args(["-e", &format!("CREATE DATABASE `{target}`")]).run().await;
                if !o.ok {
                    bail!("create database failed: {}", o.error_summary("mysql"));
                }
                let o = Cmd::new("bash")
                    .args(["-c", "set -o pipefail; gunzip -c \"$1\" | mysql \"$2\"", "restore", &file.to_string_lossy(), target])
                    .timeout(RUN_TIMEOUT)
                    .run_streaming(job.log.as_ref())
                    .await;
                if !o.ok {
                    bail!("import failed: {}", o.error_summary("mysql"));
                }
            }
            Engine::Postgres => {
                job.log(&format!("Creating PostgreSQL database {target}"));
                let o = Cmd::new("runuser").args(["-u", "postgres", "--", "createdb", target]).run().await;
                if !o.ok {
                    bail!("createdb failed: {}", o.error_summary("createdb"));
                }
                let o = Cmd::new("bash")
                    .args(["-c", "set -o pipefail; gunzip -c \"$1\" | runuser -u postgres -- psql -q -v ON_ERROR_STOP=1 -d \"$2\"", "restore", &file.to_string_lossy(), target])
                    .timeout(RUN_TIMEOUT)
                    .run_streaming(job.log.as_ref())
                    .await;
                if !o.ok {
                    bail!("import failed: {}", o.error_summary("psql"));
                }
            }
        }
        job.log(&format!("Restored into {target}. Compare it with the live database; when you're happy, point your app's .env at it or copy the data over."));
        Ok(())
    }
    .await;
    if let Some(td) = tmpdir {
        let _ = std::fs::remove_dir_all(td);
    }
    res
}

// ------------------------------------------------------------------ scheduler

/// Background loop: start due plans every 30 s.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let _ = sweep_interrupted(&state.db);
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let Ok(plans) = list_plans(&state.db) else {
                continue;
            };
            let now_t = Utc::now();
            for p in plans.iter().filter(|p| is_due(p, now_t)) {
                match submit_run(&state, p, Some("scheduler".into())) {
                    Ok(id) => tracing::info!("scheduled backup {} started (job {id})", p.name),
                    Err(e) => tracing::warn!("scheduled backup {} failed to start: {e:#}", p.name),
                }
            }
        }
    });
}

// ------------------------------------------------------------------ disk

#[derive(Debug, Serialize)]
pub struct DiskUsage {
    pub path: String,
    pub label: String,
    pub bytes: u64,
}

pub async fn disk_usage(backup_dir: &Path) -> Vec<DiskUsage> {
    let items = [
        ("/var/www", "Websites"),
        ("/var/lib/mysql", "MySQL data"),
        ("/var/lib/postgresql", "PostgreSQL data"),
        (&*backup_dir.to_string_lossy(), "Local backups"),
        ("/var/log", "Logs"),
        ("/var/cache/apt", "Package cache"),
        ("/usr", "System & packages"),
    ];
    let mut out = Vec::new();
    for (p, label) in items {
        if !Path::new(p).exists() {
            continue;
        }
        let o = Cmd::new("du")
            .args(["-sxb", p])
            .timeout(Duration::from_secs(60))
            .run()
            .await;
        let bytes = o
            .stdout
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        out.push(DiskUsage {
            path: p.to_string(),
            label: label.to_string(),
            bytes,
        });
    }
    out
}

pub fn valid_folder(p: &str) -> bool {
    valid_backup_folder(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn schedules() {
        assert!(validate_schedule("daily 03:00").is_ok());
        assert!(validate_schedule("weekly sun 04:30").is_ok());
        assert!(validate_schedule("hourly").is_ok());
        assert!(validate_schedule("daily 25:00").is_err());
        assert!(validate_schedule("every day").is_err());
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 10, 0, 0).unwrap();
        let mut p = Plan {
            id: "p".into(),
            name: "n".into(),
            destination_id: None,
            databases: vec!["all".into()],
            paths: vec![],
            schedule: "daily 03:00".into(),
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 3,
            enabled: true,
            last_run_at: None,
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        assert!(is_due(&p, now));
        p.last_run_at = Some("2026-11-13T03:00:05Z".into());
        assert!(!is_due(&p, now));
        p.last_run_at = Some("2026-11-12T03:00:05Z".into());
        assert!(is_due(&p, now));
        p.enabled = false;
        assert!(!is_due(&p, now));
        p.enabled = true;
        p.schedule = "manual".into();
        assert!(!is_due(&p, now));
        // created after today's slot → not due until tomorrow
        p.schedule = "daily 03:00".into();
        p.last_run_at = None;
        p.created_at = "2026-11-13T05:00:00Z".into();
        assert!(!is_due(&p, now));
    }

    #[test]
    fn retention_keeps_daily_weekly_monthly() {
        let mut stamps = Vec::new();
        for d in 1..=60 {
            let t = Utc.with_ymd_and_hms(2026, 9, 1, 3, 0, 0).unwrap() + chrono::Duration::days(d);
            stamps.push(t.format("%Y%m%d-%H%M%S").to_string());
        }
        let keep = retained(&stamps, 7, 4, 3);
        assert!(keep.contains(&stamps[59]));
        assert!(keep.len() >= 7 && keep.len() <= 14, "{}", keep.len());
        assert!(!keep.contains(&stamps[20]) || keep.len() < 60);
    }

    #[test]
    fn destination_validation() {
        let ok = serde_json::json!({"host":"100.64.0.5","user":"backup","port":22,"path":"/volume1/backups/vps"});
        assert!(validate_destination("sftp", &ok).is_ok());
        let bad = serde_json::json!({"host":"nas;rm -rf /","user":"b","path":"/x"});
        assert!(validate_destination("sftp", &bad).is_err());
        let s3 = serde_json::json!({"bucket":"my-backups","access_key":"A","secret_key":"S","endpoint":"https://s3.synology.me","path":"vps"});
        assert!(validate_destination("s3", &s3).is_ok());
        assert!(validate_destination("local", &serde_json::json!({"path":"/etc/x"})).is_err());
        assert!(valid_backup_folder("/var/www/shop/storage/app"));
        assert!(!valid_backup_folder("/root/.ssh"));
    }
}
