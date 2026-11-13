//! Restore a whole site from a backup into a separate staging copy:
//! files → `/var/www/<staging domain>`, database → `<db>_staging_<date>` with
//! its own user, `.env` pointed at both (mail to the log, debug off, config
//! cache cleared), served by a new isolated nginx site. The live site and
//! database are never touched.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use super::*;
use crate::plans::sites::{self, SiteKind};

#[derive(Debug, Clone, Deserialize)]
pub struct StagingBody {
    /// The folder item to restore (its original path, e.g. /var/www/crm).
    pub files: String,
    /// Optional database item (`mysql/crm_prod`).
    #[serde(default)]
    pub database: Option<String>,
    /// Domain for the staging copy, e.g. staging.crm.example.com.
    pub domain: String,
    /// PHP version for the staging pool (defaults to the newest installed).
    #[serde(default)]
    pub php_version: Option<String>,
}

pub fn validate(state: &AppState, run: &Run, b: &StagingBody) -> Result<(), String> {
    if run.status != "ok" {
        return Err("only successful backups can be restored".into());
    }
    if !sites::valid_domain(&b.domain) {
        return Err("enter a staging domain like staging.example.com".into());
    }
    let dir = format!("/var/www/{}", b.domain.to_ascii_lowercase());
    if Path::new(&dir).exists() {
        return Err(format!("{dir} already exists — pick another staging domain"));
    }
    if !run.items.iter().any(|i| i.kind == "files" && i.source == b.files) {
        return Err(format!("{} is not a folder in this backup", b.files));
    }
    if let Some(db) = &b.database {
        if !run.items.iter().any(|i| i.kind == "database" && &i.source == db) {
            return Err(format!("{db} is not a database in this backup"));
        }
    }
    if sites::load_model(&state.changes_env, &b.domain).is_ok() {
        return Err(format!("a site {} already exists", b.domain));
    }
    Ok(())
}

pub fn submit(state: &AppState, run: &Run, b: StagingBody, actor: Option<String>) -> Result<String> {
    let domain = b.domain.to_ascii_lowercase();
    let spec = JobSpec::new("backup_staging", &format!("Restore {} to staging {domain}", b.files))
        .heavy()
        .lock(format!("site:{domain}"))
        .timeout(RUN_TIMEOUT)
        .actor(actor);
    let (st, run) = (state.clone(), run.clone());
    state
        .jobs
        .submit(spec, move |ctx| async move { restore(&st, &ctx, &run, &b, &domain).await })
}

fn set_env(text: &str, vals: &[(&str, String)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut done = std::collections::HashSet::new();
    for line in text.lines() {
        let key = line.split_once('=').map(|(k, _)| k.trim().to_string());
        match key.and_then(|k| vals.iter().find(|(vk, _)| *vk == k)) {
            Some((k, v)) => {
                out.push(format!("{k}={}", crate::plans::projects::quote_env(v)));
                done.insert(*k);
            }
            None => out.push(line.to_string()),
        }
    }
    for (k, v) in vals {
        if !done.contains(k) {
            out.push(format!("{k}={}", crate::plans::projects::quote_env(v)));
        }
    }
    out.join("\n") + "\n"
}

async fn sh(job: &JobCtx, script: &str, args: &[&str]) -> Result<()> {
    let o = Cmd::new("bash")
        .args(["-c", script, "staging"])
        .args(args)
        .timeout(RUN_TIMEOUT)
        .run_streaming(job.log.as_ref())
        .await;
    if !o.ok {
        bail!("{}", o.error_summary("command"));
    }
    Ok(())
}

async fn restore(state: &AppState, job: &JobCtx, run: &Run, b: &StagingBody, domain: &str) -> Result<()> {
    let dir = format!("/var/www/{domain}");
    let item = run
        .items
        .iter()
        .find(|i| i.kind == "files" && i.source == b.files)
        .ok_or_else(|| anyhow!("folder not in backup"))?
        .clone();

    // 1) files
    job.log(&format!("== 1/4 Files: {} → {dir}", b.files));
    let (archive, tmp1) = fetch_item(state, job, run, &item).await?;
    let unpack = PathBuf::from(format!("/var/www/.nexacloud-staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&unpack)?;
    let res = sh(
        job,
        "set -e; tar -xzf \"$1\" -C \"$2\"; mv \"$2/$3\" \"$4\"",
        &[
            &archive.to_string_lossy(),
            &unpack.to_string_lossy(),
            b.files.trim_start_matches('/'),
            &dir,
        ],
    )
    .await;
    let _ = std::fs::remove_dir_all(&unpack);
    if let Some(t) = tmp1 {
        let _ = std::fs::remove_dir_all(t);
    }
    res.context("unpacking the files failed")?;

    // Layout: release folders or plain.
    let app = if Path::new(&format!("{dir}/current")).is_symlink() {
        format!("{dir}/current")
    } else {
        dir.clone()
    };
    let laravel = Path::new(&format!("{app}/artisan")).exists();
    let root = ["public", "public_html", "web", "htdocs"]
        .iter()
        .map(|r| format!("{app}/{r}"))
        .find(|r| Path::new(r).is_dir())
        .unwrap_or_else(|| app.clone());
    let env_path = [format!("{dir}/shared/.env"), format!("{dir}/.env")]
        .into_iter()
        .find(|p| Path::new(p).is_file() && !Path::new(p).is_symlink());

    // 2) database
    let mut env_vals: Vec<(&str, String)> = vec![
        ("APP_ENV", "staging".into()),
        ("APP_DEBUG", "false".into()),
        ("APP_URL", format!("http://{domain}")),
        ("MAIL_MAILER", "log".into()),
        ("QUEUE_CONNECTION", "sync".into()),
    ];
    if let Some(src) = &b.database {
        let ditem = run
            .items
            .iter()
            .find(|i| i.kind == "database" && &i.source == src)
            .ok_or_else(|| anyhow!("database not in backup"))?
            .clone();
        let (engine, name) = src.split_once('/').ok_or_else(|| anyhow!("bad database"))?;
        let engine_s = if engine == "postgres" { "postgres" } else { "mysql" };
        let mut target = format!("{name}_staging_{}", &run.stamp[..8]);
        target.truncate(60);
        let user = crate::apps::db_name(&format!("stg.{domain}"));
        job.log(&format!("== 2/4 Database {src} → {target} (user {user})"));
        let pw = crate::dbm::create_db_with_user(engine_s, &target, Some(&user))
            .await
            .map_err(|e| anyhow!("creating {target} failed: {e}"))?
            .unwrap_or_default();
        let (file, tmp2) = fetch_item(state, job, run, &ditem).await?;
        let r = if engine_s == "mysql" {
            sh(job, "set -o pipefail; gunzip -c \"$1\" | mysql \"$2\"", &[&file.to_string_lossy(), &target]).await
        } else {
            let r = sh(
                job,
                "set -o pipefail; gunzip -c \"$1\" | runuser -u postgres -- psql -q -v ON_ERROR_STOP=1 -d \"$2\"",
                &[&file.to_string_lossy(), &target],
            )
            .await;
            let _ = sh(
                job,
                "runuser -u postgres -- psql -q -d \"$1\" -c \"GRANT ALL ON ALL TABLES IN SCHEMA public TO \\\"$2\\\"; GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO \\\"$2\\\";\"",
                &[&target, &user],
            )
            .await;
            r
        };
        if let Some(t) = tmp2 {
            let _ = std::fs::remove_dir_all(t);
        }
        r.context("importing the database failed")?;
        env_vals.push(("DB_DATABASE", target.clone()));
        env_vals.push(("DB_USERNAME", user.clone()));
        env_vals.push(("DB_PASSWORD", pw));
        env_vals.push(("DB_HOST", "127.0.0.1".into()));
    } else {
        job.log("== 2/4 Database: not restored");
    }

    // 3) .env + caches
    job.log("== 3/4 .env for staging (debug off, mail → log, queue sync)");
    if let Some(p) = &env_path {
        let text = std::fs::read_to_string(p).unwrap_or_default();
        std::fs::write(p, set_env(&text, &env_vals))?;
    } else {
        job.log("   no .env in the backup — set it from the project page later");
    }
    // A cached config holds the live database password and paths.
    let _ = std::fs::remove_file(format!("{app}/bootstrap/cache/config.php"));

    // 4) user + site
    job.log(&format!("== 4/4 Site {domain} with its own user"));
    let php = match &b.php_version {
        Some(v) => v.clone(),
        None => crate::routes::php::info(&crate::routes::sites::scan(false).await)
            .await
            .versions
            .iter()
            .filter(|v| v.fpm_installed)
            .map(|v| v.version.clone())
            .next_back()
            .ok_or_else(|| anyhow!("no PHP-FPM installed"))?,
    };
    let u = sites::site_user(domain);
    sh(
        job,
        "set -e; id -u \"$1\" >/dev/null 2>&1 || useradd --system --user-group --home-dir \"$2\" --no-create-home --shell /usr/sbin/nologin \"$1\"; chown -R \"$1:$1\" \"$2\"; chmod 0750 \"$2\"; for f in \"$2/.env\" \"$2/shared/.env\"; do if [ -f \"$f\" ] && [ ! -L \"$f\" ]; then chmod 0600 \"$f\"; fi; done",
        &[&u, &dir],
    )
    .await?;
    let plan = sites::plan_create(
        &state.changes_env,
        &sites::CreateParams {
            kind: if laravel { SiteKind::Laravel } else { SiteKind::Php },
            domain: domain.to_string(),
            aliases: vec![],
            root: Some(root),
            php_version: Some(php),
            upstream: None,
            redirect_to: None,
            redirect_code: None,
            options: None,
            isolate: true,
            no_placeholder: true,
        },
    )
    .map_err(|e| anyhow!(e))?;
    crate::changes::apply_now(&state.db, &state.changes_env, plan, "staging restore", &job.log).await?;
    job.log(&format!(
        "Staging copy is live at http://{domain}/ — point the domain's DNS at this server (or add it to your computer's hosts file). Queue workers and cron are not started for staging."
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_rewrite() {
        let t = "APP_ENV=production\nDB_DATABASE=crm\n# keep\nMAIL_MAILER=smtp\n";
        let out = super::set_env(t, &[("APP_ENV", "staging".into()), ("DB_DATABASE", "crm_staging".into()), ("NEW", "a b".into())]);
        assert_eq!(out, "APP_ENV=staging\nDB_DATABASE=crm_staging\n# keep\nMAIL_MAILER=smtp\nNEW=\"a b\"\n");
    }
}
