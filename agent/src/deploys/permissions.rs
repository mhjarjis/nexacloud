//! File permissions that "just work" for PHP / Laravel projects.
//!
//! The rule: **the user PHP runs as owns the project**, and deploys, Artisan,
//! queue workers and cron run as that same user. Then `storage/` and
//! `bootstrap/cache` are writable by everything that needs them, and there's
//! no "laravel.log: Permission denied" because a deploy user and the PHP user
//! fight over the same files.
//!
//! - the PHP user is the site's own `nc_*` user (isolated sites), or the
//!   `user =` of the PHP-FPM pool serving the site (usually `www-data`);
//! - everything under the project folder is chowned to it (only when some
//!   file is owned by someone else, so it's cheap on every deploy);
//! - `storage/`, `bootstrap/cache` and upload folders get `ug+rwX` and the
//!   setgid bit on folders; Laravel's storage sub-folders are created;
//! - `.env` is 0640 (0600 on isolated sites).

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};

use super::Project;
use crate::jobs::JobCtx;
use crate::util::cmd::Cmd;

const SCRIPT: &str = r#"set -e
BASE="$1"; WEB="$2"; KIND="$3"; SHARED="$4"; APP="$5"; ISO="$6"
[ -d "$BASE" ] || { echo "$BASE is missing"; exit 1; }
if [ -n "$(find "$BASE" \( ! -user "$WEB" -o ! -group "$WEB" \) -print -quit 2>/dev/null)" ]; then
  chown -R "$WEB:$WEB" "$BASE"
  echo "files now owned by $WEB"
fi
if [ "$ISO" = 1 ]; then chmod 0750 "$BASE"; else chmod 0755 "$BASE"; fi
if [ "$KIND" = laravel ] && [ -d "$SHARED/storage" ]; then
  mkdir -p "$SHARED/storage/app/public" "$SHARED/storage/framework/cache/data" "$SHARED/storage/framework/sessions" "$SHARED/storage/framework/views" "$SHARED/storage/logs"
  chown -R "$WEB:$WEB" "$SHARED/storage"
fi
for d in "$SHARED/storage" "$APP/bootstrap/cache" "$SHARED/uploads" "$APP/uploads" "$APP/public/uploads" "$APP/public_html/uploads"; do
  r="$(readlink -f "$d" 2>/dev/null || true)"
  if [ -n "$r" ] && [ -d "$r" ]; then
    chmod -R u+rwX,g+rwX "$r"
    find "$r" -type d -exec chmod g+s {} +
  fi
done
for f in "$SHARED/.env" "$BASE/.env"; do
  if [ -f "$f" ] && [ ! -L "$f" ]; then
    if [ "$ISO" = 1 ]; then chmod 0600 "$f"; else chmod 0640 "$f"; fi
  fi
done
echo "permissions ok: owner $WEB, storage and cache writable"
"#;

/// `user =` of the PHP-FPM pool listening on `socket`.
fn pool_user_for_socket(socket: &str) -> Option<String> {
    let rd = std::fs::read_dir("/etc/php").ok()?;
    for v in rd.flatten() {
        let pool_dir = v.path().join("fpm/pool.d");
        let Ok(files) = std::fs::read_dir(&pool_dir) else {
            continue;
        };
        for f in files.flatten() {
            let Ok(t) = std::fs::read_to_string(f.path()) else {
                continue;
            };
            let mut listen = None;
            let mut user = None;
            for l in t.lines() {
                let l = l.trim();
                if let Some((k, val)) = l.split_once('=') {
                    match k.trim() {
                        "listen" => listen = Some(val.trim().to_string()),
                        "user" => user = Some(val.trim().to_string()),
                        _ => {}
                    }
                }
            }
            if listen.as_deref() == Some(socket) {
                return user;
            }
        }
    }
    None
}

/// The user PHP runs as for this project's site.
pub fn web_user(p: &Project) -> String {
    let model = crate::plans::sites::load_model(
        &crate::changes::ChangeEnv::system(std::path::PathBuf::from("/tmp")),
        &p.site_id,
    )
    .ok();
    if let Some(u) = model.as_ref().and_then(|m| m.user.clone()) {
        return u;
    }
    let socket = model.as_ref().and_then(|m| m.fpm_socket.clone()).or_else(|| {
        crate::nginx::merge_blocks(crate::routes::sites::read_blocks(
            Path::new("/etc/nginx/sites-enabled"),
            Path::new("/etc/nginx/conf.d"),
        ))
        .into_iter()
        .find(|b| b.id() == p.site_id)
        .and_then(|b| b.fastcgi_pass)
        .and_then(|f| f.strip_prefix("unix:").map(String::from))
    });
    socket
        .and_then(|s| pool_user_for_socket(&s))
        .filter(|u| !u.is_empty() && u != "root")
        .unwrap_or_else(|| "www-data".into())
}

/// Arguments for [`SCRIPT`] (used by the deploy step and the plan).
pub fn script_args(p: &Project) -> Vec<String> {
    let web = web_user(p);
    vec![
        "bash".into(),
        "-c".into(),
        SCRIPT.into(),
        "permissions".into(),
        p.dir.clone(),
        web.clone(),
        p.kind.clone(),
        p.shared_dir(),
        p.app_dir(),
        if web.starts_with("nc_") { "1" } else { "0" }.into(),
    ]
}

/// Fix ownership and modes now (as root). Logs to the job when given.
pub async fn fix(p: &Project, job: Option<&JobCtx>) -> Result<()> {
    if !Path::new(&p.dir).is_dir() {
        bail!("{} is missing", p.dir);
    }
    let args = script_args(p);
    let c = Cmd::new(&args[0]).args(&args[1..]).timeout(Duration::from_secs(900));
    let o = match job {
        Some(j) => c.run_streaming(j.log.as_ref()).await,
        None => c.run().await,
    };
    if !o.ok {
        bail!("{}", o.error_summary("fixing permissions"));
    }
    Ok(())
}

/// Problems a visitor would hit (empty when everything is fine).
pub async fn check(p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    if !Path::new(&p.dir).is_dir() {
        return out;
    }
    let web = web_user(p);
    if let Some((uid, _)) = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&p.dir).ok().map(|m| (m.uid(), m.gid()))
    } {
        let owner = crate::util::user_name(uid).unwrap_or_default();
        if owner != web {
            out.push(format!("{} is owned by {owner}, but PHP runs as {web}", p.dir));
        }
    }
    if p.kind == "laravel" {
        for rel in ["storage/logs", "storage/framework/views", "storage/framework/cache", "storage/framework/sessions"] {
            let d = format!("{}/{rel}", p.shared_dir());
            if Path::new(&d).exists() && !writable_by(&web, &d).await {
                out.push(format!("{rel} is not writable by {web}"));
            }
        }
        let bc = format!("{}/bootstrap/cache", p.app_dir());
        if Path::new(&bc).exists() && !writable_by(&web, &bc).await {
            out.push(format!("bootstrap/cache is not writable by {web}"));
        }
    }
    out
}

async fn writable_by(user: &str, path: &str) -> bool {
    Cmd::new("runuser")
        .args(["-u", user, "--", "test", "-w", path])
        .timeout(Duration::from_secs(5))
        .run()
        .await
        .ok
}

#[cfg(test)]
mod tests {
    #[test]
    fn script_is_strict_and_quoted() {
        assert!(super::SCRIPT.starts_with("set -e"));
        // every variable expansion is quoted
        assert!(!super::SCRIPT.contains(" $BASE "));
    }
}
