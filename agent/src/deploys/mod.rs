//! Git deploys: connect a site to a repository and deploy on push.
//!
//! - Each project gets a read-only **deploy key** (ed25519, root-only at
//!   `{data_dir}/deploy-keys/<id>`); you add the public half to the GitHub
//!   repo (Settings → Deploy keys). Local repos under `/srv/git/` need no key.
//! - Deploys run **as the site's user** (owner of the project directory, e.g.
//!   `www-data`) through `systemd-run --uid … -p MemoryMax=2G`, never as root.
//!   The key is copied to a private temp dir owned by that user for the run.
//! - In-place mode: `git fetch` + `git reset --hard origin/<branch>`, then the
//!   project's script (composer, npm build, migrate, optimize, queue:restart).
//!   A database dump is taken before `migrate`. After the script, PHP-FPM is
//!   reloaded and the site is probed; a 5xx rolls the code back to the
//!   previous commit automatically.
//! - Triggers: manual, GitHub webhook (`POST /hooks/github/<id>`, HMAC-SHA256),
//!   or polling (`git ls-remote` every 60 s) when auto-deploy is on.

pub mod releases;
pub mod routes;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::jobs::{JobCtx, JobSpec};
use crate::state::AppState;
use crate::util::cmd::Cmd;

const DEPLOY_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub site_id: String,
    /// laravel | php | static
    pub kind: String,
    pub dir: String,
    pub repo: String,
    pub branch: String,
    pub run_as: String,
    pub script: String,
    pub migrate: bool,
    pub auto_deploy: bool,
    #[serde(skip_serializing)]
    pub webhook_secret: String,
    pub created_at: String,
    pub last_deploy_at: Option<String>,
    pub last_commit: Option<String>,
    /// in_place | releases (zero-downtime: releases/<n> + current symlink)
    #[serde(default = "in_place")]
    pub strategy: String,
    #[serde(default = "five")]
    pub keep_releases: u32,
    /// Run `php artisan test` (Laravel) before going live.
    #[serde(default)]
    pub run_tests: bool,
    /// Send a notification when a deploy fails.
    #[serde(default = "yes")]
    pub notify: bool,
}

fn in_place() -> String {
    "in_place".into()
}
fn five() -> u32 {
    5
}
fn yes() -> bool {
    true
}

impl Project {
    /// The directory the live code is in: `dir/current` with release folders.
    pub fn app_dir(&self) -> String {
        if self.strategy == "releases" {
            format!("{}/current", self.dir)
        } else {
            self.dir.clone()
        }
    }
    /// Where .env and storage/ live (`dir/shared` with release folders).
    pub fn shared_dir(&self) -> String {
        if self.strategy == "releases" {
            format!("{}/shared", self.dir)
        } else {
            self.dir.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deploy {
    pub id: String,
    pub project_id: String,
    pub job_id: Option<String>,
    /// queued | running | ok | failed | rolled_back
    pub status: String,
    /// manual | webhook | poll | rollback
    pub trigger: String,
    pub commit: Option<String>,
    pub message: Option<String>,
    pub author: Option<String>,
    pub previous_commit: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Release folder name (release strategy).
    #[serde(default)]
    pub release: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Step {
    pub name: String,
    pub cmd: String,
    /// ok | failed | skipped | running
    pub status: String,
    pub secs: Option<f64>,
    #[serde(default)]
    pub result: String,
}

// ------------------------------------------------------------------ validation

pub fn valid_repo(r: &str) -> bool {
    let r = r.trim();
    let safe = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@~".contains(&b))
    };
    if r.len() > 200 || r.contains("..") || r.starts_with('-') {
        return false;
    }
    if let Some(rest) = r.strip_prefix("https://") {
        return rest.contains('/') && safe(rest);
    }
    if r.starts_with("/srv/git/") {
        return safe(r);
    }
    // scp-like: git@github.com:owner/repo.git
    if let Some((user_host, path)) = r.split_once(':') {
        return user_host.contains('@') && !path.is_empty() && !path.starts_with('/') && safe(r);
    }
    false
}

pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 100
        && !b.starts_with('-')
        && !b.contains("..")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-/".contains(&c))
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && u != "root"
        && u.len() <= 32
        && u.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

pub fn default_script(kind: &str, migrate: bool) -> String {
    let mut s = String::from(
        "# Runs as the site user inside the project directory, after git fetch/reset.\n",
    );
    s.push_str("if [ -f composer.json ] && command -v composer >/dev/null; then\n  composer install --no-dev --no-interaction --prefer-dist --optimize-autoloader\nfi\n");
    s.push_str("if [ -f package.json ] && grep -q '\"build\"' package.json && command -v npm >/dev/null; then\n  npm ci --no-audit --no-fund && npm run build\nfi\n");
    if kind == "laravel" {
        if migrate {
            s.push_str("php artisan migrate --force\n");
        }
        s.push_str("php artisan optimize\nphp artisan queue:restart\n");
    }
    s
}

// ------------------------------------------------------------------ store

fn now() -> String {
    crate::util::fmt_ts(Utc::now())
}

const P_COLS: &str = "id, site_id, kind, dir, repo, branch, run_as, script, migrate, auto_deploy, webhook_secret, created_at, last_deploy_at, last_commit, strategy, keep_releases, run_tests, notify";

fn p_row(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        site_id: r.get(1)?,
        kind: r.get(2)?,
        dir: r.get(3)?,
        repo: r.get(4)?,
        branch: r.get(5)?,
        run_as: r.get(6)?,
        script: r.get(7)?,
        migrate: r.get::<_, i64>(8)? != 0,
        auto_deploy: r.get::<_, i64>(9)? != 0,
        webhook_secret: r.get(10)?,
        created_at: r.get(11)?,
        last_deploy_at: r.get(12)?,
        last_commit: r.get(13)?,
        strategy: r.get(14)?,
        keep_releases: r.get::<_, i64>(15)?.clamp(1, 50) as u32,
        run_tests: r.get::<_, i64>(16)? != 0,
        notify: r.get::<_, i64>(17)? != 0,
    })
}

pub fn list_projects(db: &Db) -> Result<Vec<Project>> {
    let c = db.conn();
    let mut st = c.prepare(&format!("SELECT {P_COLS} FROM projects ORDER BY id"))?;
    let mut v = st
        .query_map([], p_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for p in v.iter_mut() {
        // The disk is the truth: a `current` symlink means release folders
        // (also after converting), an `nc_*` owner means an isolated site.
        if p.strategy != "releases" && Path::new(&format!("{}/current", p.dir)).is_symlink() {
            p.strategy = "releases".into();
        }
        if let Some(n) = owner_of(&p.dir).and_then(|(uid, _)| crate::util::user_name(uid)) {
            if n.starts_with("nc_") && n != p.run_as {
                p.run_as = n;
            }
        }
    }
    Ok(v)
}

pub fn get_project(db: &Db, id: &str) -> Result<Option<Project>> {
    Ok(list_projects(db)?.into_iter().find(|p| p.id == id))
}

pub fn save_project(db: &Db, p: &Project) -> Result<()> {
    db.conn().execute(
        &format!("INSERT INTO projects ({P_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
          ON CONFLICT(id) DO UPDATE SET repo=?5, branch=?6, run_as=?7, script=?8, migrate=?9, auto_deploy=?10,
            last_deploy_at=?13, last_commit=?14, strategy=?15, keep_releases=?16, run_tests=?17, notify=?18"),
        rusqlite::params![
            p.id, p.site_id, p.kind, p.dir, p.repo, p.branch, p.run_as, p.script, p.migrate as i64,
            p.auto_deploy as i64, p.webhook_secret, p.created_at, p.last_deploy_at, p.last_commit,
            p.strategy, p.keep_releases as i64, p.run_tests as i64, p.notify as i64
        ],
    )?;
    Ok(())
}

pub fn delete_project(db: &Db, id: &str) -> Result<bool> {
    Ok(db
        .conn()
        .execute("DELETE FROM projects WHERE id = ?1", [id])?
        > 0)
}

const D_COLS: &str = "id, project_id, job_id, status, trigger, commit_sha, message, author, previous_commit, started_at, finished_at, error, steps, release";

fn d_row(r: &rusqlite::Row) -> rusqlite::Result<Deploy> {
    Ok(Deploy {
        id: r.get(0)?,
        project_id: r.get(1)?,
        job_id: r.get(2)?,
        status: r.get(3)?,
        trigger: r.get(4)?,
        commit: r.get(5)?,
        message: r.get(6)?,
        author: r.get(7)?,
        previous_commit: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
        error: r.get(11)?,
        steps: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
        release: r.get(13)?,
    })
}

pub fn list_deploys(db: &Db, project: Option<&str>, limit: usize) -> Result<Vec<Deploy>> {
    let c = db.conn();
    let sql = format!(
        "SELECT {D_COLS} FROM deploys {} ORDER BY started_at DESC LIMIT {}",
        if project.is_some() {
            "WHERE project_id = ?1"
        } else {
            ""
        },
        limit.min(200)
    );
    let mut st = c.prepare(&sql)?;
    let v = match project {
        Some(p) => st
            .query_map([p], d_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
        None => st
            .query_map([], d_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    Ok(v)
}

fn save_deploy(db: &Db, d: &Deploy) -> Result<()> {
    db.conn().execute(
        &format!(
            "INSERT INTO deploys ({D_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
          ON CONFLICT(id) DO UPDATE SET job_id=?3, status=?4, commit_sha=?6, message=?7, author=?8,
            previous_commit=?9, finished_at=?11, error=?12, steps=?13, release=?14"
        ),
        rusqlite::params![
            d.id,
            d.project_id,
            d.job_id,
            d.status,
            d.trigger,
            d.commit,
            d.message,
            d.author,
            d.previous_commit,
            d.started_at,
            d.finished_at,
            d.error,
            serde_json::to_string(&d.steps).unwrap_or_else(|_| "[]".into()),
            d.release
        ],
    )?;
    Ok(())
}

pub fn sweep_interrupted(db: &Db) -> Result<usize> {
    Ok(db.conn().execute(
        "UPDATE deploys SET status = 'failed', error = 'interrupted by an agent restart', finished_at = ?1 WHERE status IN ('queued','running')",
        [now()],
    )?)
}

fn in_flight(db: &Db, project: &str) -> bool {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM deploys WHERE project_id = ?1 AND status IN ('queued','running')",
            [project],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// The PHP binary the site's nginx config uses (`php8.3`), or `php`.
pub fn php_bin(site_id: &str) -> String {
    let blocks = crate::nginx::merge_blocks(crate::routes::sites::read_blocks(
        Path::new("/etc/nginx/sites-enabled"),
        Path::new("/etc/nginx/conf.d"),
    ));
    blocks
        .iter()
        .find(|b| b.id() == site_id)
        .and_then(|b| {
            b.fastcgi_pass
                .as_deref()
                .and_then(crate::nginx::php_version_from_socket)
        })
        .or_else(|| {
            crate::plans::sites::load_model(
                &crate::changes::ChangeEnv::system(PathBuf::from("/tmp")),
                site_id,
            )
            .ok()
            .and_then(|m| m.php_version)
        })
        .filter(|v| Path::new(&format!("/usr/bin/php{v}")).exists())
        .map(|v| format!("php{v}"))
        .unwrap_or_else(|| "php".into())
}

/// A directory whose `php` is the site's PHP version, to prepend to PATH so
/// `php artisan …` and `composer` in deploy scripts use the right version.
fn php_path_dir(site_id: &str) -> Option<String> {
    let bin = php_bin(site_id);
    if bin == "php" {
        return None;
    }
    let dir = format!("/var/lib/nexacloud/phpbin/{bin}");
    let _ = std::fs::create_dir_all(&dir);
    let link = format!("{dir}/php");
    if !Path::new(&link).exists() {
        let _ = std::os::unix::fs::symlink(format!("/usr/bin/{bin}"), &link);
    }
    let _ = std::fs::set_permissions(
        "/var/lib/nexacloud/phpbin",
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    );
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    Some(dir)
}

// ------------------------------------------------------------------ keys

pub fn key_path(state: &AppState, id: &str) -> PathBuf {
    state.config.data_dir.join("deploy-keys").join(id)
}

fn known_hosts_path(state: &AppState) -> PathBuf {
    state
        .config
        .data_dir
        .join("deploy-keys")
        .join("known_hosts")
}

pub fn needs_key(repo: &str) -> bool {
    !repo.starts_with("/srv/git/") && !repo.starts_with("https://")
}

pub async fn ensure_key(state: &AppState, id: &str) -> Result<String> {
    let dir = state.config.data_dir.join("deploy-keys");
    crate::util::create_private_dir(&dir)?;
    let key = key_path(state, id);
    if !key.exists() {
        let o = Cmd::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                &format!("nexacloud-deploy-{id}"),
                "-f",
                &key.to_string_lossy(),
                "-q",
            ])
            .timeout(Duration::from_secs(30))
            .run()
            .await;
        if !o.ok {
            bail!("ssh-keygen failed: {}", o.error_summary("ssh-keygen"));
        }
    }
    Ok(std::fs::read_to_string(format!("{}.pub", key.display()))?
        .trim()
        .to_string())
}

pub fn public_key(state: &AppState, id: &str) -> Option<String> {
    std::fs::read_to_string(format!("{}.pub", key_path(state, id).display()))
        .ok()
        .map(|s| s.trim().to_string())
}

// ------------------------------------------------------------------ webhook / actions

pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex_decode(hex) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

pub fn actions_yaml(p: &Project) -> String {
    format!(
        r#"# .github/workflows/deploy.yml — run tests on GitHub, then tell NexaCloud to deploy.
# Repository secrets: NEXACLOUD_URL (e.g. https://panel.example.com) and
# NEXACLOUD_SECRET (the webhook secret shown in NexaCloud).
name: Deploy
on:
  push:
    branches: [{branch}]
jobs:
  test-and-deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
{tests}      - name: Trigger NexaCloud deploy
        env:
          URL: ${{{{ secrets.NEXACLOUD_URL }}}}
          SECRET: ${{{{ secrets.NEXACLOUD_SECRET }}}}
        run: |
          body='{{"ref":"refs/heads/{branch}","after":"'"$GITHUB_SHA"'"}}'
          sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$SECRET" | sed 's/^.* //')
          curl --fail -sS -X POST "$URL/hooks/github/{id}" \
            -H "Content-Type: application/json" -H "X-GitHub-Event: push" \
            -H "X-Hub-Signature-256: sha256=$sig" -d "$body"
"#,
        branch = p.branch,
        id = p.id,
        tests = if p.kind == "laravel" {
            "      - uses: shivammathur/setup-php@v2\n        with: { php-version: \"8.3\" }\n      - run: composer install --no-interaction --prefer-dist\n      - run: cp .env.example .env && php artisan key:generate && php artisan test\n"
        } else {
            ""
        }
    )
}

// ------------------------------------------------------------------ deploy

/// Queue a deploy. `commit`: deploy a specific commit (rollback) instead of the branch head.
pub fn submit(
    state: &AppState,
    project: &Project,
    trigger: &str,
    commit: Option<String>,
    actor: Option<String>,
) -> Result<String> {
    if in_flight(&state.db, &project.id) {
        bail!("a deploy of {} is already queued or running", project.id);
    }
    let mut d = Deploy {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: project.id.clone(),
        job_id: None,
        status: "queued".into(),
        trigger: trigger.into(),
        commit: commit.clone(),
        message: None,
        author: None,
        previous_commit: project.last_commit.clone(),
        started_at: now(),
        finished_at: None,
        error: None,
        steps: Vec::new(),
        release: None,
    };
    save_deploy(&state.db, &d)?;
    let spec = JobSpec::new("deploy", &format!("Deploy {} ({})", project.id, trigger))
        .heavy()
        .lock(format!("site:{}", project.site_id))
        .timeout(DEPLOY_TIMEOUT)
        .actor(actor);
    let st = state.clone();
    let p = project.clone();
    let mut d2 = d.clone();
    let job = state.jobs.submit(spec, move |ctx| async move {
        d2.status = "running".into();
        d2.job_id = Some(ctx.id.clone());
        let _ = save_deploy(&st.db, &d2);
        let res = run(&st, &ctx, &p, &mut d2, commit.as_deref()).await;
        d2.finished_at = Some(now());
        match &res {
            Ok(rolled_back) => {
                d2.status = if *rolled_back { "rolled_back".into() } else { "ok".into() };
            }
            Err(e) => {
                d2.status = "failed".into();
                d2.error = Some(format!("{e:#}"));
            }
        }
        let _ = save_deploy(&st.db, &d2);
        crate::monitor::project::invalidate(&p.id);
        if d2.status != "ok" && p.notify {
            crate::monitor::notify::send(
                &st,
                crate::monitor::notify::Event::DeployFailed,
                &format!("deploy_failed:{}", p.id),
                "critical",
                &format!(
                    "Deploy of {} {}",
                    p.id,
                    if d2.status == "rolled_back" { "was rolled back" } else { "failed" }
                ),
                &format!(
                    "{}{}",
                    d2.commit.as_deref().map(|c| format!("Commit {} — ", &c[..c.len().min(8)])).unwrap_or_default(),
                    d2.error.clone().unwrap_or_default()
                ),
                None,
            );
        }
        if d2.status == "rolled_back" {
            return Err(anyhow!("the site failed its health check after deploying, so the previous commit was restored"));
        }
        res.map(|_| ())
    })?;
    d.job_id = Some(job.clone());
    save_deploy(&state.db, &d)?;
    Ok(job)
}

/// Owner uid/gid of the project dir.
fn owner_of(dir: &str) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).ok().map(|m| (m.uid(), m.gid()))
}

struct TempKey(Option<PathBuf>);
impl Drop for TempKey {
    fn drop(&mut self) {
        if let Some(d) = self.0.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Run a shell snippet as the project user in `cwd`, streaming to the job log.
pub async fn as_user_in(
    p: &Project,
    job: &JobCtx,
    env: &[(String, String)],
    script: &str,
    timeout: Duration,
    cwd: &str,
) -> Result<()> {
    let mut c = Cmd::new("systemd-run").args([
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        &format!("--uid={}", p.run_as),
        &format!("--gid={}", p.run_as),
        "-p",
        "MemoryMax=2G",
        &format!("--working-directory={cwd}"),
        "-E",
        &format!("HOME={}", p.dir),
        "-E",
        &format!("COMPOSER_HOME=/tmp/nexacloud-composer-{}", p.run_as),
        "-E",
        &format!(
            "PATH={}/usr/local/bin:/usr/bin:/bin",
            php_path_dir(&p.site_id)
                .map(|d| format!("{d}:"))
                .unwrap_or_default()
        ),
    ]);
    for (k, v) in env {
        c = c.args(["-E", &format!("{k}={v}")]);
    }
    let o = c
        .args(["--", "bash", "-eo", "pipefail", "-c", script])
        .timeout(timeout)
        .run_streaming(job.log.as_ref())
        .await;
    if !o.ok {
        bail!(
            "{}",
            if o.timed_out {
                "timed out".to_string()
            } else {
                format!("exit code {:?}", o.exit_code)
            }
        );
    }
    Ok(())
}

/// [`as_user_in`] in the live app directory.
pub async fn as_user(
    p: &Project,
    job: &JobCtx,
    env: &[(String, String)],
    script: &str,
    timeout: Duration,
) -> Result<()> {
    as_user_in(p, job, env, script, timeout, &p.app_dir()).await
}

async fn head_info_at(
    dir: &str,
    env: &[(String, String)],
) -> Option<(String, String, String)> {
    let mut c = crate::util::git_as_owner(Path::new(dir)).args([
        "log",
        "-1",
        "--format=%H%x1f%s%x1f%an",
    ]);
    for (k, v) in env {
        c = c.env(k, v);
    }
    let o = c.timeout(Duration::from_secs(20)).run().await;
    if !o.ok {
        return None;
    }
    let mut it = o.stdout.trim().split('\u{1f}');
    Some((
        it.next()?.to_string(),
        it.next().unwrap_or("").to_string(),
        it.next().unwrap_or("").to_string(),
    ))
}

async fn probe(site_id: &str) -> Option<u16> {
    let sites = crate::routes::sites::scan(false).await;
    let s = sites.into_iter().find(|s| s.id == site_id)?;
    let (scheme, port) = if s.ssl {
        ("https", "443")
    } else {
        ("http", "80")
    };
    for _ in 0..6 {
        let o = Cmd::new("curl")
            .args([
                "-ks",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                "15",
                "--resolve",
                &format!("{site_id}:{port}:127.0.0.1"),
                &format!("{scheme}://{site_id}/"),
            ])
            .timeout(Duration::from_secs(20))
            .run()
            .await;
        let code: u16 = o.stdout.trim().parse().unwrap_or(0);
        if code > 0 && code < 500 {
            return Some(code);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    None
}

async fn reload_fpm(p: &Project, job: &JobCtx) {
    let sites = crate::routes::sites::scan(false).await;
    if let Some(v) = sites
        .iter()
        .find(|s| s.id == p.site_id)
        .and_then(|s| s.php_version.clone())
    {
        let o = Cmd::new("systemctl")
            .args(["reload", &format!("php{v}-fpm")])
            .timeout(Duration::from_secs(60))
            .run()
            .await;
        job.log(&format!(
            "Reloaded php{v}-fpm (clears OPcache): {}",
            if o.ok { "ok" } else { "failed" }
        ));
    }
    // FrankenPHP keeps code in memory too.
    if let Ok(m) = crate::plans::sites::load_model(
        &crate::changes::ChangeEnv::system(PathBuf::from("/tmp")),
        &p.site_id,
    ) {
        if m.runtime == crate::plans::sites::Runtime::Frankenphp {
            let unit = crate::plans::runtime::unit_name(&p.site_id);
            let o = Cmd::new("systemctl")
                .args(["restart", &unit])
                .timeout(Duration::from_secs(60))
                .run()
                .await;
            job.log(&format!(
                "Restarted {unit}: {}",
                if o.ok { "ok" } else { "failed" }
            ));
        }
    }
}

fn env_value(p: &Project, key: &str) -> Option<String> {
    let env = std::fs::read_to_string(Path::new(&p.shared_dir()).join(".env")).unwrap_or_default();
    env.lines().find_map(|l| {
        let (a, b) = l.split_once('=')?;
        (a.trim() == key).then(|| b.trim().trim_matches('"').to_string())
    })
}

async fn pre_migrate_backup(state: &AppState, p: &Project, job: &JobCtx) -> Result<String> {
    let (Some(conn), Some(name)) = (env_value(p, "DB_CONNECTION"), env_value(p, "DB_DATABASE"))
    else {
        return Ok("no database in .env".into());
    };
    let engine = match conn.as_str() {
        "mysql" | "mariadb" => crate::routes::databases::Engine::Mysql,
        "pgsql" => crate::routes::databases::Engine::Postgres,
        _ => return Ok(format!("{conn}: not backed up")),
    };
    if !crate::util::is_valid_db_name(&name) {
        return Ok("database name not supported".into());
    }
    let dir = state.config.backup_dir.join("deploys").join(&p.id);
    for d in [
        &state.config.backup_dir,
        &state.config.backup_dir.join("deploys"),
        &dir,
    ] {
        let _ = crate::util::create_private_dir(d);
    }
    job.log(&format!("Backing up database {name} before migrating"));
    let f = crate::routes::databases::dump_into(job, engine, &name, &dir).await?;
    job.log(&format!("  saved {}", f.display()));
    prune_deploy_dumps(&dir, 7);
    Ok(format!("{name} saved"))
}

/// Keep the newest `keep` pre-migrate dumps of a project.
fn prune_deploy_dumps(dir: &Path, keep: usize) {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    let n = v.len().saturating_sub(keep);
    for (_, f) in v.into_iter().take(n) {
        let _ = std::fs::remove_file(f);
    }
}

// ------------------------------------------------------------------ steps

fn step_begin(state: &AppState, d: &mut Deploy, name: &str, cmd: &str) -> (usize, std::time::Instant) {
    d.steps.push(Step {
        name: name.into(),
        cmd: cmd.into(),
        status: "running".into(),
        secs: None,
        result: String::new(),
    });
    let _ = save_deploy(&state.db, d);
    (d.steps.len() - 1, std::time::Instant::now())
}

fn step_end(
    state: &AppState,
    d: &mut Deploy,
    (i, t0): (usize, std::time::Instant),
    ok: bool,
    result: &str,
) {
    if let Some(s) = d.steps.get_mut(i) {
        s.status = if ok { "ok" } else { "failed" }.into();
        s.secs = Some((t0.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        s.result = result.into();
    }
    let _ = save_deploy(&state.db, d);
}

fn step_skip(state: &AppState, d: &mut Deploy, name: &str, cmd: &str, why: &str) {
    d.steps.push(Step {
        name: name.into(),
        cmd: cmd.into(),
        status: "skipped".into(),
        secs: None,
        result: why.into(),
    });
    let _ = save_deploy(&state.db, d);
}

/// First non-comment lines of the deploy script, for the step list.
fn script_summary(s: &str) -> String {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("if ") && *l != "fi")
        .collect();
    let mut out = lines.iter().take(3).cloned().collect::<Vec<_>>().join(" · ");
    if lines.len() > 3 {
        out.push_str(&format!(" · +{}", lines.len() - 3));
    }
    crate::util::cmd::truncate(&out, 160)
}

const TEST_SCRIPT: &str = "composer install --no-interaction --prefer-dist --quiet\nphp artisan test\n";

/// Deploy key → private temp copy readable by the site user.
async fn deploy_env(state: &AppState, p: &Project, d: &Deploy) -> Result<(Vec<(String, String)>, TempKey)> {
    let mut env: Vec<(String, String)> = vec![("GIT_TERMINAL_PROMPT".into(), "0".into())];
    let mut guard = TempKey(None);
    if needs_key(&p.repo) {
        let key = key_path(state, &p.id);
        if !key.exists() {
            bail!("deploy key missing — open the project and add its deploy key to GitHub");
        }
        let tmp = PathBuf::from(format!("/run/nexacloud-deploy-{}", d.id));
        std::fs::create_dir_all(&tmp)?;
        guard = TempKey(Some(tmp.clone()));
        let kh = known_hosts_path(state);
        let _ = std::fs::copy(&kh, tmp.join("known_hosts"));
        std::fs::copy(&key, tmp.join("key"))?;
        for f in [tmp.clone(), tmp.join("key"), tmp.join("known_hosts")] {
            let _ = Cmd::new("chown")
                .arg(format!("{}:{}", p.run_as, p.run_as))
                .arg(f.to_string_lossy())
                .run()
                .await;
        }
        let _ = std::fs::set_permissions(
            tmp.join("key"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        );
        env.push((
            "GIT_SSH_COMMAND".into(),
            format!(
                "ssh -i {k} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={h}",
                k = tmp.join("key").display(),
                h = tmp.join("known_hosts").display()
            ),
        ));
    }
    Ok((env, guard))
}

const GIT_SYNC: &str = "[ -d .git ] || git init -q -b \"$NC_BRANCH\"\ngit remote get-url origin >/dev/null 2>&1 || git remote add origin \"$NC_REPO\"\n[ \"$(git remote get-url origin)\" = \"$NC_REPO\" ] || git remote set-url origin \"$NC_REPO\"\ngit fetch --prune origin \"$NC_BRANCH\"\ngit checkout -q -f -B \"$NC_BRANCH\" \"$NC_TARGET\"\ngit reset -q --hard \"$NC_TARGET\"\n";

fn git_env(p: &Project, env: &[(String, String)], target: &str) -> Vec<(String, String)> {
    let mut g = env.to_vec();
    g.push(("NC_REPO".into(), p.repo.clone()));
    g.push(("NC_BRANCH".into(), p.branch.clone()));
    g.push(("NC_TARGET".into(), target.to_string()));
    g
}

/// Returns Ok(true) when the deploy was rolled back after a failed probe.
async fn run(
    state: &AppState,
    job: &JobCtx,
    p: &Project,
    d: &mut Deploy,
    commit: Option<&str>,
) -> Result<bool> {
    if owner_of(&p.dir).is_none() {
        bail!("project directory {} is missing", p.dir);
    }
    if commit.is_some_and(|c| !c.bytes().all(|b| b.is_ascii_hexdigit()) || c.len() < 7) {
        bail!("invalid commit");
    }
    let (env, guard) = deploy_env(state, p, d).await?;
    let res = if p.strategy == "releases" {
        releases::run(state, job, p, d, commit, &env).await
    } else {
        run_in_place(state, job, p, d, commit, &env).await
    };
    if let Some(g) = &guard.0 {
        let _ = std::fs::copy(g.join("known_hosts"), known_hosts_path(state));
    }
    if res.as_ref().is_ok_and(|rb| !rb) {
        after_success(state, job, p, d).await;
    }
    res
}

/// Bookkeeping after a deploy went live.
async fn after_success(state: &AppState, job: &JobCtx, p: &Project, d: &Deploy) {
    // Targeted update: settings edited during the deploy are kept.
    let _ = state.db.conn().execute(
        "UPDATE projects SET last_commit = ?2, last_deploy_at = ?3 WHERE id = ?1",
        rusqlite::params![p.id, d.commit, now()],
    );
    if d.trigger == "rollback" && p.auto_deploy {
        // Otherwise the poller would redeploy the newer (bad) commit within a minute.
        let _ = state
            .db
            .conn()
            .execute("UPDATE projects SET auto_deploy = 0 WHERE id = ?1", [&p.id]);
        job.log("Auto-deploy paused because you rolled back. Turn it on again after pushing a fix.");
    }
    crate::apps::after_first_deploy(state, job, p).await;
}

async fn run_in_place(
    state: &AppState,
    job: &JobCtx,
    p: &Project,
    d: &mut Deploy,
    commit: Option<&str>,
    env: &[(String, String)],
) -> Result<bool> {
    let before = head_info_at(&p.dir, &[]).await.map(|h| h.0);
    d.previous_commit = before.clone();
    let target = commit
        .map(String::from)
        .unwrap_or_else(|| format!("origin/{}", p.branch));

    // 1) fetch + reset (as the site user)
    let s = step_begin(state, d, "Fetch code", &format!("git fetch && reset --hard {}", short(&target)));
    job.log(&format!("Fetching {} ({})", p.repo, p.branch));
    if let Err(e) = as_user_in(p, job, &git_env(p, env, &target), GIT_SYNC, Duration::from_secs(600), &p.dir).await {
        step_end(state, d, s, false, "Failed");
        return Err(e.context("git fetch/reset failed"));
    }
    if let Some((sha, msg, author)) = head_info_at(&p.dir, env).await {
        job.log(&format!("Now at {} — {msg} ({author})", short(&sha)));
        d.commit = Some(sha);
        d.message = Some(msg);
        d.author = Some(author);
    }
    let res = d.commit.as_deref().map(short).unwrap_or("Done").to_string();
    step_end(state, d, s, true, &res);

    // 2) tests, backup, script
    let mut script_res: Result<()> = Ok(());
    if p.run_tests && p.kind == "laravel" {
        let s = step_begin(state, d, "Run tests", "composer install && php artisan test");
        let r = as_user_in(p, job, env, TEST_SCRIPT, DEPLOY_TIMEOUT, &p.dir).await;
        step_end(state, d, s, r.is_ok(), if r.is_ok() { "Passed" } else { "Failed" });
        script_res = r.map_err(|e| anyhow!("tests failed ({e:#})"));
    }
    if script_res.is_ok() {
        if p.migrate && p.kind == "laravel" && p.script.contains("migrate") {
            let s = step_begin(state, d, "Back up database", "dump before migrate");
            match pre_migrate_backup(state, p, job).await {
                Ok(r) => step_end(state, d, s, true, &r),
                Err(e) => {
                    job.log(&format!("  backup failed: {e:#}"));
                    step_end(state, d, s, false, "Skipped (failed)");
                }
            }
        }
        job.log("Running deploy script");
        let s = step_begin(state, d, "Run deploy script", &script_summary(&p.script));
        script_res = as_user_in(p, job, env, &p.script, DEPLOY_TIMEOUT, &p.dir).await;
        step_end(state, d, s, script_res.is_ok(), if script_res.is_ok() { "Done" } else { "Failed" });
    }
    let s = step_begin(state, d, "Reload & health check", &format!("php-fpm reload · GET {}/", p.site_id));
    reload_fpm(p, job).await;

    // 3) health check → automatic code rollback on failure
    let healthy = if script_res.is_ok() { probe(&p.site_id).await } else { None };
    match (&script_res, healthy) {
        (Ok(()), Some(code)) => {
            step_end(state, d, s, true, &format!("HTTP {code}"));
            job.log(&format!("Health check: HTTP {code} — deploy is live"));
            Ok(false)
        }
        _ => {
            step_end(state, d, s, false, if script_res.is_ok() { "5xx" } else { "Skipped" });
            let why = match &script_res {
                Err(e) => format!("{e:#}"),
                Ok(()) => "the site returns errors (5xx) after deploying".into(),
            };
            let why = if why.starts_with("tests failed") { why } else if script_res.is_err() { format!("deploy script failed ({why})") } else { why };
            let Some(prev) = before.filter(|b| Some(b) != d.commit.as_ref()) else {
                bail!("{why}; nothing to roll back to");
            };
            job.log(&format!("{why} — rolling back to {}", short(&prev)));
            let mut renv = env.to_vec();
            renv.push(("NC_PREV".into(), prev.clone()));
            as_user_in(p, job, &renv, "git reset --hard \"$NC_PREV\"", Duration::from_secs(120), &p.dir).await?;
            let _ = as_user_in(
                p,
                job,
                env,
                &p.script.replace("php artisan migrate --force\n", ""),
                DEPLOY_TIMEOUT,
                &p.dir,
            )
            .await;
            reload_fpm(p, job).await;
            d.error = Some(why);
            Ok(true)
        }
    }
}

pub fn short(s: &str) -> &str {
    if s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        &s[..8]
    } else {
        s
    }
}

// ------------------------------------------------------------------ poller

pub fn spawn_poller(state: AppState) {
    tokio::spawn(async move {
        let _ = sweep_interrupted(&state.db);
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let Ok(projects) = list_projects(&state.db) else {
                continue;
            };
            for p in projects.into_iter().filter(|p| p.auto_deploy) {
                if in_flight(&state.db, &p.id) {
                    continue;
                }
                if let Some(remote) = remote_head(&state, &p).await {
                    // Don't retry a commit whose last deploy failed / was rolled back.
                    let last_attempt = list_deploys(&state.db, Some(&p.id), 1)
                        .ok()
                        .and_then(|v| v.into_iter().next());
                    let failed_before = last_attempt.is_some_and(|d| {
                        d.commit.as_deref() == Some(remote.as_str()) && d.status != "ok"
                    });
                    if p.last_commit.as_deref() != Some(remote.as_str()) && !failed_before {
                        match submit(&state, &p, "poll", None, Some("poller".into())) {
                            Ok(j) => tracing::info!("new commit on {} → deploy job {j}", p.id),
                            Err(e) => tracing::warn!("poll deploy of {}: {e:#}", p.id),
                        }
                    }
                }
            }
        }
    });
}

async fn remote_head(state: &AppState, p: &Project) -> Option<String> {
    // Local repos are owned by the site user; git refuses to read them as
    // root ("dubious ownership"), so ask as that user.
    let refname = format!("refs/heads/{}", p.branch);
    let mut c = if p.repo.starts_with("/srv/git/") {
        Cmd::new("runuser")
            .args(["-u", &p.run_as, "--", "git", "ls-remote", &p.repo, &refname])
            .current_dir("/tmp")
    } else {
        Cmd::new("git").args(["ls-remote", &p.repo, &refname])
    }
    .env("GIT_TERMINAL_PROMPT", "0");
    if needs_key(&p.repo) {
        c = c.env(
            "GIT_SSH_COMMAND",
            &format!(
                "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={} -o BatchMode=yes",
                key_path(state, &p.id).display(),
                known_hosts_path(state).display()
            ),
        );
    }
    let o = c.timeout(Duration::from_secs(30)).run().await;
    o.ok.then(|| o.stdout.split_whitespace().next().map(String::from))
        .flatten()
}

pub fn valid_run_as(u: &str) -> bool {
    valid_user(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_validation() {
        assert!(valid_repo("git@github.com:nexadev/shop.git"));
        assert!(valid_repo("https://github.com/nexadev/shop.git"));
        assert!(valid_repo("/srv/git/shop.git"));
        assert!(!valid_repo("git@github.com:/etc/passwd"));
        assert!(!valid_repo("--upload-pack=touch /tmp/x"));
        assert!(!valid_repo("https://x/$(id)"));
        assert!(!valid_repo("/etc/shadow"));
        assert!(valid_branch("main"));
        assert!(valid_branch("release/2026-11"));
        assert!(!valid_branch("-x"));
        assert!(!valid_branch("a..b"));
        assert!(valid_user("www-data"));
        assert!(!valid_user("root"));
    }

    #[test]
    fn hmac_signature() {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(b"{\"ref\":\"refs/heads/main\"}");
        let sig: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(verify_signature(
            "s3cret",
            b"{\"ref\":\"refs/heads/main\"}",
            &format!("sha256={sig}")
        ));
        assert!(!verify_signature(
            "other",
            b"{\"ref\":\"refs/heads/main\"}",
            &format!("sha256={sig}")
        ));
        assert!(!verify_signature(
            "s3cret",
            b"tampered",
            &format!("sha256={sig}")
        ));
        assert!(!verify_signature("s3cret", b"x", "sha1=abc"));
    }

    #[test]
    fn scripts_and_yaml() {
        let s = default_script("laravel", true);
        assert!(s.contains("php artisan migrate --force"));
        assert!(!default_script("php", true).contains("artisan"));
        let p = Project {
            id: "shop.example.com".into(),
            site_id: "shop.example.com".into(),
            kind: "laravel".into(),
            dir: "/var/www/shop".into(),
            repo: "git@github.com:a/b.git".into(),
            branch: "main".into(),
            run_as: "www-data".into(),
            script: s,
            migrate: true,
            auto_deploy: false,
            webhook_secret: "x".into(),
            created_at: "".into(),
            last_deploy_at: None,
            last_commit: None,
            strategy: "in_place".into(),
            keep_releases: 5,
            run_tests: false,
            notify: true,
        };
        let y = actions_yaml(&p);
        assert!(y.contains("branches: [main]"));
        assert!(y.contains("/hooks/github/shop.example.com"));
        assert!(y.contains("${{ secrets.NEXACLOUD_SECRET }}"));
    }
}
