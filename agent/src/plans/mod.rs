//! Every server change the panel can make, behind one API:
//!
//! - `POST /api/plans/preview {"kind": "...", "params": {...}}` → change preview
//!   (diffs, commands, plan_hash). Read-only.
//! - `POST /api/plans/apply {"kind": "...", "params": {...}, "plan_hash": "..."}`
//!   (sudo) → rebuilds the plan on the server, refuses if the hash differs from
//!   the preview, then runs it through the change pipeline (snapshot → apply →
//!   verify → auto-revert). Returns `{change_id, job_id, plan_hash}`.

pub mod projects;
pub mod runtime;
pub mod sites;
pub mod system;

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit::{audited, AuditInfo};
use crate::changes::{self, ChangeEnv, ChangePlan};
use crate::error::{ApiError, ApiResult};
use crate::routes::changes::client_ip;
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum PlanRequest {
    SiteCreate(sites::CreateParams),
    SiteDelete { id: String },
    SiteOptions(sites::OptionsParams),
    SitePhp { id: String, version: String },
    SiteSsl(sites::SslParams),
    NginxRaw(sites::RawParams),
    PhpInstall { version: String },
    PhpExtensions(system::ExtParams),
    PhpIni(system::IniParams),
    PhpCli { version: String },
    ServiceInstall { id: String },
    AdvisorFix { rule: String },
    DiskCleanup { action: String },
    ProjectEnv(projects::EnvParams),
    ProjectWorker(projects::WorkerParams),
    ProjectScheduler { id: String, enabled: bool },
    SiteRuntime(runtime::RuntimeParams),
    SiteAdopt(sites::AdoptParams),
    FirewallEnable(runtime::FirewallParams),
    SiteIsolate { id: String },
    SiteRoot { id: String, root: String },
    ProjectEnvRaw(projects::EnvRawParams),
    ProjectShared { id: String, name: String },
    ProjectPermissions { id: String },
    ProjectReleases { id: String },
}

impl PlanRequest {
    fn name(&self) -> &'static str {
        match self {
            PlanRequest::SiteCreate(_) => "site_create",
            PlanRequest::SiteDelete { .. } => "site_delete",
            PlanRequest::SiteOptions(_) => "site_options",
            PlanRequest::SitePhp { .. } => "site_php",
            PlanRequest::SiteSsl(_) => "site_ssl",
            PlanRequest::NginxRaw(_) => "nginx_raw",
            PlanRequest::PhpInstall { .. } => "php_install",
            PlanRequest::PhpExtensions(_) => "php_extensions",
            PlanRequest::PhpIni(_) => "php_ini",
            PlanRequest::PhpCli { .. } => "php_cli",
            PlanRequest::ServiceInstall { .. } => "service_install",
            PlanRequest::AdvisorFix { .. } => "advisor_fix",
            PlanRequest::DiskCleanup { .. } => "disk_cleanup",
            PlanRequest::ProjectEnv(_) => "project_env",
            PlanRequest::ProjectWorker(_) => "project_worker",
            PlanRequest::ProjectScheduler { .. } => "project_scheduler",
            PlanRequest::SiteRuntime(_) => "site_runtime",
            PlanRequest::SiteAdopt(_) => "site_adopt",
            PlanRequest::FirewallEnable(_) => "firewall_enable",
            PlanRequest::SiteIsolate { .. } => "site_isolate",
            PlanRequest::SiteRoot { .. } => "site_root",
            PlanRequest::ProjectEnvRaw(_) => "project_env",
            PlanRequest::ProjectShared { .. } => "project_shared",
            PlanRequest::ProjectPermissions { .. } => "project_permissions",
            PlanRequest::ProjectReleases { .. } => "project_releases",
        }
    }
}

pub async fn build(state: &AppState, req: &PlanRequest) -> Result<ChangePlan, String> {
    let env = &state.changes_env;
    let project = |id: &str| {
        crate::deploys::get_project(&state.db, id)
            .ok()
            .flatten()
            .ok_or_else(|| {
                format!("project {id} not found — connect the site to a repository first")
            })
    };
    match req {
        PlanRequest::ProjectEnv(p) => projects::plan_env(env, &project(&p.id)?, p),
        PlanRequest::ProjectWorker(w) => projects::plan_worker(env, &project(&w.id)?, w),
        PlanRequest::ProjectScheduler { id, enabled } => {
            projects::plan_scheduler(env, &project(id)?, *enabled)
        }
        PlanRequest::SiteCreate(p) => sites::plan_create(env, p),
        PlanRequest::SiteIsolate { id } => {
            let p = crate::deploys::get_project(&state.db, id).ok().flatten();
            sites::plan_isolate(env, id, p.as_ref())
        }
        PlanRequest::SiteRoot { id, root } => sites::plan_root(env, id, root),
        PlanRequest::ProjectPermissions { id } => projects::plan_permissions(env, &project(id)?),
        PlanRequest::ProjectShared { id, name } => projects::plan_shared(env, &project(id)?, name),
        PlanRequest::ProjectEnvRaw(p) => projects::plan_env_raw(env, &project(&p.id)?, p),
        PlanRequest::ProjectReleases { id } => projects::plan_releases(env, &project(id)?),
        PlanRequest::SiteRuntime(p) => runtime::plan_runtime(env, p),
        PlanRequest::SiteAdopt(p) => sites::plan_adopt(env, p),
        PlanRequest::FirewallEnable(p) => {
            let port = state
                .config
                .listen
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8443);
            runtime::plan_firewall(env, port, p)
        }
        PlanRequest::SiteDelete { id } => sites::plan_delete(env, id),
        PlanRequest::SiteOptions(p) => sites::plan_options(env, p),
        PlanRequest::SitePhp { id, version } => sites::plan_php_switch(env, id, version),
        PlanRequest::SiteSsl(p) => sites::plan_ssl(env, p),
        PlanRequest::NginxRaw(p) => sites::plan_raw(env, p),
        PlanRequest::PhpInstall { version } => system::plan_php_install(env, version),
        PlanRequest::PhpExtensions(p) => system::plan_php_extensions(env, p),
        PlanRequest::PhpIni(p) => system::plan_php_ini(env, p),
        PlanRequest::PhpCli { version } => system::plan_php_cli(env, version),
        PlanRequest::ServiceInstall { id } => system::plan_service_install(env, id),
        PlanRequest::AdvisorFix { rule } => crate::advisor::rules::fix_plan(env, rule).await,
        PlanRequest::DiskCleanup { action } => disk_cleanup(env, action),
    }
}

fn disk_cleanup(env: &ChangeEnv, action: &str) -> Result<ChangePlan, String> {
    let b = match action {
        "apt-cache" => {
            ChangePlan::builder("disk_cleanup", "Clear the package download cache", "disk")
                .lock("apt")
                .command(["apt-get", "clean"])
        }
        "journal" => ChangePlan::builder("disk_cleanup", "Shrink system logs to 200 MB", "disk")
            .command(["journalctl", "--vacuum-size=200M"]),
        "old-kernels" => ChangePlan::builder(
            "disk_cleanup",
            "Remove unused packages and old kernels",
            "disk",
        )
        .lock("apt")
        .command_timeout(["apt-get", "autoremove", "-y", "-q"], 900),
        "dev-caches" => ChangePlan::builder("disk_cleanup", "Clear Composer and npm download caches", "disk")
            .command([
                "sh",
                "-c",
                "rm -rf /tmp/nexacloud-composer-* /root/.cache/composer /root/.npm/_cacache; for d in /var/www/*/.npm/_cacache /var/www/*/.cache/composer; do rm -rf \"$d\"; done; true",
            ]),
        _ => return Err("unknown cleanup action".into()),
    };
    b.note("This cannot be rolled back (it only deletes caches and unused files).")
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Deserialize)]
struct ApplyEnvelope {
    #[serde(flatten)]
    req: PlanRequest,
    plan_hash: Option<String>,
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid request: {e}")))
}

pub async fn preview(State(state): State<AppState>, body: Bytes) -> Response {
    let res: ApiResult<Response> = async {
        let req: PlanRequest = parse(&body)?;
        let plan = build(&state, &req).await.map_err(ApiError::conflict)?;
        Ok(Json(changes::preview(&plan)).into_response())
    }
    .await;
    audited(
        res.unwrap_or_else(IntoResponse::into_response),
        AuditInfo::skip(),
    )
}

pub async fn apply(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let mut info = AuditInfo::action("change.apply", "plan");
    let res: ApiResult<Response> = async {
        let env: ApplyEnvelope = parse(&body)?;
        info = AuditInfo::action(&format!("plan.{}", env.req.name()), "server");
        let plan = build(&state, &env.req).await.map_err(ApiError::conflict)?;
        info = AuditInfo::action(&format!("plan.{}", env.req.name()), plan.target.clone());
        if plan.is_noop() {
            return Err(ApiError::conflict(
                plan.note.clone().unwrap_or_else(|| "nothing to do".into()),
            ));
        }
        let hash = plan.hash();
        info.plan_hash = Some(hash.clone());
        if let Some(expected) = env.plan_hash {
            if expected != hash {
                return Err(ApiError::conflict(
                    "the server changed since the preview; preview again",
                ));
            }
        }
        let (change_id, job_id) = changes::submit_apply(
            &state.jobs,
            &state.db,
            &state.changes_env,
            plan,
            Some(client_ip(&addr)),
        )?;
        info.change_id = Some(change_id.clone());
        info.job_id = Some(job_id.clone());
        Ok(
            Json(json!({"change_id": change_id, "job_id": job_id, "plan_hash": hash}))
                .into_response(),
        )
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ reads

pub async fn site_model(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<sites::SiteModel>> {
    sites::load_model(&state.changes_env, &id)
        .map(Json)
        .map_err(ApiError::not_found)
}

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

/// `GET /api/nginx/file?path=/etc/nginx/...` → `{path, real_path, content}`.
pub async fn nginx_file(
    State(state): State<AppState>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<Value>> {
    let real =
        sites::resolve_nginx_file(&state.changes_env, &q.path).map_err(ApiError::bad_request)?;
    let content = std::fs::read_to_string(state.changes_env.host_path(&real))
        .map_err(|e| ApiError::bad_request(format!("{e}")))?;
    Ok(Json(
        json!({"path": q.path, "real_path": real, "content": content}),
    ))
}

/// `GET /api/php/{version}/config` → extensions + our ini drop-in values.
pub async fn php_config(
    State(state): State<AppState>,
    UrlPath(v): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    if !sites::valid_php_version(&v) {
        return Err(ApiError::bad_request("invalid version"));
    }
    let env = &state.changes_env;
    if !system::installed_php(env).contains(&v) {
        return Err(ApiError::not_found(format!("PHP {v} is not installed")));
    }
    let modules = crate::util::cmd::Cmd::new(&format!("php{v}"))
        .arg("-m")
        .timeout(std::time::Duration::from_secs(10))
        .run()
        .await;
    let loaded: Vec<String> = modules
        .stdout
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty() && !l.starts_with('['))
        .collect();
    let ini_keys: Vec<Value> = system::INI_KEYS
        .iter()
        .map(|(k, t)| json!({"key": k, "type": t}))
        .collect();
    Ok(Json(json!({
        "version": v,
        "loaded_modules": loaded,
        "optional_extensions": system::OPTIONAL_EXTENSIONS,
        "ini": system::read_ini(env, &v),
        "ini_keys": ini_keys,
    })))
}

pub async fn catalog(State(state): State<AppState>) -> Json<Vec<system::CatalogItem>> {
    Json(system::catalog(&state.changes_env))
}

pub async fn advisor(State(state): State<AppState>) -> Json<crate::advisor::rules::Report> {
    Json(crate::advisor::rules::evaluate(&state.changes_env, &state.db).await)
}

pub async fn advisor_dismiss(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<crate::advisor::rules::DismissBody>,
) -> Response {
    let info = AuditInfo::action("advisor.dismiss", id.clone());
    let res = if id.len() > 80
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
    {
        Err(ApiError::bad_request("invalid id"))
    } else {
        crate::advisor::rules::dismiss(&state.db, &id, b.days)
            .map(|_| Json(json!({"ok": true})).into_response())
            .map_err(ApiError::from)
    };
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

/// `GET /api/firewall/guard` — is the auto-disable timer armed?
pub async fn firewall_guard() -> Json<Value> {
    let o = crate::util::cmd::Cmd::new("systemctl")
        .args(["is-active", &format!("{}.timer", runtime::GUARD_UNIT)])
        .run()
        .await;
    Json(json!({"armed": o.stdout.trim() == "active"}))
}

/// `POST /api/firewall/confirm` (sudo) — keep the firewall on: disarm the guard.
pub async fn firewall_confirm() -> Response {
    let info = AuditInfo::action("firewall.confirm", "ufw");
    let o = crate::util::cmd::Cmd::new("systemctl")
        .args(["stop", &format!("{}.timer", runtime::GUARD_UNIT)])
        .run()
        .await;
    let res = if o.ok {
        Json(json!({"ok": true})).into_response()
    } else {
        ApiError::conflict(format!(
            "could not stop the guard timer: {}",
            o.error_summary("systemctl")
        ))
        .into_response()
    };
    audited(res, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        let r: PlanRequest = serde_json::from_value(
            json!({"kind":"site_php","params":{"id":"a.com","version":"8.4"}}),
        )
        .unwrap();
        assert_eq!(r.name(), "site_php");
        let r: ApplyEnvelope = serde_json::from_value(
            json!({"kind":"php_install","params":{"version":"8.4"},"plan_hash":"x"}),
        )
        .unwrap();
        assert_eq!(r.plan_hash.as_deref(), Some("x"));
        assert!(
            serde_json::from_value::<PlanRequest>(json!({"kind":"rm_rf","params":{}})).is_err()
        );
    }
}
