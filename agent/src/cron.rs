//! Cron jobs (any command, like cPanel's "Cron Jobs"), with last run, exit
//! code and output.
//!
//! All jobs live in `/etc/cron.d/nexacloud-jobs` (regenerated on every
//! change). Each line runs the command through
//! `/usr/local/lib/nexacloud/cron-run <id>`, which keeps the last 64 KB of
//! output and `start end exit` in `/var/lib/nexacloud-cron/<id>/`.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::auth::Identity;
use crate::error::{ApiError, ApiResult};
use crate::jobs::JobSpec;
use crate::state::AppState;
use crate::util::cmd::Cmd;

const CRON_FILE: &str = "/etc/cron.d/nexacloud-jobs";
const RUNNER: &str = "/usr/local/lib/nexacloud/cron-run";
const STATE_DIR: &str = "/var/lib/nexacloud-cron";

const RUNNER_SCRIPT: &str = r#"#!/bin/bash
# Managed by NexaCloud: run a cron job and keep its last output and exit code.
id="$1"; shift; [ "$1" = "--" ] && shift
d="/var/lib/nexacloud-cron/$id"
start=$(date +%s)
out="$(/bin/bash -c "$*" 2>&1)"; code=$?
printf '%s\n' "$out" | tail -c 65536 > "$d/last.log" 2>/dev/null
printf '%s %s %s\n' "$start" "$(date +%s)" "$code" > "$d/status" 2>/dev/null
exit $code
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub project_id: Option<String>,
    pub user: String,
    pub schedule: String,
    pub command: String,
    pub cwd: String,
    pub enabled: bool,
    pub created_at: String,
}

pub fn valid_schedule(s: &str) -> bool {
    let s = s.trim();
    if ["@hourly", "@daily", "@weekly", "@monthly", "@yearly", "@reboot", "@midnight"].contains(&s) {
        return true;
    }
    let f: Vec<&str> = s.split_whitespace().collect();
    f.len() == 5
        && f.iter().all(|x| !x.is_empty() && x.len() <= 40 && x.bytes().all(|b| b.is_ascii_digit() || b"*/,-".contains(&b)))
}

fn valid_user(u: &str) -> bool {
    !u.is_empty() && u.len() <= 32 && u.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

/// `%` means newline to cron; quote the command for `bash -c`.
fn cron_quote(cmd: &str) -> String {
    format!("'{}'", cmd.replace('\'', "'\\''").replace('%', "\\%"))
}

pub fn render(jobs: &[CronJob]) -> String {
    let mut out = String::from("# Managed by NexaCloud — cron jobs from the panel. Edit them there.\nSHELL=/bin/sh\nPATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n");
    for j in jobs.iter().filter(|j| j.enabled) {
        out.push_str(&format!(
            "{} {} cd {} && {RUNNER} {} -- {}\n",
            j.schedule.trim(),
            j.user,
            cron_quote(&j.cwd),
            j.id,
            cron_quote(&j.command)
        ));
    }
    out
}

fn now() -> String {
    crate::util::fmt_ts(chrono::Utc::now())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<CronJob> {
    Ok(CronJob {
        id: r.get(0)?,
        project_id: r.get(1)?,
        user: r.get(2)?,
        schedule: r.get(3)?,
        command: r.get(4)?,
        cwd: r.get(5)?,
        enabled: r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
    })
}

pub fn list(db: &crate::db::Db) -> Result<Vec<CronJob>> {
    let c = db.conn();
    let mut st = c.prepare("SELECT id, project_id, user, schedule, command, cwd, enabled, created_at FROM cron_jobs ORDER BY created_at")?;
    let v = st.query_map([], row)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

fn save(db: &crate::db::Db, j: &CronJob) -> Result<()> {
    db.conn().execute(
        "INSERT INTO cron_jobs (id, project_id, user, schedule, command, cwd, enabled, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(id) DO UPDATE SET user=?3, schedule=?4, command=?5, cwd=?6, enabled=?7",
        rusqlite::params![j.id, j.project_id, j.user, j.schedule, j.command, j.cwd, j.enabled as i64, j.created_at],
    )?;
    Ok(())
}

/// Install the runner, create state folders, rewrite the cron file.
async fn sync(state: &AppState) -> Result<()> {
    let jobs = list(&state.db)?;
    std::fs::create_dir_all("/usr/local/lib/nexacloud")?;
    std::fs::write(RUNNER, RUNNER_SCRIPT)?;
    std::fs::set_permissions(RUNNER, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::create_dir_all(STATE_DIR)?;
    std::fs::set_permissions(STATE_DIR, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    for j in &jobs {
        let d = format!("{STATE_DIR}/{}", j.id);
        let _ = std::fs::create_dir_all(&d);
        let _ = Cmd::new("chown").args([&format!("{u}:{u}", u = j.user), &d]).run().await;
        let _ = std::fs::set_permissions(&d, std::os::unix::fs::PermissionsExt::from_mode(0o750));
    }
    crate::util::atomic_write(Path::new(CRON_FILE), render(&jobs).as_bytes(), 0o644, Some((0, 0)))?;
    if !Path::new("/usr/sbin/cron").exists() {
        let _ = Cmd::new("apt-get").args(["install", "-y", "-q", "cron"]).env("DEBIAN_FRONTEND", "noninteractive").timeout(Duration::from_secs(600)).run().await;
    }
    let _ = Cmd::new("systemctl").args(["enable", "--now", "cron"]).run().await;
    Ok(())
}

#[derive(Serialize)]
struct JobView {
    #[serde(flatten)]
    job: CronJob,
    last_start: Option<i64>,
    last_secs: Option<i64>,
    last_exit: Option<i32>,
}

fn status_of(id: &str) -> (Option<i64>, Option<i64>, Option<i32>) {
    let t = std::fs::read_to_string(format!("{STATE_DIR}/{id}/status")).unwrap_or_default();
    let f: Vec<&str> = t.split_whitespace().collect();
    if f.len() < 3 {
        return (None, None, None);
    }
    let s: Option<i64> = f[0].parse().ok();
    let e: Option<i64> = f[1].parse().ok();
    (s, s.zip(e).map(|(a, b)| b - a), f[2].parse().ok())
}

fn visible(me: &Identity, j: &CronJob) -> bool {
    me.role == crate::users::Role::Admin || j.project_id.as_deref().is_some_and(|p| me.can_change_project(p) || me.role == crate::users::Role::Viewer)
}

pub async fn list_route(State(state): State<AppState>, Extension(me): Extension<Identity>) -> ApiResult<Json<serde_json::Value>> {
    let v: Vec<JobView> = list(&state.db)?
        .into_iter()
        .filter(|j| visible(&me, j))
        .map(|j| {
            let (s, d, e) = status_of(&j.id);
            JobView { job: j, last_start: s, last_secs: d, last_exit: e }
        })
        .collect();
    Ok(Json(json!(v)))
}

#[derive(Deserialize)]
pub struct JobBody {
    project_id: Option<String>,
    user: Option<String>,
    schedule: String,
    command: String,
    #[serde(default = "yes")]
    enabled: bool,
}
fn yes() -> bool {
    true
}

/// Check and complete a job from the body (user and folder follow the project).
fn build(state: &AppState, me: &Identity, b: &JobBody, existing: Option<&CronJob>) -> ApiResult<CronJob> {
    if !valid_schedule(&b.schedule) {
        return Err(ApiError::bad_request("schedule: five fields like */5 * * * * or @daily"));
    }
    let cmd = b.command.trim();
    if cmd.is_empty() || cmd.len() > 2000 || cmd.contains('\n') || cmd.contains('\r') {
        return Err(ApiError::bad_request("command: one line, up to 2000 characters"));
    }
    let (project_id, user, cwd) = match &b.project_id {
        Some(pid) if !pid.is_empty() => {
            if !me.can_change_project(pid) {
                return Err(ApiError::new(axum::http::StatusCode::FORBIDDEN, "you cannot change that project"));
            }
            let p = crate::deploys::get_project(&state.db, pid)?.ok_or_else(|| ApiError::not_found("project not found"))?;
            (Some(pid.clone()), crate::deploys::permissions::web_user(&p), p.app_dir())
        }
        _ => {
            if me.role != crate::users::Role::Admin {
                return Err(ApiError::new(axum::http::StatusCode::FORBIDDEN, "only admins can add server-wide cron jobs"));
            }
            let u = b.user.clone().unwrap_or_else(|| "root".into());
            if !valid_user(&u) {
                return Err(ApiError::bad_request("invalid user"));
            }
            (None, u, "/".to_string())
        }
    };
    Ok(CronJob {
        id: existing.map(|e| e.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..12].to_string()),
        project_id,
        user,
        schedule: b.schedule.trim().to_string(),
        command: cmd.to_string(),
        cwd,
        enabled: b.enabled,
        created_at: existing.map(|e| e.created_at.clone()).unwrap_or_else(now),
    })
}

async fn user_exists(u: &str) -> bool {
    Cmd::new("id").arg(u).run().await.ok
}

pub async fn create(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<JobBody>) -> Response {
    let info = AuditInfo::action("cron.create", b.command.chars().take(60).collect::<String>());
    let res: ApiResult<Response> = async {
        let j = build(&state, &me, &b, None)?;
        if !user_exists(&j.user).await {
            return Err(ApiError::bad_request(format!("user {} does not exist", j.user)));
        }
        save(&state.db, &j)?;
        sync(&state).await.map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(Json(json!(j)).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn find(state: &AppState, me: &Identity, id: &str) -> ApiResult<CronJob> {
    let j = list(&state.db)?.into_iter().find(|j| j.id == id).ok_or_else(|| ApiError::not_found("cron job not found"))?;
    let ok = me.role == crate::users::Role::Admin || j.project_id.as_deref().is_some_and(|p| me.can_change_project(p));
    if !ok {
        return Err(ApiError::new(axum::http::StatusCode::FORBIDDEN, "you cannot change that cron job"));
    }
    Ok(j)
}

pub async fn update(State(state): State<AppState>, Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>, Json(b): Json<JobBody>) -> Response {
    let info = AuditInfo::action("cron.update", id.clone());
    let res: ApiResult<Response> = async {
        let old = find(&state, &me, &id)?;
        let j = build(&state, &me, &b, Some(&old))?;
        if !user_exists(&j.user).await {
            return Err(ApiError::bad_request(format!("user {} does not exist", j.user)));
        }
        save(&state.db, &j)?;
        sync(&state).await.map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(Json(json!(j)).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn remove(State(state): State<AppState>, Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("cron.delete", id.clone());
    let res: ApiResult<Response> = async {
        find(&state, &me, &id)?;
        state.db.conn().execute("DELETE FROM cron_jobs WHERE id = ?1", [&id]).map_err(|e| ApiError::internal(e.to_string()))?;
        let _ = std::fs::remove_dir_all(format!("{STATE_DIR}/{id}"));
        sync(&state).await.map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

/// Run now (as a job with live output), through the same runner.
pub async fn run_now(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Extension(me): Extension<Identity>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let mut info = AuditInfo::action("cron.run", id.clone());
    let res: ApiResult<Response> = async {
        let j = find(&state, &me, &id)?;
        sync(&state).await.map_err(|e| ApiError::internal(format!("{e:#}")))?;
        let spec = JobSpec::new("cron_run", &format!("Cron: {}", crate::util::cmd::truncate(&j.command, 60)))
            .timeout(Duration::from_secs(3600))
            .actor(Some(crate::routes::changes::client_ip(&addr)));
        let job = state.jobs.submit(spec, move |ctx| async move {
            let o = Cmd::new("runuser")
                .args(["-u", &j.user, "--", "bash", "-c", &format!("cd {} && {RUNNER} {} -- {}", cron_quote(&j.cwd), j.id, cron_quote(&j.command).replace("\\%", "%"))])
                .timeout(Duration::from_secs(3600))
                .run()
                .await;
            let log = std::fs::read_to_string(format!("{STATE_DIR}/{}/last.log", j.id)).unwrap_or_default();
            for l in log.lines() {
                ctx.log(l);
            }
            if o.ok {
                Ok(())
            } else {
                Err(anyhow!("exit code {:?}", o.exit_code))
            }
        })?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn log(State(state): State<AppState>, Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<serde_json::Value>> {
    let j = list(&state.db)?.into_iter().find(|j| j.id == id).ok_or_else(|| ApiError::not_found("cron job not found"))?;
    if !visible(&me, &j) {
        return Err(ApiError::not_found("cron job not found"));
    }
    let (s, d, e) = status_of(&id);
    let text = std::fs::read_to_string(format!("{STATE_DIR}/{id}/last.log")).unwrap_or_default();
    Ok(Json(json!({"log": text, "last_start": s, "last_secs": d, "last_exit": e})))
}

/// Startup: make sure the file matches the database.
pub async fn startup(state: &AppState) {
    if list(&state.db).map(|v| !v.is_empty()).unwrap_or(false) {
        if let Err(e) = sync(state).await {
            tracing::warn!("cron sync: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_and_rendering() {
        assert!(valid_schedule("*/5 * * * *"));
        assert!(valid_schedule("0 3 * * 1-5"));
        assert!(valid_schedule("@daily"));
        assert!(!valid_schedule("* * * *"));
        assert!(!valid_schedule("* * * * * rm"));
        assert!(!valid_schedule("$(id) * * * *"));
        let j = CronJob { id: "abc".into(), project_id: None, user: "www-data".into(), schedule: "0 3 * * *".into(), command: "echo 50% done && echo 'hi'".into(), cwd: "/var/www/x".into(), enabled: true, created_at: String::new() };
        let r = render(&[j.clone(), CronJob { enabled: false, id: "off".into(), ..j }]);
        assert!(r.contains("0 3 * * * www-data cd '/var/www/x' && /usr/local/lib/nexacloud/cron-run abc -- 'echo 50\\% done && echo '\\''hi'\\'''\n"));
        assert!(!r.contains(" off "));
    }
}
