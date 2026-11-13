//! HTTP API for backups: `/api/backup/...` and `/api/disk`.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;
use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::routes::changes::client_ip;
use crate::state::AppState;

fn wrap(res: ApiResult<Response>, info: AuditInfo) -> Response {
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ destinations

pub async fn destinations(State(state): State<AppState>) -> ApiResult<Json<Vec<Destination>>> {
    Ok(Json(
        list_destinations(&state.db)?.iter().map(redacted).collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewDestination {
    name: String,
    kind: String,
    config: Value,
}

pub async fn create_destination(
    State(state): State<AppState>,
    Json(b): Json<NewDestination>,
) -> Response {
    let info = AuditInfo::action("backup.destination.create", b.name.clone());
    let res = (|| {
        if !valid_name(&b.name) {
            return Err(ApiError::bad_request("give the destination a name"));
        }
        validate_destination(&b.kind, &b.config).map_err(ApiError::bad_request)?;
        let d = Destination {
            id: uuid::Uuid::new_v4().to_string(),
            name: b.name.trim().to_string(),
            kind: b.kind.clone(),
            config: b.config.clone(),
            created_at: crate::util::fmt_ts(chrono::Utc::now()),
        };
        insert_destination(&state.db, &d)?;
        Ok(Json(redacted(&d)).into_response())
    })();
    wrap(res, info)
}

pub async fn delete_destination_route(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let info = AuditInfo::action("backup.destination.delete", id.clone());
    let res = match delete_destination(&state.db, &id) {
        Ok(true) => Ok(Json(json!({"ok": true})).into_response()),
        Ok(false) => Err(ApiError::not_found("destination not found")),
        Err(e) => Err(ApiError::conflict(format!("{e:#}"))),
    };
    wrap(res, info)
}

pub async fn test_destination_route(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let mut info = AuditInfo::action("backup.destination.test", id.clone());
    let res = (|| {
        let d = get_destination(&state.db, &id)?
            .ok_or_else(|| ApiError::not_found("destination not found"))?;
        let spec = JobSpec::new(
            "backup_test",
            &format!("Test backup destination {}", d.name),
        )
        .timeout(Duration::from_secs(600))
        .actor(Some(client_ip(&addr)));
        let job = state.jobs.submit(
            spec,
            move |ctx| async move { test_destination(&d, &ctx).await },
        )?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}

pub async fn ssh_key() -> ApiResult<Json<Value>> {
    let key = ensure_ssh_key()
        .await
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(Json(json!({"public_key": key})))
}

// ------------------------------------------------------------------ plans

pub async fn plans(State(state): State<AppState>) -> ApiResult<Json<Vec<Plan>>> {
    Ok(Json(list_plans(&state.db)?))
}

#[derive(Deserialize)]
pub struct PlanBody {
    name: String,
    destination_id: Option<String>,
    #[serde(default)]
    databases: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    schedule: String,
    #[serde(default = "seven")]
    keep_daily: u32,
    #[serde(default = "four")]
    keep_weekly: u32,
    #[serde(default = "three")]
    keep_monthly: u32,
    #[serde(default = "yes")]
    enabled: bool,
}
fn seven() -> u32 {
    7
}
fn four() -> u32 {
    4
}
fn three() -> u32 {
    3
}
fn yes() -> bool {
    true
}

fn validate_plan(state: &AppState, b: &PlanBody) -> Result<(), ApiError> {
    if !valid_name(&b.name) {
        return Err(ApiError::bad_request("give the plan a name"));
    }
    validate_schedule(&b.schedule).map_err(ApiError::bad_request)?;
    if b.databases.is_empty() && b.paths.is_empty() {
        return Err(ApiError::bad_request(
            "choose at least one database or folder",
        ));
    }
    for d in &b.databases {
        let ok = d == "all"
            || d.split_once('/').is_some_and(|(e, n)| {
                matches!(e, "mysql" | "postgres") && crate::util::is_valid_db_name(n)
            });
        if !ok {
            return Err(ApiError::bad_request(format!("invalid database {d:?}")));
        }
    }
    for p in &b.paths {
        if !valid_folder(p) {
            return Err(ApiError::bad_request(format!(
                "folder {p:?} must be under /var/www, /etc, /home, /srv or /opt"
            )));
        }
    }
    if b.keep_daily + b.keep_weekly + b.keep_monthly == 0
        || b.keep_daily > 90
        || b.keep_weekly > 52
        || b.keep_monthly > 36
    {
        return Err(ApiError::bad_request(
            "retention must keep at least one backup (max 90 daily / 52 weekly / 36 monthly)",
        ));
    }
    if let Some(d) = &b.destination_id {
        if get_destination(&state.db, d).ok().flatten().is_none() {
            return Err(ApiError::bad_request("unknown destination"));
        }
    }
    Ok(())
}

pub async fn create_plan(State(state): State<AppState>, Json(b): Json<PlanBody>) -> Response {
    let info = AuditInfo::action("backup.plan.create", b.name.clone());
    let res = (|| {
        validate_plan(&state, &b)?;
        let p = Plan {
            id: uuid::Uuid::new_v4().to_string(),
            name: b.name.trim().into(),
            destination_id: b.destination_id.clone(),
            databases: b.databases.clone(),
            paths: b.paths.clone(),
            schedule: b.schedule.clone(),
            keep_daily: b.keep_daily,
            keep_weekly: b.keep_weekly,
            keep_monthly: b.keep_monthly,
            enabled: b.enabled,
            last_run_at: None,
            created_at: crate::util::fmt_ts(chrono::Utc::now()),
        };
        upsert_plan(&state.db, &p)?;
        Ok(Json(p).into_response())
    })();
    wrap(res, info)
}

pub async fn update_plan(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<PlanBody>,
) -> Response {
    let info = AuditInfo::action("backup.plan.update", id.clone());
    let res = (|| {
        let mut p =
            get_plan(&state.db, &id)?.ok_or_else(|| ApiError::not_found("plan not found"))?;
        validate_plan(&state, &b)?;
        p.name = b.name.trim().into();
        p.destination_id = b.destination_id.clone();
        p.databases = b.databases.clone();
        p.paths = b.paths.clone();
        p.schedule = b.schedule.clone();
        p.keep_daily = b.keep_daily;
        p.keep_weekly = b.keep_weekly;
        p.keep_monthly = b.keep_monthly;
        p.enabled = b.enabled;
        upsert_plan(&state.db, &p)?;
        Ok(Json(p).into_response())
    })();
    wrap(res, info)
}

pub async fn delete_plan_route(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let info = AuditInfo::action("backup.plan.delete", id.clone());
    let res = match delete_plan(&state.db, &id) {
        Ok(true) => Ok(Json(json!({"ok": true})).into_response()),
        Ok(false) => Err(ApiError::not_found("plan not found")),
        Err(e) => Err(ApiError::from(e)),
    };
    wrap(res, info)
}

pub async fn run_plan(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let mut info = AuditInfo::action("backup.run", id.clone());
    let res = (|| {
        let p = get_plan(&state.db, &id)?.ok_or_else(|| ApiError::not_found("plan not found"))?;
        let job = submit_run(&state, &p, Some(client_ip(&addr)))?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}

// ------------------------------------------------------------------ runs

#[derive(Deserialize)]
pub struct RunsQuery {
    plan: Option<String>,
    limit: Option<usize>,
}

pub async fn runs(
    State(state): State<AppState>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Vec<Run>>> {
    Ok(Json(list_runs(
        &state.db,
        q.plan.as_deref(),
        q.limit.unwrap_or(50),
    )?))
}

pub async fn restore_run(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RestoreBody>,
) -> Response {
    let mut info = AuditInfo::action("backup.restore", format!("{id}:{}", b.source));
    let res = (|| {
        let r =
            get_run(&state.db, &id)?.ok_or_else(|| ApiError::not_found("backup run not found"))?;
        if r.status != "ok" {
            return Err(ApiError::conflict(
                "only successful backups can be restored",
            ));
        }
        let (job, target) = submit_restore(&state, &r, &b.source, Some(client_ip(&addr)))
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job, "target_database": target})).into_response())
    })();
    wrap(res, info)
}

pub async fn disk(State(state): State<AppState>) -> Json<Vec<DiskUsage>> {
    Json(disk_usage(&state.config.backup_dir).await)
}

/// `POST /api/backup/runs/{id}/staging` — restore a site into a staging copy.
pub async fn staging_run(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<super::staging::StagingBody>,
) -> Response {
    let mut info = AuditInfo::action("backup.staging", format!("{id}:{} → {}", b.files, b.domain));
    let res = (|| {
        let r = get_run(&state.db, &id)?.ok_or_else(|| ApiError::not_found("backup run not found"))?;
        super::staging::validate(&state, &r, &b).map_err(ApiError::bad_request)?;
        let job = super::staging::submit(&state, &r, b, Some(client_ip(&addr)))
            .map_err(|e| ApiError::conflict(format!("{e:#}")))?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}
