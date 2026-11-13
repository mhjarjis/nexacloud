//! `/api/projects/...` and the public `/hooks/github/{id}` webhook.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;
use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::routes::changes::client_ip;

fn wrap(res: ApiResult<Response>, info: AuditInfo) -> Response {
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn view(state: &AppState, p: &Project) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("deploy_key".into(), json!(public_key(state, &p.id)));
        o.insert("needs_key".into(), json!(needs_key(&p.repo)));
        o.insert(
            "webhook_path".into(),
            json!(format!("/hooks/github/{}", p.id)),
        );
        o.insert("actions_yaml".into(), json!(actions_yaml(p)));
        o.insert(
            "last".into(),
            json!(list_deploys(&state.db, Some(&p.id), 1)
                .ok()
                .and_then(|v| v.into_iter().next())),
        );
    }
    v
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let ps = list_projects(&state.db)?;
    Ok(Json(json!(ps
        .iter()
        .map(|p| view(&state, p))
        .collect::<Vec<_>>())))
}

pub async fn get(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    let mut v = view(&state, &p);
    v["webhook_secret"] = json!(p.webhook_secret);
    v["deploys"] = json!(list_deploys(&state.db, Some(&id), 20)?);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct CreateBody {
    site_id: String,
    repo: String,
    #[serde(default = "main")]
    branch: String,
    #[serde(default = "yes")]
    migrate: bool,
    #[serde(default)]
    auto_deploy: bool,
    run_as: Option<String>,
    script: Option<String>,
    #[serde(default)]
    run_tests: bool,
}
fn main() -> String {
    "main".into()
}
fn yes() -> bool {
    true
}

pub async fn create(State(state): State<AppState>, Json(b): Json<CreateBody>) -> Response {
    let info = AuditInfo::action("project.create", b.site_id.clone());
    let res = async {
        if get_project(&state.db, &b.site_id)?.is_some() {
            return Err(ApiError::conflict("this site is already connected to a repository"));
        }
        if !valid_repo(&b.repo) {
            return Err(ApiError::bad_request("repository must look like git@github.com:owner/repo.git, https://github.com/owner/repo.git or /srv/git/name.git"));
        }
        if !valid_branch(&b.branch) {
            return Err(ApiError::bad_request("invalid branch name"));
        }
        let site = crate::routes::sites::scan(true)
            .await
            .into_iter()
            .find(|s| s.id == b.site_id)
            .ok_or_else(|| ApiError::not_found("site not found"))?;
        let root = site.root.clone().ok_or_else(|| ApiError::bad_request("this site has no document root (proxy/redirect)"))?;
        let kind = if site.kind == "laravel" || root.ends_with("/public") && site.php_version.is_some() && std::path::Path::new(&root).parent().is_some_and(|p| p.join("artisan").exists()) {
            "laravel"
        } else if site.php_version.is_some() {
            "php"
        } else {
            "static"
        };
        let mut dir = if kind == "laravel" || root.ends_with("/public") || root.ends_with("/public_html") {
            std::path::Path::new(&root).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or(root.clone())
        } else {
            root.clone()
        };
        // A web root inside `<dir>/current` means zero-downtime release folders.
        let strategy = if let Some(base) = dir.strip_suffix("/current") {
            dir = base.to_string();
            "releases"
        } else {
            "in_place"
        };
        if !dir.starts_with("/var/www/") && !dir.starts_with("/srv/") && !dir.starts_with("/home/") {
            return Err(ApiError::bad_request(format!("project directory {dir} must be under /var/www, /srv or /home")));
        }
        let run_as = match &b.run_as {
            Some(u) if valid_run_as(u) => u.clone(),
            Some(_) => return Err(ApiError::bad_request("invalid user (and never root)")),
            None => {
                use std::os::unix::fs::MetadataExt;
                let uid = std::fs::metadata(&dir).map(|m| m.uid()).unwrap_or(33);
                let name = crate::util::cmd::Cmd::new("id").args(["-nu", &uid.to_string()]).run().await.stdout.trim().to_string();
                if name.is_empty() || name == "root" { "www-data".into() } else { name }
            }
        };
        let secret: String = {
            use rand::distr::{Alphanumeric, SampleString};
            Alphanumeric.sample_string(&mut rand::rng(), 40)
        };
        let p = Project {
            id: b.site_id.clone(),
            site_id: b.site_id.clone(),
            kind: kind.into(),
            dir,
            repo: b.repo.trim().into(),
            branch: b.branch.clone(),
            run_as,
            script: b.script.clone().unwrap_or_else(|| default_script(kind, b.migrate)),
            migrate: b.migrate,
            auto_deploy: b.auto_deploy,
            webhook_secret: secret,
            created_at: now(),
            last_deploy_at: None,
            last_commit: site.git.as_ref().map(|g| g.commit.clone()),
            strategy: strategy.into(),
            keep_releases: 5,
            run_tests: b.run_tests,
            notify: true,
        };
        if p.script.len() > 20_000 {
            return Err(ApiError::bad_request("deploy script too long"));
        }
        if needs_key(&p.repo) {
            ensure_key(&state, &p.id).await.map_err(|e| ApiError::internal(format!("{e:#}")))?;
        }
        save_project(&state.db, &p)?;
        let mut v = view(&state, &p);
        v["webhook_secret"] = json!(p.webhook_secret);
        Ok(Json(v).into_response())
    }
    .await;
    wrap(res, info)
}

#[derive(Deserialize)]
pub struct UpdateBody {
    repo: Option<String>,
    branch: Option<String>,
    script: Option<String>,
    migrate: Option<bool>,
    auto_deploy: Option<bool>,
    run_as: Option<String>,
    run_tests: Option<bool>,
    notify: Option<bool>,
    keep_releases: Option<u32>,
}

pub async fn update(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> Response {
    let info = AuditInfo::action("project.update", id.clone());
    let res = (|| {
        let mut p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        if let Some(r) = b.repo {
            if !valid_repo(&r) {
                return Err(ApiError::bad_request("invalid repository"));
            }
            p.repo = r;
        }
        if let Some(br) = b.branch {
            if !valid_branch(&br) {
                return Err(ApiError::bad_request("invalid branch"));
            }
            p.branch = br;
        }
        if let Some(s) = b.script {
            if s.len() > 20_000 {
                return Err(ApiError::bad_request("deploy script too long"));
            }
            p.script = s;
        }
        if let Some(u) = b.run_as {
            if !valid_run_as(&u) {
                return Err(ApiError::bad_request("invalid user (and never root)"));
            }
            p.run_as = u;
        }
        if let Some(m) = b.migrate {
            p.migrate = m;
        }
        if let Some(a) = b.auto_deploy {
            p.auto_deploy = a;
        }
        if let Some(t) = b.run_tests {
            p.run_tests = t;
        }
        if let Some(n) = b.notify {
            p.notify = n;
        }
        if let Some(k) = b.keep_releases {
            if !(1..=20).contains(&k) {
                return Err(ApiError::bad_request("keep 1–20 releases"));
            }
            p.keep_releases = k;
        }
        save_project(&state.db, &p)?;
        Ok(Json(view(&state, &p)).into_response())
    })();
    wrap(res, info)
}

pub async fn remove(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("project.delete", id.clone());
    let res = match delete_project(&state.db, &id) {
        Ok(true) => {
            let _ = std::fs::remove_file(key_path(&state, &id));
            let _ = std::fs::remove_file(format!("{}.pub", key_path(&state, &id).display()));
            Ok(
                Json(json!({"ok": true, "note": "the site and its files were not touched"}))
                    .into_response(),
            )
        }
        Ok(false) => Err(ApiError::not_found("project not found")),
        Err(e) => Err(ApiError::from(e)),
    };
    wrap(res, info)
}

#[derive(Deserialize, Default)]
pub struct DeployBody {
    commit: Option<String>,
}

pub async fn deploy(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Response {
    let mut info = AuditInfo::action("project.deploy", id.clone());
    let res = (|| {
        let b: DeployBody = if body.iter().all(u8::is_ascii_whitespace) {
            DeployBody::default()
        } else {
            serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("invalid body"))?
        };
        let p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        let trigger = if b.commit.is_some() {
            "rollback"
        } else {
            "manual"
        };
        let job = submit(&state, &p, trigger, b.commit, Some(client_ip(&addr)))
            .map_err(|e| ApiError::conflict(format!("{e:#}")))?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}

pub async fn deploys(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Vec<Deploy>>> {
    Ok(Json(list_deploys(&state.db, Some(&id), 50)?))
}

pub async fn all_deploys(State(state): State<AppState>) -> ApiResult<Json<Vec<Deploy>>> {
    Ok(Json(list_deploys(&state.db, None, 30)?))
}

// ------------------------------------------------------------------ webhook (public)

pub async fn github_hook(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let res: ApiResult<Response> = (|| {
        let p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("unknown project"))?;
        let sig = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_signature(&p.webhook_secret, &body, sig) {
            return Err(ApiError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "bad signature",
            ));
        }
        let event = headers
            .get("x-github-event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if event == "ping" {
            return Ok(Json(json!({"ok": true, "pong": true})).into_response());
        }
        if event != "push" {
            return Ok(
                Json(json!({"ok": true, "ignored": format!("event {event}")})).into_response(),
            );
        }
        let v: Value =
            serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("invalid JSON"))?;
        let r = v.get("ref").and_then(Value::as_str).unwrap_or("");
        if r != format!("refs/heads/{}", p.branch) {
            return Ok(Json(json!({"ok": true, "ignored": format!("push to {r}")})).into_response());
        }
        match submit(&state, &p, "webhook", None, Some("github".into())) {
            Ok(job) => Ok(Json(json!({"ok": true, "job_id": job})).into_response()),
            Err(e) => Ok(Json(json!({"ok": true, "ignored": format!("{e:#}")})).into_response()),
        }
    })();
    let info = AuditInfo::action("project.webhook", id);
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ project ops

#[derive(Deserialize)]
pub struct LinesQuery {
    lines: Option<usize>,
    /// a source id from `sources` (default: newest app log)
    file: Option<String>,
    /// case-insensitive substring filter
    q: Option<String>,
}

/// Log sources of a project: its storage/logs files, nginx errors for its
/// domain and the PHP-FPM log of its PHP version.
fn log_sources(p: &Project) -> Vec<(String, String, std::path::PathBuf)> {
    let dir = std::path::Path::new(&p.shared_dir()).join("storage/logs");
    let mut files: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.ends_with(".log").then(|| {
                        (
                            e.metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH),
                            n,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    let mut out: Vec<(String, String, std::path::PathBuf)> = files
        .into_iter()
        .map(|(_, n)| (n.clone(), n.clone(), dir.join(&n)))
        .collect();
    if p.kind != "laravel" {
        for n in ["error_log", "php_errors.log"] {
            let f = std::path::Path::new(&p.dir).join(n);
            if f.is_file() {
                out.push((n.into(), n.into(), f));
            }
        }
    }
    out.push((
        "nginx-error".into(),
        format!("nginx error.log ({})", p.site_id),
        "/var/log/nginx/error.log".into(),
    ));
    let php = php_bin(&p.site_id);
    if let Some(v) = php.strip_prefix("php") {
        let f = std::path::PathBuf::from(format!("/var/log/php{v}-fpm.log"));
        if f.exists() {
            out.push((format!("php{v}-fpm"), format!("php{v}-fpm.log"), f));
        }
    }
    out
}

/// Tail of one of the project's logs — read-only.
pub async fn logs(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<LinesQuery>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    let sources = log_sources(&p);
    let n = q.lines.unwrap_or(200).clamp(1, 2000);
    let pick = q
        .file
        .as_ref()
        .and_then(|f| sources.iter().find(|s| &s.0 == f))
        .or_else(|| sources.first());
    let filter = q.q.as_deref().map(str::to_lowercase).filter(|f| !f.is_empty());
    let text = pick
        .map(|(sid, _, path)| {
            let budget = if filter.is_some() || sid == "nginx-error" { 4_000_000 } else { (n * 400) as u64 };
            let raw = crate::jobs::read_log_tail(path, budget);
            let lines: Vec<&str> = raw
                .lines()
                .filter(|l| sid != "nginx-error" || l.contains(&format!("server: {},", p.site_id)))
                .filter(|l| filter.as_ref().is_none_or(|f| l.to_lowercase().contains(f)))
                .collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "sources": sources.iter().map(|s| json!({"id": s.0, "label": s.1})).collect::<Vec<_>>(),
        "files": sources.iter().map(|s| s.0.clone()).collect::<Vec<_>>(),
        "file": pick.map(|s| s.0.clone()),
        "text": text,
    })))
}

#[derive(Deserialize)]
pub struct FreshQuery {
    fresh: Option<bool>,
}

/// Health, workers, schedule, errors and response times for the project page.
pub async fn overview(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<FreshQuery>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(
        crate::monitor::project::overview(&state, &p, q.fresh.unwrap_or(false)).await,
    ))
}

#[derive(Deserialize)]
pub struct RevealBody {
    key: String,
}

/// One secret `.env` value, on purpose (sudo, audited). Never logged.
pub async fn env_reveal(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RevealBody>,
) -> Response {
    let info = AuditInfo::action("project.env_reveal", format!("{id}: {}", b.key));
    let res = (|| {
        let p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        let path = std::path::Path::new(&p.shared_dir()).join(".env");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let v = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == b.key)
            .map(|(_, v)| {
                let v = v.trim();
                v.strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .map(|x| x.replace("\\\"", "\""))
                    .unwrap_or_else(|| v.to_string())
            })
            .ok_or_else(|| ApiError::not_found("no such variable"))?;
        Ok(Json(json!({"key": b.key, "value": v})).into_response())
    })();
    wrap(res, info)
}

#[derive(Deserialize)]
pub struct RestartBody {
    /// worker name; all workers when absent
    name: Option<String>,
}

pub async fn workers_restart(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RestartBody>,
) -> Response {
    let info = AuditInfo::action(
        "project.workers_restart",
        format!("{id}: {}", b.name.as_deref().unwrap_or("all")),
    );
    let res = async {
        let p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        let procs = crate::monitor::project::supervisor_status(&p.id).await;
        let targets: Vec<String> = match &b.name {
            None => vec![format!("nexacloud-{}:*", sup_name(&p.id))],
            Some(n) => {
                let base = format!("nexacloud-{}", sup_name(&p.id));
                let prog = if n == "default" && procs.iter().any(|s| s.program == base) {
                    base
                } else {
                    format!("{base}-{n}")
                };
                procs
                    .iter()
                    .filter(|s| s.program == prog)
                    .map(|s| s.name.clone())
                    .collect()
            }
        };
        if targets.is_empty() {
            return Err(ApiError::not_found("no such worker"));
        }
        let o = crate::util::cmd::Cmd::new("supervisorctl")
            .arg("restart")
            .args(&targets)
            .timeout(Duration::from_secs(120))
            .run()
            .await;
        if !o.ok {
            return Err(ApiError::internal(o.error_summary("supervisorctl restart")));
        }
        crate::monitor::project::invalidate(&p.id);
        Ok(Json(json!({"restarted": targets})).into_response())
    }
    .await;
    wrap(res, info)
}

const ARTISAN_ALLOW: &[&str] = &[
    "about",
    "migrate",
    "migrate:status",
    "optimize",
    "optimize:clear",
    "cache:clear",
    "config:cache",
    "config:clear",
    "route:cache",
    "route:clear",
    "view:cache",
    "view:clear",
    "queue:restart",
    "queue:failed",
    "queue:retry all",
    "storage:link",
    "down",
    "up",
    "schedule:list",
    "event:cache",
];

#[derive(Deserialize)]
pub struct ArtisanBody {
    command: String,
}

pub async fn artisan(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<ArtisanBody>,
) -> Response {
    let mut info = AuditInfo::action("project.artisan", format!("{id}: {}", b.command));
    let res = (|| {
        let p =
            get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        if p.kind != "laravel" {
            return Err(ApiError::bad_request("not a Laravel project"));
        }
        let cmd = b.command.trim().to_string();
        if !ARTISAN_ALLOW.contains(&cmd.as_str()) {
            return Err(ApiError::bad_request(format!(
                "allowed: {}",
                ARTISAN_ALLOW.join(", ")
            )));
        }
        let mut args: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        if args[0] == "migrate" {
            args.push("--force".into());
        }
        let spec = JobSpec::new("artisan", &format!("php artisan {cmd} ({id})"))
            .lock(format!("site:{}", p.site_id))
            .timeout(Duration::from_secs(600))
            .actor(Some(client_ip(&addr)));
        let job = state.jobs.submit(spec, move |ctx| async move {
            let script = format!(
                "{} artisan {}",
                php_bin(&p.site_id),
                args.iter()
                    .map(|a| format!("'{a}'"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            as_user(&p, &ctx, &[], &script, Duration::from_secs(600)).await
        })?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}

pub fn artisan_allowlist() -> Vec<&'static str> {
    ARTISAN_ALLOW.to_vec()
}

/// `.env` with secret values masked.
pub async fn env_get(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    let path = std::path::Path::new(&p.shared_dir()).join(".env");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let vars: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let k = k.trim();
            let secret = ["PASSWORD", "SECRET", "KEY", "TOKEN"].iter().any(|s| k.contains(s));
            json!({"key": k, "value": if secret { Value::Null } else { json!(v.trim()) }, "secret": secret})
        })
        .collect();
    Ok(Json(
        json!({"path": path, "exists": path.exists(), "vars": vars}),
    ))
}

pub async fn status(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    let worker =
        std::path::Path::new(&format!("/etc/supervisor/conf.d/nexacloud-{id}.conf")).exists();
    let sched = std::path::Path::new(&format!("/etc/cron.d/{}", cron_name(&id))).exists();
    let maintenance = std::path::Path::new(&p.app_dir())
        .join("storage/framework/down")
        .exists();
    let workers = if worker {
        crate::util::cmd::Cmd::new("supervisorctl")
            .args(["status", &format!("nexacloud-{}:*", sup_name(&id))])
            .run()
            .await
            .stdout
    } else {
        String::new()
    };
    Ok(Json(
        json!({"worker": worker, "worker_status": workers, "scheduler": sched, "maintenance": maintenance, "artisan": artisan_allowlist()}),
    ))
}

pub fn sup_name(id: &str) -> String {
    id.replace('.', "-")
}
pub fn cron_name(id: &str) -> String {
    format!("nexacloud-{}", id.replace('.', "-"))
}

/// Release folders of a project (newest first).
pub async fn releases(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(json!({
        "strategy": p.strategy,
        "releases": if p.strategy == "releases" { super::releases::list(&p) } else { Vec::new() },
    })))
}

/// Make an older release live again (a deploy with trigger "rollback" that
/// only switches the `current` link and checks the site).
pub async fn release_activate(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath((id, name)): UrlPath<(String, String)>,
) -> Response {
    let mut info = AuditInfo::action("project.release_activate", format!("{id}: {name}"));
    let res = (|| {
        let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
        let r = super::releases::list(&p)
            .into_iter()
            .find(|r| r.name == name)
            .ok_or_else(|| ApiError::not_found("no such release"))?;
        if r.current {
            return Err(ApiError::conflict("that release is already live"));
        }
        if r.meta.commit.len() < 7 {
            return Err(ApiError::conflict("this release has no commit recorded"));
        }
        let job = submit(&state, &p, "rollback", Some(r.meta.commit.clone()), Some(client_ip(&addr)))
            .map_err(|e| ApiError::conflict(format!("{e:#}")))?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job})).into_response())
    })();
    wrap(res, info)
}
