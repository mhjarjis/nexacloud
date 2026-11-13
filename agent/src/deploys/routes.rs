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
        let dir = if kind == "laravel" || root.ends_with("/public") || root.ends_with("/public_html") {
            std::path::Path::new(&root).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or(root.clone())
        } else {
            root.clone()
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
    file: Option<String>,
}

/// Tail of a Laravel log (storage/logs/*.log) — read-only.
pub async fn logs(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<LinesQuery>,
) -> ApiResult<Json<Value>> {
    let p = get_project(&state.db, &id)?.ok_or_else(|| ApiError::not_found("project not found"))?;
    let dir = std::path::Path::new(&p.dir).join("storage/logs");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".log"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    let pick = q
        .file
        .filter(|f| files.contains(f))
        .or_else(|| files.first().cloned());
    let text = pick
        .as_ref()
        .map(|f| {
            crate::jobs::read_log_tail(
                &dir.join(f),
                (q.lines.unwrap_or(200).min(2000) * 300) as u64,
            )
        })
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let n = q.lines.unwrap_or(200).min(2000);
    let tail = lines[lines.len().saturating_sub(n)..].join("\n");
    Ok(Json(json!({"files": files, "file": pick, "text": tail})))
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
                "php artisan {}",
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
    let path = std::path::Path::new(&p.dir).join(".env");
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
    let maintenance = std::path::Path::new(&p.dir)
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
