//! One-click WordPress: site (own user + PHP pool), MySQL database,
//! WordPress files, wp-config.php with fresh salts, the admin account
//! (through WordPress's own installer), optional HTTPS.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use axum::extract::{ConnectInfo, State};
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

const WP_URL: &str = "https://wordpress.org/latest.tar.gz";

#[derive(Debug, Clone, Deserialize)]
pub struct NewWordPress {
    pub domain: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub php_version: String,
    pub title: String,
    pub admin_user: String,
    pub admin_email: String,
    pub admin_password: String,
    /// Let's Encrypt contact; HTTPS is skipped when empty.
    #[serde(default)]
    pub ssl_email: String,
    /// Ask search engines not to index (staging).
    #[serde(default)]
    pub hide_from_search: bool,
}

fn validate(state: &AppState, w: &NewWordPress) -> Result<(), String> {
    if !sites::valid_domain(&w.domain) {
        return Err("enter a domain like blog.example.com".into());
    }
    if !sites::valid_php_version(&w.php_version) {
        return Err("choose a PHP version".into());
    }
    if w.title.trim().is_empty() || w.title.len() > 120 {
        return Err("site title: 1–120 characters".into());
    }
    let u = w.admin_user.trim();
    if u.len() < 3 || u.len() > 60 || !u.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)) || u.eq_ignore_ascii_case("admin") {
        return Err("admin username: 3–60 letters, digits, . _ - @ (not \"admin\" — bots try that first)".into());
    }
    if !crate::monitor::notify::valid_email(&w.admin_email) {
        return Err("admin email is not valid".into());
    }
    if w.admin_password.chars().count() < 12 || w.admin_password.len() > 200 {
        return Err("admin password: at least 12 characters".into());
    }
    if !w.ssl_email.is_empty() && !crate::monitor::notify::valid_email(&w.ssl_email) {
        return Err("the HTTPS email address is not valid".into());
    }
    if deploys::get_project(&state.db, &w.domain).ok().flatten().is_some() {
        return Err(format!("{} already exists", w.domain));
    }
    Ok(())
}

fn site_params(w: &NewWordPress) -> sites::CreateParams {
    sites::CreateParams {
        kind: SiteKind::Php,
        domain: w.domain.clone(),
        aliases: w.aliases.clone(),
        root: Some(format!("/var/www/{}", w.domain)),
        php_version: Some(w.php_version.clone()),
        upstream: None,
        redirect_to: None,
        redirect_code: None,
        options: Some(sites::SiteOptions { client_max_body_size: "128M".into(), ..Default::default() }),
        isolate: true,
        no_placeholder: true,
    }
}

/// `POST /api/apps/wordpress` → `{job_id, project_id}`.
pub async fn create(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>, Json(w): Json<NewWordPress>) -> Response {
    let mut info = AuditInfo::action("app.wordpress", w.domain.clone());
    let res: ApiResult<Response> = async {
        let mut w = w;
        w.domain = w.domain.trim().to_ascii_lowercase();
        validate(&state, &w).map_err(ApiError::bad_request)?;
        let dbs = crate::routes::databases::mysql_info().await;
        if !dbs.available {
            return Err(ApiError::bad_request("WordPress needs MySQL — install it under PHP & Services first"));
        }
        sites::plan_create(&state.changes_env, &site_params(&w)).map_err(ApiError::conflict)?;
        let spec = JobSpec::new("app_wordpress", &format!("WordPress on {}", w.domain))
            .lock(format!("site:{}", w.domain))
            .timeout(Duration::from_secs(1800))
            .actor(Some(addr.ip().to_canonical().to_string()));
        let st = state.clone();
        let w2 = w.clone();
        let job = state.jobs.submit(spec, move |ctx| async move { build(&st, &ctx, &w2).await })?;
        info.job_id = Some(job.clone());
        Ok(Json(json!({"job_id": job, "project_id": w.domain})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn salt() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

fn php_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

pub fn wp_config(db: &str, user: &str, password: &str, https: bool) -> String {
    let keys = ["AUTH_KEY", "SECURE_AUTH_KEY", "LOGGED_IN_KEY", "NONCE_KEY", "AUTH_SALT", "SECURE_AUTH_SALT", "LOGGED_IN_SALT", "NONCE_SALT"];
    let mut s = String::from("<?php\n// Written by NexaCloud. Database password and salts are unique to this site.\n");
    s.push_str(&format!("define( 'DB_NAME', '{}' );\ndefine( 'DB_USER', '{}' );\ndefine( 'DB_PASSWORD', '{}' );\ndefine( 'DB_HOST', '127.0.0.1' );\ndefine( 'DB_CHARSET', 'utf8mb4' );\ndefine( 'DB_COLLATE', '' );\n\n", php_str(db), php_str(user), php_str(password)));
    for k in keys {
        s.push_str(&format!("define( '{k}', '{}' );\n", salt()));
    }
    s.push_str("\n$table_prefix = 'wp_';\n\ndefine( 'WP_DEBUG', false );\ndefine( 'FS_METHOD', 'direct' );\ndefine( 'DISALLOW_FILE_EDIT', true );\ndefine( 'WP_AUTO_UPDATE_CORE', 'minor' );\n");
    if https {
        s.push_str("if ( isset( $_SERVER['HTTP_X_FORWARDED_PROTO'] ) && $_SERVER['HTTP_X_FORWARDED_PROTO'] === 'https' ) { $_SERVER['HTTPS'] = 'on'; }\n");
    }
    s.push_str("\nif ( ! defined( 'ABSPATH' ) ) {\n\tdefine( 'ABSPATH', __DIR__ . '/' );\n}\nrequire_once ABSPATH . 'wp-settings.php';\n");
    s
}

async fn apply(state: &AppState, job: &JobCtx, plan: changes::ChangePlan) -> Result<()> {
    changes::apply_now(&state.db, &state.changes_env, plan, "WordPress installer", &job.log).await.map(|_| ())
}

async fn sh_as(user: &str, script: &str, args: &[&str], secs: u64) -> crate::util::cmd::CmdOutput {
    let mut a: Vec<String> = vec!["-u".into(), user.into(), "--".into(), "sh".into(), "-c".into(), script.into(), "wp".into()];
    a.extend(args.iter().map(|s| s.to_string()));
    Cmd::new("runuser").args(a).timeout(Duration::from_secs(secs)).run().await
}

async fn build(state: &AppState, job: &JobCtx, w: &NewWordPress) -> Result<()> {
    let base = format!("/var/www/{}", w.domain);

    job.log(&format!("== 1/5 Site {} (PHP {}, own user and PHP pool)", w.domain, w.php_version));
    let plan = sites::plan_create(&state.changes_env, &site_params(w)).map_err(|e| anyhow!(e))?;
    apply(state, job, plan).await?;
    let model = sites::load_model(&state.changes_env, &w.domain).map_err(|e| anyhow!(e))?;
    let run_as = model.user.clone().unwrap_or_else(|| "www-data".into());

    job.log("== 2/5 MySQL database");
    let name = crate::apps::db_name(&w.domain);
    let pw = crate::dbm::create_db_with_user("mysql", &name, Some(&name)).await.map_err(|e| anyhow!("creating the database failed: {e}"))?.unwrap_or_default();
    job.log(&format!("   database and user {name} (password saved in wp-config.php)"));

    job.log("== 3/5 Download WordPress");
    let url = std::env::var("NEXACLOUD_WP_URL").unwrap_or_else(|_| WP_URL.into());
    let o = sh_as(
        &run_as,
        "set -e; cd \"$1\"; curl -fsSL --retry 3 \"$2\" -o .wp.tar.gz; tar -xzf .wp.tar.gz --strip-components=1 --no-same-owner; rm -f .wp.tar.gz; test -f wp-settings.php",
        &[&base, &url],
        600,
    )
    .await;
    if !o.ok {
        bail!("downloading WordPress failed: {}", o.error_summary("curl/tar"));
    }
    let ver = std::fs::read_to_string(format!("{base}/wp-includes/version.php")).ok().and_then(|t| t.lines().find(|l| l.starts_with("$wp_version")).and_then(|l| l.split('\'').nth(1).map(str::to_string))).unwrap_or_default();
    job.log(&format!("   WordPress {ver}"));
    let uid = std::fs::metadata(&base).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(33);
    let gid = std::fs::metadata(&base).map(|m| std::os::unix::fs::MetadataExt::gid(&m)).unwrap_or(33);
    crate::util::atomic_write(std::path::Path::new(&format!("{base}/wp-config.php")), wp_config(&name, &name, &pw, !w.ssl_email.is_empty()).as_bytes(), 0o640, Some((uid, gid)))?;
    job.log("   wp-config.php written (readable by the site user only)");

    job.log("== 4/5 Create the admin account");
    let form = [
        ("weblog_title", w.title.trim().to_string()),
        ("user_name", w.admin_user.trim().to_string()),
        ("admin_password", w.admin_password.clone()),
        ("admin_password2", w.admin_password.clone()),
        ("pw_weak", "1".into()),
        ("admin_email", w.admin_email.trim().to_string()),
        ("blog_public", if w.hide_from_search { "0".into() } else { "1".into() }),
        ("language", String::new()),
    ];
    let body: String = form.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect::<Vec<_>>().join("&");
    // POST the form to WordPress's installer over loopback (body on stdin: no password in argv).
    let o = Cmd::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "120", "--resolve", &format!("{}:80:127.0.0.1", w.domain), "--data-binary", "@-", &format!("http://{}/wp-admin/install.php?step=2", w.domain)])
        .stdin(body)
        .timeout(Duration::from_secs(130))
        .run()
        .await;
    let installed = Cmd::new("mysql").args(["-N", "-e", &format!("SELECT COUNT(*) FROM `{name}`.wp_users")]).run().await;
    if installed.stdout.trim() == "1" {
        job.log(&format!("   admin {} created", w.admin_user.trim()));
    } else {
        bail!("WordPress's installer did not finish (HTTP {}). Open http://{}/wp-admin/install.php to finish it by hand.", o.stdout.trim(), w.domain);
    }

    let p = Project {
        id: w.domain.clone(),
        site_id: w.domain.clone(),
        kind: "php".into(),
        dir: base.clone(),
        repo: String::new(),
        branch: "main".into(),
        run_as: run_as.clone(),
        script: deploys::default_script("php", false),
        migrate: false,
        auto_deploy: false,
        webhook_secret: {
            use rand::distr::{Alphanumeric, SampleString};
            Alphanumeric.sample_string(&mut rand::rng(), 40)
        },
        created_at: crate::util::fmt_ts(chrono::Utc::now()),
        last_deploy_at: None,
        last_commit: None,
        strategy: "in_place".into(),
        keep_releases: 5,
        run_tests: false,
        notify: true,
    };
    deploys::save_project(&state.db, &p)?;

    if w.ssl_email.is_empty() {
        job.log("== 5/5 HTTPS: skipped (turn it on later from Domains & SSL)");
    } else {
        job.log("== 5/5 HTTPS (Let's Encrypt)");
        match sites::plan_ssl(&state.changes_env, &sites::SslParams { id: w.domain.clone(), email: w.ssl_email.clone() }) {
            Ok(plan) => {
                if let Err(e) = apply(state, job, plan).await {
                    job.log(&format!("   HTTPS failed: {e:#}. Check that {} points to this server, then turn it on from Domains & SSL.", w.domain));
                } else {
                    let _ = Cmd::new("mysql").args(["-e", &format!("UPDATE `{name}`.wp_options SET option_value = REPLACE(option_value, 'http://', 'https://') WHERE option_name IN ('siteurl','home')")]).run().await;
                }
            }
            Err(e) => job.log(&format!("   HTTPS skipped: {e}")),
        }
    }
    job.log(&format!("Done — sign in at {}://{}/wp-admin/", if w.ssl_email.is_empty() { "http" } else { "https" }, w.domain));
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_escapes_and_salts() {
        let c = wp_config("blog_db", "blog_db", "pa'ss\\word", false);
        assert!(c.contains("define( 'DB_PASSWORD', 'pa\\'ss\\\\word' );"));
        assert_eq!(c.matches("_KEY', '").count() + c.matches("_SALT', '").count(), 8);
        assert!(c.contains("DISALLOW_FILE_EDIT"));
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
    }
}
