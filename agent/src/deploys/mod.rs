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

pub mod routes;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
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

const P_COLS: &str = "id, site_id, kind, dir, repo, branch, run_as, script, migrate, auto_deploy, webhook_secret, created_at, last_deploy_at, last_commit";

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
    })
}

pub fn list_projects(db: &Db) -> Result<Vec<Project>> {
    let c = db.conn();
    let mut st = c.prepare(&format!("SELECT {P_COLS} FROM projects ORDER BY id"))?;
    let v = st
        .query_map([], p_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn get_project(db: &Db, id: &str) -> Result<Option<Project>> {
    Ok(list_projects(db)?.into_iter().find(|p| p.id == id))
}

pub fn save_project(db: &Db, p: &Project) -> Result<()> {
    db.conn().execute(
        &format!("INSERT INTO projects ({P_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
          ON CONFLICT(id) DO UPDATE SET repo=?5, branch=?6, run_as=?7, script=?8, migrate=?9, auto_deploy=?10,
            last_deploy_at=?13, last_commit=?14"),
        rusqlite::params![
            p.id, p.site_id, p.kind, p.dir, p.repo, p.branch, p.run_as, p.script, p.migrate as i64,
            p.auto_deploy as i64, p.webhook_secret, p.created_at, p.last_deploy_at, p.last_commit
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

const D_COLS: &str = "id, project_id, job_id, status, trigger, commit_sha, message, author, previous_commit, started_at, finished_at, error";

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
            "INSERT INTO deploys ({D_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
          ON CONFLICT(id) DO UPDATE SET job_id=?3, status=?4, commit_sha=?6, message=?7, author=?8,
            previous_commit=?9, finished_at=?11, error=?12"
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
            d.error
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

/// Run a shell snippet as the project user in its dir, streaming to the job log.
async fn as_user(
    p: &Project,
    job: &JobCtx,
    env: &[(String, String)],
    script: &str,
    timeout: Duration,
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
        &format!("--working-directory={}", p.dir),
        "-E",
        &format!("HOME={}", p.dir),
        "-E",
        "COMPOSER_HOME=/tmp/nexacloud-composer",
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

async fn head_info(p: &Project, env: &[(String, String)]) -> Option<(String, String, String)> {
    let mut c = crate::util::git_as_owner(Path::new(&p.dir)).args([
        "log",
        "-1",
        "--format=%H%x1f%s%x1f%an",
    ]);
    for (k, v) in env {
        c = c.env(k, v);
    }
    let o = c.timeout(Duration::from_secs(20)).run().await;
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
}

async fn pre_migrate_backup(state: &AppState, p: &Project, job: &JobCtx) {
    let env = std::fs::read_to_string(Path::new(&p.dir).join(".env")).unwrap_or_default();
    let get = |k: &str| {
        env.lines().find_map(|l| {
            let (a, b) = l.split_once('=')?;
            (a.trim() == k).then(|| b.trim().trim_matches('"').to_string())
        })
    };
    let (Some(conn), Some(name)) = (get("DB_CONNECTION"), get("DB_DATABASE")) else {
        return;
    };
    let engine = match conn.as_str() {
        "mysql" | "mariadb" => crate::routes::databases::Engine::Mysql,
        "pgsql" => crate::routes::databases::Engine::Postgres,
        _ => return,
    };
    if !crate::util::is_valid_db_name(&name) {
        return;
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
    match crate::routes::databases::dump_into(job, engine, &name, &dir).await {
        Ok(f) => job.log(&format!("  saved {}", f.display())),
        Err(e) => job.log(&format!("  backup skipped: {e:#}")),
    }
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
    // Deploy key → private temp copy readable by the site user.
    let mut env: Vec<(String, String)> = vec![("GIT_TERMINAL_PROMPT".into(), "0".into())];
    let mut _guard = TempKey(None);
    if needs_key(&p.repo) {
        let key = key_path(state, &p.id);
        if !key.exists() {
            bail!("deploy key missing — open the project and add its deploy key to GitHub");
        }
        let tmp = PathBuf::from(format!("/run/nexacloud-deploy-{}", d.id));
        std::fs::create_dir_all(&tmp)?;
        _guard = TempKey(Some(tmp.clone()));
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
    let before = head_info(p, &[]).await.map(|h| h.0);
    d.previous_commit = before.clone();

    // 1) fetch + reset (as the site user)
    let target = commit
        .map(String::from)
        .unwrap_or_else(|| format!("origin/{}", p.branch));
    if commit.is_some_and(|c| !c.bytes().all(|b| b.is_ascii_hexdigit()) || c.len() < 7) {
        bail!("invalid commit");
    }
    job.log(&format!("Fetching {} ({})", p.repo, p.branch));
    let git = "[ -d .git ] || git init -q -b \"$NC_BRANCH\"\ngit remote get-url origin >/dev/null 2>&1 || git remote add origin \"$NC_REPO\"\n[ \"$(git remote get-url origin)\" = \"$NC_REPO\" ] || git remote set-url origin \"$NC_REPO\"\ngit fetch --prune origin \"$NC_BRANCH\"\ngit checkout -q -f -B \"$NC_BRANCH\" \"$NC_TARGET\"\ngit reset -q --hard \"$NC_TARGET\"\n";
    let mut genv = env.clone();
    genv.push(("NC_REPO".into(), p.repo.clone()));
    genv.push(("NC_BRANCH".into(), p.branch.clone()));
    genv.push(("NC_TARGET".into(), target.clone()));
    as_user(p, job, &genv, git, Duration::from_secs(600))
        .await
        .context("git fetch/reset failed")?;
    if let Some(g) = &_guard.0 {
        let _ = std::fs::copy(g.join("known_hosts"), known_hosts_path(state));
    }
    if let Some((sha, msg, author)) = head_info(p, &env).await {
        job.log(&format!(
            "Now at {} — {msg} ({author})",
            &sha[..sha.len().min(10)]
        ));
        d.commit = Some(sha);
        d.message = Some(msg);
        d.author = Some(author);
    }
    let _ = save_deploy(&state.db, d);

    // 2) pre-migrate backup + script
    if p.migrate && p.kind == "laravel" {
        pre_migrate_backup(state, p, job).await;
    }
    job.log("Running deploy script");
    let script_res = as_user(p, job, &env, &p.script, DEPLOY_TIMEOUT).await;
    reload_fpm(p, job).await;

    // 3) health check → automatic code rollback on failure
    let healthy = probe(&p.site_id).await;
    match (&script_res, healthy) {
        (Ok(()), Some(code)) => {
            job.log(&format!("Health check: HTTP {code} — deploy is live"));
            // Targeted update: settings edited during the deploy are kept.
            state.db.conn().execute(
                "UPDATE projects SET last_commit = ?2, last_deploy_at = ?3 WHERE id = ?1",
                rusqlite::params![p.id, d.commit, now()],
            )?;
            if d.trigger == "rollback" && p.auto_deploy {
                // Otherwise the poller would redeploy the newer (bad) commit within a minute.
                state
                    .db
                    .conn()
                    .execute("UPDATE projects SET auto_deploy = 0 WHERE id = ?1", [&p.id])?;
                job.log("Auto-deploy paused because you rolled back. Turn it on again after pushing a fix.");
            }
            Ok(false)
        }
        _ => {
            let why = match &script_res {
                Err(e) => format!("deploy script failed ({e:#})"),
                Ok(()) => "the site returns errors (5xx) after deploying".into(),
            };
            let Some(prev) = before.filter(|b| Some(b) != d.commit.as_ref()) else {
                bail!("{why}; nothing to roll back to");
            };
            job.log(&format!(
                "{why} — rolling back to {}",
                &prev[..prev.len().min(10)]
            ));
            let mut renv = env.clone();
            renv.push(("NC_PREV".into(), prev.clone()));
            as_user(
                p,
                job,
                &renv,
                "git reset --hard \"$NC_PREV\"",
                Duration::from_secs(120),
            )
            .await?;
            let _ = as_user(
                p,
                job,
                &env,
                &p.script.replace("php artisan migrate --force\n", ""),
                DEPLOY_TIMEOUT,
            )
            .await;
            reload_fpm(p, job).await;
            d.error = Some(why);
            Ok(true)
        }
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
        };
        let y = actions_yaml(&p);
        assert!(y.contains("branches: [main]"));
        assert!(y.contains("/hooks/github/shop.example.com"));
        assert!(y.contains("${{ secrets.NEXACLOUD_SECRET }}"));
    }
}
