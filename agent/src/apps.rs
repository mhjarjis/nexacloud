//! One-step "New app": nginx site with its own Linux user and PHP pool,
//! a database and user, a production `.env`, the Git project with
//! zero-downtime release folders, optional HTTPS — then the first deploy.
//! Queue worker and scheduler are switched on after that first deploy
//! succeeds (they need the code).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use axum::extract::{ConnectInfo, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::changes;
use crate::deploys::{self, Project};
use crate::error::{ApiError, ApiResult};
use crate::jobs::{JobCtx, JobSpec};
use crate::plans::sites::{self, SiteKind};
use crate::routes::changes::client_ip;
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NewApp {
    /// laravel | php
    pub kind: String,
    pub domain: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub php_version: String,
    pub repo: String,
    #[serde(default = "main")]
    pub branch: String,
    /// mysql | postgres | none
    #[serde(default = "mysql")]
    pub database: String,
    #[serde(default = "yes")]
    pub isolate: bool,
    #[serde(default = "yes")]
    pub releases: bool,
    /// Let's Encrypt contact; HTTPS is skipped when empty.
    #[serde(default)]
    pub ssl_email: String,
    #[serde(default = "yes")]
    pub worker: bool,
    #[serde(default = "yes")]
    pub scheduler: bool,
    #[serde(default)]
    pub auto_deploy: bool,
    /// Extra .env values.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Folder (inside the code) the web server serves; default `public`.
    #[serde(default)]
    pub web_root: Option<String>,
    /// Nightly backup of the new database.
    #[serde(default)]
    pub backup: bool,
    /// Run `npm ci && npm run build` on deploy (when package.json has a build script).
    #[serde(default = "yes")]
    pub npm_build: bool,
    /// Run `composer install` on deploy.
    #[serde(default = "yes")]
    pub composer: bool,
}
fn main() -> String {
    "main".into()
}
fn mysql() -> String {
    "mysql".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PendingSetup {
    worker: bool,
    scheduler: bool,
}

/// `crm.example.com` → `crm_example_com` (max 32 chars, MySQL user limit).
pub fn db_name(domain: &str) -> String {
    let mut s: String = domain
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert_str(0, "db_");
    }
    if s.len() > 32 {
        let h = crate::util::sha256_hex(domain.as_bytes());
        s = format!("{}_{}", &s[..24], &h[..7]);
    }
    s
}

fn random_key() -> String {
    use rand::RngExt;
    let mut b = [0u8; 32];
    rand::rng().fill(&mut b);
    use std::fmt::Write;
    // base64 without pulling in a crate
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in b.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                let _ = write!(out, "{}", T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    format!("base64:{out}")
}

fn validate(state: &AppState, a: &NewApp) -> Result<(), String> {
    if !["laravel", "php", "static"].contains(&a.kind.as_str()) {
        return Err("type must be Laravel, plain PHP or static".into());
    }
    if let Some(w) = &a.web_root {
        let w = w.trim().trim_matches('/');
        if w.contains("..") || !w.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b)) {
            return Err("web root: a folder like public or public_html".into());
        }
    }
    if !sites::valid_domain(&a.domain) {
        return Err("enter a domain like app.example.com".into());
    }
    if !deploys::valid_repo(&a.repo) {
        return Err("repository: git@github.com:owner/repo.git, https://… or /srv/git/…".into());
    }
    if !deploys::valid_branch(&a.branch) {
        return Err("invalid branch name".into());
    }
    if !["mysql", "postgres", "none"].contains(&a.database.as_str()) {
        return Err("database must be mysql, postgres or none".into());
    }
    if !a.ssl_email.is_empty() && !crate::monitor::notify::valid_email(&a.ssl_email) {
        return Err("the HTTPS email address is not valid".into());
    }
    if deploys::get_project(&state.db, &a.domain).ok().flatten().is_some() {
        return Err(format!("{} is already connected to a repository", a.domain));
    }
    for k in a.env.keys() {
        if !crate::plans::projects::valid_env_key(k) {
            return Err(format!("invalid .env name {k:?}"));
        }
    }
    Ok(())
}

/// `POST /api/apps` → `{job_id, project_id}`.
pub async fn create(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(a): Json<NewApp>,
) -> Response {
    let mut info = AuditInfo::action("app.create", a.domain.clone());
    let res: ApiResult<Response> = async {
        let mut a = a;
        a.domain = a.domain.trim().to_ascii_lowercase();
        a.repo = a.repo.trim().to_string();
        validate(&state, &a).map_err(ApiError::bad_request)?;
        // Dry-run the site plan now so obvious problems (domain taken, PHP
        // missing) are reported before anything is created.
        sites::plan_create(&state.changes_env, &site_params(&a)).map_err(ApiError::conflict)?;
        let spec = JobSpec::new("app_create", &format!("New app {}", a.domain))
            .lock(format!("site:{}", a.domain))
            .timeout(Duration::from_secs(1800))
            .actor(Some(client_ip(&addr)));
        let st = state.clone();
        let a2 = a.clone();
        let job = state
            .jobs
            .submit(spec, move |ctx| async move { build(&st, &ctx, &a2).await })?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job, "project_id": a.domain})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn site_params(a: &NewApp) -> sites::CreateParams {
    let base = format!("/var/www/{}", a.domain);
    let app = if a.releases { format!("{base}/current") } else { base };
    let web = a
        .web_root
        .as_deref()
        .map(|w| w.trim().trim_matches('/').to_string())
        .unwrap_or_else(|| if a.kind == "static" { String::new() } else { "public".into() });
    let root = if web.is_empty() { app } else { format!("{app}/{web}") };
    sites::CreateParams {
        kind: match a.kind.as_str() {
            "laravel" => SiteKind::Laravel,
            "static" => SiteKind::Static,
            _ => SiteKind::Php,
        },
        domain: a.domain.clone(),
        aliases: a.aliases.clone(),
        root: Some(root),
        php_version: (a.kind != "static").then(|| a.php_version.clone()),
        upstream: None,
        redirect_to: None,
        redirect_code: None,
        options: None,
        isolate: a.isolate && a.kind != "static",
        no_placeholder: a.releases,
    }
}

async fn apply(state: &AppState, job: &JobCtx, plan: changes::ChangePlan) -> Result<()> {
    changes::apply_now(&state.db, &state.changes_env, plan, "new-app wizard", &job.log)
        .await
        .map(|_| ())
}

async fn build(state: &AppState, job: &JobCtx, a: &NewApp) -> Result<()> {
    let base = format!("/var/www/{}", a.domain);

    // 1) site (+ user + pool)
    job.log(&format!("== 1/6 Site {} ({}, PHP {})", a.domain, a.kind, a.php_version));
    let plan = sites::plan_create(&state.changes_env, &site_params(a)).map_err(|e| anyhow!(e))?;
    apply(state, job, plan).await?;
    let model = sites::load_model(&state.changes_env, &a.domain).map_err(|e| anyhow!(e))?;
    let run_as = model.user.clone().unwrap_or_else(|| "www-data".into());
    job.log(&format!("   runs as {run_as}"));

    // 2) database
    let mut env: BTreeMap<String, Option<String>> = BTreeMap::new();
    if a.database != "none" {
        let name = db_name(&a.domain);
        job.log(&format!("== 2/6 Database {name} ({})", a.database));
        let pw = crate::dbm::create_db_with_user(&a.database, &name, Some(&name))
            .await
            .map_err(|e| anyhow!("creating the database failed: {e}"))?
            .unwrap_or_default();
        let (conn, port) = if a.database == "mysql" { ("mysql", "3306") } else { ("pgsql", "5432") };
        for (k, v) in [
            ("DB_CONNECTION", conn),
            ("DB_HOST", "127.0.0.1"),
            ("DB_PORT", port),
            ("DB_DATABASE", &name),
            ("DB_USERNAME", &name),
            ("DB_PASSWORD", &pw),
        ] {
            env.insert(k.into(), Some(v.to_string()));
        }
        job.log(&format!("   user {name} with a generated password (saved in .env)"));
        if a.backup {
            let plan = crate::backups::Plan {
                id: uuid::Uuid::new_v4().to_string(),
                name: format!("{} nightly", a.domain),
                destination_id: None,
                databases: vec![format!("{}/{name}", a.database)],
                paths: Vec::new(),
                schedule: "daily 03:00".into(),
                keep_daily: 7,
                keep_weekly: 4,
                keep_monthly: 3,
                enabled: true,
                last_run_at: None,
                created_at: crate::util::fmt_ts(chrono::Utc::now()),
            };
            match crate::backups::upsert_plan(&state.db, &plan) {
                Ok(()) => job.log("   nightly database backup at 03:00 (Backups & Storage)"),
                Err(e) => job.log(&format!("   backup plan not created: {e:#}")),
            }
        }
    } else {
        job.log("== 2/6 Database: none");
        if a.kind == "laravel" {
            env.insert("DB_CONNECTION".into(), Some("sqlite".into()));
            env.insert(
                "DB_DATABASE".into(),
                Some(format!("{base}/{}database.sqlite", if a.releases { "shared/" } else { "" })),
            );
        }
    }

    // 3) project
    job.log(&format!("== 3/6 Git project {} @ {}", a.repo, a.branch));
    let secret: String = {
        use rand::distr::{Alphanumeric, SampleString};
        Alphanumeric.sample_string(&mut rand::rng(), 40)
    };
    let p = Project {
        id: a.domain.clone(),
        site_id: a.domain.clone(),
        kind: a.kind.clone(),
        dir: base.clone(),
        repo: a.repo.clone(),
        branch: a.branch.clone(),
        run_as: run_as.clone(),
        script: {
            let mut s = deploys::default_script(&a.kind, true);
            if !a.npm_build {
                s = s.replace("if [ -f package.json ] && grep -q '\"build\"' package.json && command -v npm >/dev/null; then\n  npm ci --no-audit --no-fund && npm run build\nfi\n", "");
            }
            if !a.composer {
                s = s.replace("if [ -f composer.json ] && command -v composer >/dev/null; then\n  composer install --no-dev --no-interaction --prefer-dist --optimize-autoloader\nfi\n", "");
            }
            s
        },
        migrate: true,
        auto_deploy: a.auto_deploy,
        webhook_secret: secret,
        created_at: crate::util::fmt_ts(chrono::Utc::now()),
        last_deploy_at: None,
        last_commit: None,
        strategy: if a.releases { "releases".into() } else { "in_place".into() },
        keep_releases: 5,
        run_tests: false,
        notify: true,
    };
    let needs_key = deploys::needs_key(&p.repo);
    if needs_key {
        deploys::ensure_key(state, &p.id).await?;
    }
    deploys::save_project(&state.db, &p)?;

    // 4) .env
    if a.kind == "laravel" {
        job.log("== 4/6 Production .env");
        let scheme = if a.ssl_email.is_empty() { "http" } else { "https" };
        let name = a.domain.split('.').next().unwrap_or("App");
        for (k, v) in [
            ("APP_NAME", format!("\"{}\"", name)),
            ("APP_ENV", "production".into()),
            ("APP_KEY", random_key()),
            ("APP_DEBUG", "false".into()),
            ("APP_URL", format!("{scheme}://{}", a.domain)),
            ("LOG_CHANNEL", "daily".into()),
            ("LOG_LEVEL", "warning".into()),
            ("SESSION_DRIVER", "file".into()),
            ("CACHE_STORE", "file".into()),
            ("QUEUE_CONNECTION", if a.database == "none" { "sync".into() } else { "database".into() }),
        ] {
            env.entry(k.into()).or_insert(Some(v.trim_matches('"').to_string()));
        }
    } else {
        job.log("== 4/6 .env");
    }
    for (k, v) in &a.env {
        env.insert(k.clone(), Some(v.clone()));
    }
    if !env.is_empty() {
        let plan = crate::plans::projects::plan_env(
            &state.changes_env,
            &p,
            &crate::plans::projects::EnvParams { id: p.id.clone(), set: env },
        )
        .map_err(|e| anyhow!(e))?;
        apply(state, job, plan).await?;
        if a.database == "none" && a.kind == "laravel" {
            let db = format!("{base}/{}database.sqlite", if a.releases { "shared/" } else { "" });
            let _ = crate::util::cmd::Cmd::new("runuser")
                .args(["-u", &run_as, "--", "touch", &db])
                .run()
                .await;
        }
    } else {
        job.log("   nothing to write");
    }

    // 5) HTTPS
    if a.ssl_email.is_empty() {
        job.log("== 5/6 HTTPS: skipped (add it later from Sites)");
    } else {
        job.log("== 5/6 HTTPS (Let's Encrypt)");
        match sites::plan_ssl(
            &state.changes_env,
            &sites::SslParams { id: a.domain.clone(), email: a.ssl_email.clone() },
        ) {
            Ok(plan) => {
                if let Err(e) = apply(state, job, plan).await {
                    job.log(&format!("   HTTPS failed: {e:#}. Check that {} points to this server, then enable it from Sites.", a.domain));
                }
            }
            Err(e) => job.log(&format!("   HTTPS skipped: {e}")),
        }
    }

    // 6) first deploy (worker + scheduler follow it)
    if a.kind == "laravel" && (a.worker || a.scheduler) {
        let _ = state.db.set_setting(
            &format!("pending_setup:{}", p.id),
            &serde_json::to_string(&PendingSetup { worker: a.worker, scheduler: a.scheduler })?,
        );
    }
    if needs_key {
        job.log("== 6/6 First deploy: waiting for you");
        job.log("   Add the deploy key shown on the project page to GitHub (Settings → Deploy keys), then press Deploy now.");
        return Ok(());
    }
    job.log("== 6/6 First deploy");
    match deploys::submit(state, &p, "manual", None, Some("new-app wizard".into())) {
        Ok(j) => job.log(&format!("   queued deploy job {j}")),
        Err(e) => bail!("could not queue the first deploy: {e:#}"),
    }
    Ok(())
}

/// Called after every successful deploy: finish the wizard's worker and
/// scheduler setup once the code exists.
pub async fn after_first_deploy(state: &AppState, job: &JobCtx, p: &Project) {
    let key = format!("pending_setup:{}", p.id);
    let Some(ps) = state
        .db
        .setting(&key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<PendingSetup>(&s).ok())
    else {
        return;
    };
    let _ = state.db.conn().execute("DELETE FROM settings WHERE key = ?1", [&key]);
    if ps.worker {
        job.log("Starting the queue worker (Supervisor)");
        let w = crate::plans::projects::WorkerParams {
            id: p.id.clone(),
            enabled: true,
            processes: 2,
            queue: "default".into(),
            workers: None,
        };
        match crate::plans::projects::plan_worker(&state.changes_env, p, &w) {
            Ok(plan) => {
                if let Err(e) = apply(state, job, plan).await {
                    job.log(&format!("  worker setup failed: {e:#}"));
                }
            }
            Err(e) => job.log(&format!("  worker setup skipped: {e}")),
        }
    }
    if ps.scheduler {
        job.log("Turning on the scheduler (cron, every minute)");
        match crate::plans::projects::plan_scheduler(&state.changes_env, p, true) {
            Ok(plan) => {
                if let Err(e) = apply(state, job, plan).await {
                    job.log(&format!("  scheduler setup failed: {e:#}"));
                }
            }
            Err(e) => job.log(&format!("  scheduler setup skipped: {e}")),
        }
    }
}

/// Defaults for the wizard form.
pub async fn defaults() -> ApiResult<Json<serde_json::Value>> {
    let php = crate::routes::php::info(&crate::routes::sites::scan(false).await).await;
    let dbs = crate::routes::databases::info().await;
    Ok(Json(json!({
        "php_versions": php.versions.iter().filter(|v| v.fpm_installed).map(|v| v.version.clone()).collect::<Vec<_>>(),
        "mysql": dbs.mysql.available,
        "postgres": dbs.postgres.available,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys() {
        assert_eq!(db_name("crm.example.com"), "crm_example_com");
        assert_eq!(db_name("1shop.io"), "db_1shop_io");
        assert!(db_name("a-very-long-subdomain-name.some-company.example.com").len() <= 32);
        let k = random_key();
        assert!(k.starts_with("base64:"));
        assert_eq!(k.len(), 7 + 44);
        assert!(k.ends_with('='));
    }
}
