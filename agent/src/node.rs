//! Node.js apps: a Git project that runs as its own Linux user under a
//! systemd service (`nexacloud-node-<slug>`), behind an nginx reverse proxy.
//! Every successful deploy restarts the service.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::changes;
use crate::deploys::{self, Project};
use crate::error::{ApiError, ApiResult};
use crate::jobs::{JobCtx, JobSpec};
use crate::plans::sites::{self, SiteKind};
use crate::state::AppState;
use crate::util::cmd::Cmd;

pub const SCRIPT: &str = "# Runs as the app user inside the project directory, after git fetch/reset.\nif [ -f package-lock.json ]; then npm ci --no-audit --no-fund; else npm install --no-audit --no-fund; fi\nnpm run build --if-present\n";

pub fn unit_name(id: &str) -> String {
    let slug: String = id.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("nexacloud-node-{slug}")
}

fn unit_path(id: &str) -> String {
    format!("/etc/systemd/system/{}.service", unit_name(id))
}

pub fn unit(p: &Project, port: u16, start: &str) -> String {
    let env_file = format!("{}/.env", p.shared_dir());
    format!(
        "# Managed by NexaCloud — Node.js app {id}\n[Unit]\nDescription=Node.js app {id}\nAfter=network.target\n\n[Service]\nType=simple\nUser={u}\nGroup={u}\nWorkingDirectory={dir}\nEnvironment=NODE_ENV=production\nEnvironment=PORT={port}\nEnvironmentFile=-{env_file}\nExecStart=/bin/sh -c {cmd}\nRestart=always\nRestartSec=3\nNoNewPrivileges=true\nProtectSystem=full\nPrivateTmp=true\nStandardOutput=journal\nStandardError=journal\n\n[Install]\nWantedBy=multi-user.target\n",
        id = p.id,
        u = p.run_as,
        dir = p.app_dir(),
        cmd = shell_quote(&format!("exec {start}")),
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Port written in the unit (None if not a Node app).
pub fn port_of(id: &str) -> Option<u16> {
    std::fs::read_to_string(unit_path(id)).ok()?.lines().find_map(|l| l.strip_prefix("Environment=PORT=").and_then(|p| p.trim().parse().ok()))
}

fn used_ports() -> Vec<u16> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/etc/systemd/system") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("nexacloud-node-") {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    v.extend(t.lines().filter_map(|l| l.strip_prefix("Environment=PORT=").and_then(|p| p.trim().parse::<u16>().ok())));
                }
            }
        }
    }
    v
}

fn free_port() -> u16 {
    let used = used_ports();
    (3000..4000).find(|p| !used.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).unwrap_or(3999)
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewNode {
    pub domain: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub repo: String,
    #[serde(default = "main")]
    pub branch: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default = "npm_start")]
    pub start: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub ssl_email: String,
    #[serde(default)]
    pub releases: bool,
    #[serde(default)]
    pub auto_deploy: bool,
}
fn main() -> String {
    "main".into()
}
fn npm_start() -> String {
    "npm start".into()
}

fn validate(state: &AppState, n: &NewNode) -> Result<(), String> {
    if !sites::valid_domain(&n.domain) {
        return Err("enter a domain like api.example.com".into());
    }
    if !deploys::valid_repo(&n.repo) {
        return Err("repository: git@github.com:owner/repo.git, https://… or /srv/git/…".into());
    }
    if !deploys::valid_branch(&n.branch) {
        return Err("invalid branch name".into());
    }
    if let Some(p) = n.port {
        if !(1024..=65000).contains(&p) || used_ports().contains(&p) {
            return Err("port: a free port between 1024 and 65000".into());
        }
    }
    let s = n.start.trim();
    if s.is_empty() || s.len() > 300 || s.contains(['\n', '\r']) {
        return Err("start command: one line, like npm start or node server.js".into());
    }
    for k in n.env.keys() {
        if !crate::plans::projects::valid_env_key(k) {
            return Err(format!("invalid .env name {k:?}"));
        }
    }
    if !n.ssl_email.is_empty() && !crate::monitor::notify::valid_email(&n.ssl_email) {
        return Err("the HTTPS email address is not valid".into());
    }
    if deploys::get_project(&state.db, &n.domain).ok().flatten().is_some() {
        return Err(format!("{} already exists", n.domain));
    }
    Ok(())
}

fn site_params(n: &NewNode, port: u16) -> sites::CreateParams {
    sites::CreateParams {
        kind: SiteKind::Proxy,
        domain: n.domain.clone(),
        aliases: n.aliases.clone(),
        root: None,
        php_version: None,
        upstream: Some(format!("http://127.0.0.1:{port}")),
        redirect_to: None,
        redirect_code: None,
        options: None,
        isolate: false,
        no_placeholder: true,
    }
}

/// `POST /api/apps/node` → `{job_id, project_id}`.
pub async fn create(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>, Json(n): Json<NewNode>) -> Response {
    let mut info = AuditInfo::action("app.node", n.domain.clone());
    let res: ApiResult<Response> = async {
        let mut n = n;
        n.domain = n.domain.trim().to_ascii_lowercase();
        n.repo = n.repo.trim().to_string();
        validate(&state, &n).map_err(ApiError::bad_request)?;
        let port = n.port.unwrap_or_else(free_port);
        sites::plan_create(&state.changes_env, &site_params(&n, port)).map_err(ApiError::conflict)?;
        let spec = JobSpec::new("app_node", &format!("Node.js app {}", n.domain))
            .lock(format!("site:{}", n.domain))
            .lock("apt")
            .timeout(Duration::from_secs(1800))
            .actor(Some(addr.ip().to_canonical().to_string()));
        let st = state.clone();
        let n2 = n.clone();
        let job = state.jobs.submit(spec, move |ctx| async move { build(&st, &ctx, &n2, port).await })?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job, "project_id": n.domain, "port": port})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

async fn apply(state: &AppState, job: &JobCtx, plan: changes::ChangePlan) -> Result<()> {
    changes::apply_now(&state.db, &state.changes_env, plan, "Node.js wizard", &job.log).await.map(|_| ())
}

async fn run(job: &JobCtx, c: Cmd, what: &str) -> Result<()> {
    let o = c.timeout(Duration::from_secs(900)).run().await;
    if !o.ok {
        for l in o.combined().lines().rev().take(15).collect::<Vec<_>>().into_iter().rev() {
            job.log(&format!("   {l}"));
        }
        bail!("{what} failed");
    }
    Ok(())
}

pub fn node_installed() -> bool {
    Path::new("/usr/bin/node").exists() && Path::new("/usr/bin/npm").exists()
}

async fn build(state: &AppState, job: &JobCtx, n: &NewNode, port: u16) -> Result<()> {
    let base = format!("/var/www/{}", n.domain);
    let user = sites::site_user(&n.domain);

    job.log("== 1/6 Node.js");
    if !node_installed() {
        job.log("   installing nodejs and npm (Ubuntu packages)…");
        run(job, Cmd::new("apt-get").args(["install", "-y", "-q", "nodejs", "npm"]).env("DEBIAN_FRONTEND", "noninteractive"), "installing Node.js").await?;
    }
    let v = Cmd::new("node").arg("--version").run().await;
    job.log(&format!("   node {}", v.stdout.trim()));

    job.log(&format!("== 2/6 User {user} and folder {base}"));
    run(
        job,
        Cmd::new("sh").args(["-c", "id -u \"$1\" >/dev/null 2>&1 || useradd --system --user-group --home-dir \"$2\" --no-create-home --shell /usr/sbin/nologin \"$1\"; install -d -m 0750 -o \"$1\" -g \"$1\" \"$2\"; usermod -aG \"$1\" www-data", "u", &user, &base]),
        "creating the app user",
    )
    .await?;

    job.log(&format!("== 3/6 Site {} → 127.0.0.1:{port}", n.domain));
    let plan = sites::plan_create(&state.changes_env, &site_params(n, port)).map_err(|e| anyhow!(e))?;
    apply(state, job, plan).await?;

    job.log(&format!("== 4/6 Project {} @ {}", n.repo, n.branch));
    let p = Project {
        id: n.domain.clone(),
        site_id: n.domain.clone(),
        kind: "node".into(),
        dir: base.clone(),
        repo: n.repo.clone(),
        branch: n.branch.clone(),
        run_as: user.clone(),
        script: SCRIPT.into(),
        migrate: false,
        auto_deploy: n.auto_deploy,
        webhook_secret: {
            use rand::distr::{Alphanumeric, SampleString};
            Alphanumeric.sample_string(&mut rand::rng(), 40)
        },
        created_at: crate::util::fmt_ts(chrono::Utc::now()),
        last_deploy_at: None,
        last_commit: None,
        strategy: if n.releases { "releases".into() } else { "in_place".into() },
        keep_releases: 5,
        run_tests: false,
        notify: true,
    };
    let needs_key = deploys::needs_key(&p.repo);
    if needs_key {
        deploys::ensure_key(state, &p.id).await?;
    }
    deploys::save_project(&state.db, &p)?;
    if !n.env.is_empty() {
        let set = n.env.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
        let plan = crate::plans::projects::plan_env(&state.changes_env, &p, &crate::plans::projects::EnvParams { id: p.id.clone(), set }).map_err(|e| anyhow!(e))?;
        apply(state, job, plan).await?;
    }

    job.log(&format!("== 5/6 Service {} (runs \"{}\" as {user}, PORT={port})", unit_name(&p.id), n.start.trim()));
    crate::util::atomic_write(Path::new(&unit_path(&p.id)), unit(&p, port, n.start.trim()).as_bytes(), 0o644, Some((0, 0)))?;
    let _ = Cmd::new("systemctl").arg("daemon-reload").run().await;
    let _ = Cmd::new("systemctl").args(["enable", &unit_name(&p.id)]).run().await;

    if !n.ssl_email.is_empty() {
        match sites::plan_ssl(&state.changes_env, &sites::SslParams { id: n.domain.clone(), email: n.ssl_email.clone() }) {
            Ok(plan) => {
                if let Err(e) = apply(state, job, plan).await {
                    job.log(&format!("   HTTPS failed: {e:#} — turn it on later from Domains & SSL."));
                }
            }
            Err(e) => job.log(&format!("   HTTPS skipped: {e}")),
        }
    }

    if needs_key {
        job.log("== 6/6 First deploy: waiting for you");
        job.log("   Add the deploy key shown on the project page to GitHub (Settings → Deploy keys), then press Deploy now.");
        return Ok(());
    }
    job.log("== 6/6 First deploy");
    match deploys::submit(state, &p, "manual", None, Some("Node.js wizard".into())) {
        Ok(j) => job.log(&format!("   queued deploy job {j} — the app starts when it finishes")),
        Err(e) => bail!("could not queue the first deploy: {e:#}"),
    }
    Ok(())
}

/// During a deploy of a Node project (before the health check, and again after a
/// rollback): (re)start its service and wait until it listens.
pub async fn after_deploy(job: &JobCtx, p: &Project) {
    if p.kind != "node" || !Path::new(&unit_path(&p.id)).exists() {
        return;
    }
    let u = unit_name(&p.id);
    job.log(&format!("Restarting {u}"));
    let o = Cmd::new("systemctl").args(["restart", &u]).timeout(Duration::from_secs(60)).run().await;
    if !o.ok {
        job.log(&format!("  restart failed: {}", o.error_summary("systemctl")));
        return;
    }
    // give it a moment and report whether it is listening
    if let Some(port) = port_of(&p.id) {
        for _ in 0..20 {
            if std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok() {
                job.log(&format!("  listening on 127.0.0.1:{port}"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        job.log(&format!("  not listening on port {port} yet — check the app logs (journalctl -u {u})"));
    }
}

/// `GET /api/projects/{id}/node` — service state.
pub async fn status(UrlPath(id): UrlPath<String>) -> ApiResult<Json<serde_json::Value>> {
    let u = unit_name(&id);
    let active = Cmd::new("systemctl").args(["is-active", &u]).run().await.stdout.trim().to_string();
    let logs = Cmd::new("journalctl").args(["-u", &u, "-n", "80", "--no-pager", "-o", "short-iso"]).timeout(Duration::from_secs(10)).run().await.stdout;
    Ok(Json(json!({"unit": u, "active": active, "port": port_of(&id), "logs": logs})))
}

/// `POST /api/projects/{id}/node/restart`
pub async fn restart(UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("node.restart", id.clone());
    let res = async {
        if port_of(&id).is_none() {
            return Err(ApiError::not_found("not a Node.js app"));
        }
        let o = Cmd::new("systemctl").args(["restart", &unit_name(&id)]).timeout(Duration::from_secs(60)).run().await;
        if !o.ok {
            return Err(ApiError::bad_request(o.error_summary("systemctl restart")));
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_quotes_start_command() {
        let p = Project {
            id: "api.test".into(),
            site_id: "api.test".into(),
            kind: "node".into(),
            dir: "/var/www/api.test".into(),
            repo: "/srv/git/x.git".into(),
            branch: "main".into(),
            run_as: "nc_api_test".into(),
            script: SCRIPT.into(),
            migrate: false,
            auto_deploy: false,
            webhook_secret: String::new(),
            created_at: String::new(),
            last_deploy_at: None,
            last_commit: None,
            strategy: "releases".into(),
            keep_releases: 5,
            run_tests: false,
            notify: true,
        };
        let u = unit(&p, 3001, "node server.js --name 'x'");
        assert!(u.contains("User=nc_api_test"));
        assert!(u.contains("WorkingDirectory=/var/www/api.test/current"));
        assert!(u.contains("Environment=PORT=3001"));
        assert!(u.contains("EnvironmentFile=-/var/www/api.test/shared/.env"));
        assert!(u.contains("ExecStart=/bin/sh -c 'exec node server.js --name '\\''x'\\'''"));
        assert_eq!(unit_name("api.test"), "nexacloud-node-api-test");
    }
}
