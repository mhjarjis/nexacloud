//! Zero-downtime deploys with release folders.
//!
//! ```text
//! /var/www/app.example.com/
//!   .nexacloud-repo/     git checkout used to fetch (never served)
//!   releases/41 42 43/   one folder per deploy (code only, `git archive`)
//!   shared/              .env, storage/ and anything else that must survive deploys
//!   current -> releases/43   what nginx serves (root …/current/public)
//! ```
//!
//! A deploy builds the next release folder, links everything in `shared/`
//! into it, runs tests / the deploy script there, then swaps `current`
//! atomically (`ln -s` + `mv -T`). If the site then fails its health check,
//! `current` goes back to the previous release. Rolling back to an older
//! commit that still has a folder only swaps the link.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReleaseMeta {
    pub commit: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub name: String,
    pub current: bool,
    #[serde(flatten)]
    pub meta: ReleaseMeta,
}

const META: &str = ".nexacloud-release";

pub fn releases_dir(p: &Project) -> String {
    format!("{}/releases", p.dir)
}

/// Release folder names, oldest first (numeric order).
pub fn names(p: &Project) -> Vec<String> {
    let mut v: Vec<(u64, String)> = std::fs::read_dir(releases_dir(p))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.parse::<u64>().ok().map(|k| (k, n))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

/// Name of the release `current` points to.
pub fn current(p: &Project) -> Option<String> {
    let t = std::fs::read_link(format!("{}/current", p.dir)).ok()?;
    t.file_name().map(|n| n.to_string_lossy().into_owned())
}

pub fn list(p: &Project) -> Vec<Release> {
    let cur = current(p);
    let mut v: Vec<Release> = names(p)
        .into_iter()
        .map(|n| {
            let meta = std::fs::read_to_string(format!("{}/{n}/{META}", releases_dir(p)))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            Release {
                current: cur.as_deref() == Some(n.as_str()),
                name: n,
                meta,
            }
        })
        .collect();
    v.reverse();
    v
}

fn next_name(p: &Project) -> String {
    names(p)
        .last()
        .and_then(|n| n.parse::<u64>().ok())
        .map(|n| n + 1)
        .unwrap_or(1)
        .to_string()
}

const FETCH: &str = r#"mkdir -p "$NC_BASE/releases" "$NC_BASE/shared"
mkdir -p "$NC_CACHE" && cd "$NC_CACHE"
"#;

const ARCHIVE: &str = r#"mkdir "$NC_RELEASE"
git archive --format=tar HEAD | tar -x -C "$NC_RELEASE"
"#;

/// Seed shared/storage from the first release (Laravel), then link every
/// entry of shared/ into the release.
const LINK_SHARED: &str = r#"cd "$NC_RELEASE"
if [ "$NC_KIND" = laravel ]; then
  if [ ! -d "$NC_SHARED/storage" ]; then
    if [ -d storage ]; then cp -a storage "$NC_SHARED/storage"; else mkdir -p "$NC_SHARED/storage"; fi
  fi
  mkdir -p "$NC_SHARED/storage/app/public" "$NC_SHARED/storage/framework/cache/data" "$NC_SHARED/storage/framework/sessions" "$NC_SHARED/storage/framework/views" "$NC_SHARED/storage/logs"
fi
linked=""
for f in "$NC_SHARED"/* "$NC_SHARED"/.[!.]*; do
  [ -e "$f" ] || continue
  name="$(basename "$f")"
  rm -rf "./$name"
  ln -s "../../shared/$name" "./$name"
  linked="$linked $name"
done
echo "Linked from shared/:${linked:- (nothing yet)}"
"#;

/// Point `current` at `releases/<name>` atomically. A real directory that is
/// in the way (a placeholder) is moved into releases/ first, never deleted.
const SWITCH: &str = r#"cd "$NC_BASE"
if [ -e current ] && [ ! -L current ]; then mv current "releases/pre-nexacloud-$(date +%s)"; fi
ln -sfn "releases/$NC_NAME" .current.tmp
mv -Tf .current.tmp current
echo "current -> releases/$NC_NAME"
"#;

async fn switch_to(p: &Project, job: &JobCtx, name: &str) -> Result<()> {
    as_user_in(
        p,
        job,
        &[
            ("NC_BASE".into(), p.dir.clone()),
            ("NC_NAME".into(), name.to_string()),
        ],
        SWITCH,
        Duration::from_secs(60),
        &p.dir,
    )
    .await
}

async fn remove_release(p: &Project, job: &JobCtx, name: &str) {
    if name.parse::<u64>().is_err() {
        return;
    }
    let _ = as_user_in(
        p,
        job,
        &[("NC_DIR".into(), format!("{}/{name}", releases_dir(p)))],
        "rm -rf \"$NC_DIR\"",
        Duration::from_secs(300),
        &p.dir,
    )
    .await;
}

/// Keep the newest `keep` releases (never the live one or the one before it).
async fn prune(p: &Project, job: &JobCtx) -> Vec<String> {
    let all = names(p);
    let cur = current(p);
    let keep = p.keep_releases.max(2) as usize;
    let mut removed = Vec::new();
    if all.len() <= keep {
        return removed;
    }
    for n in &all[..all.len() - keep] {
        if Some(n) == cur.as_ref() {
            continue;
        }
        remove_release(p, job, n).await;
        removed.push(n.clone());
    }
    removed
}

fn write_meta(p: &Project, name: &str, meta: &ReleaseMeta) {
    let path = format!("{}/{name}/{META}", releases_dir(p));
    if std::fs::write(&path, serde_json::to_string(meta).unwrap_or_default()).is_ok() {
        if let Some((uid, gid)) = owner_of(&p.dir) {
            let _ = std::os::unix::fs::chown(&path, Some(uid), Some(gid));
        }
    }
}

pub async fn run(
    state: &AppState,
    job: &JobCtx,
    p: &Project,
    d: &mut Deploy,
    commit: Option<&str>,
    env: &[(String, String)],
) -> Result<bool> {
    let previous = current(p);
    d.previous_commit = previous
        .as_ref()
        .and_then(|n| list(p).into_iter().find(|r| &r.name == n))
        .map(|r| r.meta.commit);

    // Rolling back to a commit that still has a folder: just switch.
    if let Some(c) = commit {
        if let Some(r) = list(p)
            .into_iter()
            .find(|r| r.meta.commit.starts_with(c) && !r.current)
        {
            d.release = Some(r.name.clone());
            d.commit = Some(r.meta.commit.clone());
            d.message = Some(r.meta.message.clone());
            d.author = Some(r.meta.author.clone());
            let s = step_begin(state, d, "Switch release", &format!("ln -sfn releases/{} current", r.name));
            switch_to(p, job, &r.name).await?;
            step_end(state, d, s, true, "Done");
            return health(state, job, p, d, previous.as_deref(), None).await;
        }
    }

    let name = next_name(p);
    let release = format!("{}/{name}", releases_dir(p));
    d.release = Some(name.clone());
    let cache = format!("{}/.nexacloud-repo", p.dir);
    let target = commit
        .map(String::from)
        .unwrap_or_else(|| format!("origin/{}", p.branch));
    let mut genv = git_env(p, env, &target);
    genv.push(("NC_BASE".into(), p.dir.clone()));
    genv.push(("NC_CACHE".into(), cache.clone()));
    genv.push(("NC_RELEASE".into(), release.clone()));
    genv.push(("NC_SHARED".into(), p.shared_dir()));
    genv.push(("NC_KIND".into(), p.kind.clone()));

    // 1) fetch into the cache checkout, export into releases/<n>
    let s = step_begin(state, d, "Fetch code", &format!("git fetch && archive {} → releases/{name}", short(&target)));
    job.log(&format!("Fetching {} ({}) into releases/{name}", p.repo, p.branch));
    let script = format!("{FETCH}{GIT_SYNC}{ARCHIVE}");
    if let Err(e) = as_user_in(p, job, &genv, &script, Duration::from_secs(600), &p.dir).await {
        step_end(state, d, s, false, "Failed");
        remove_release(p, job, &name).await;
        bail!("git fetch failed ({e:#})");
    }
    if let Some((sha, msg, author)) = head_info_at(&cache, env).await {
        job.log(&format!("Release {name} = {} — {msg} ({author})", short(&sha)));
        write_meta(
            p,
            &name,
            &ReleaseMeta {
                commit: sha.clone(),
                message: msg.clone(),
                author: author.clone(),
                at: now(),
            },
        );
        d.commit = Some(sha);
        d.message = Some(msg);
        d.author = Some(author);
    }
    let res = d.commit.as_deref().map(short).unwrap_or("Done").to_string();
    step_end(state, d, s, true, &res);

    // 2) shared files
    let s = step_begin(state, d, "Link shared files", ".env, storage/ … from shared/");
    if let Err(e) = as_user_in(p, job, &genv, LINK_SHARED, Duration::from_secs(300), &p.dir).await {
        step_end(state, d, s, false, "Failed");
        remove_release(p, job, &name).await;
        bail!("linking shared files failed ({e:#})");
    }
    step_end(state, d, s, true, "Done");

    // 3) tests → backup → script, all inside the new folder (site untouched)
    let fail_build = |why: String| anyhow!("{why} — nothing changed on the live site");
    if p.run_tests && p.kind == "laravel" {
        let s = step_begin(state, d, "Run tests", "composer install && php artisan test");
        let r = as_user_in(p, job, env, TEST_SCRIPT, DEPLOY_TIMEOUT, &release).await;
        step_end(state, d, s, r.is_ok(), if r.is_ok() { "Passed" } else { "Failed" });
        if let Err(e) = r {
            remove_release(p, job, &name).await;
            skip_rest(state, d, &name);
            return Err(fail_build(format!("tests failed ({e:#})")));
        }
    }
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
    let s = step_begin(state, d, "Run deploy script", &script_summary(&p.script));
    job.log(&format!("Running deploy script in releases/{name}"));
    let mut senv = env.to_vec();
    senv.push(("RELEASE".into(), release.clone()));
    let r = as_user_in(p, job, &senv, &p.script, DEPLOY_TIMEOUT, &release).await;
    step_end(state, d, s, r.is_ok(), if r.is_ok() { "Done" } else { "Failed" });
    if let Err(e) = r {
        remove_release(p, job, &name).await;
        skip_rest(state, d, &name);
        return Err(fail_build(format!("deploy script failed ({e:#})")));
    }

    // 4) go live
    let s = step_begin(state, d, "Switch release", &format!("ln -sfn releases/{name} current"));
    if let Err(e) = switch_to(p, job, &name).await {
        step_end(state, d, s, false, "Failed");
        remove_release(p, job, &name).await;
        return Err(fail_build(format!("switching failed ({e:#})")));
    }
    step_end(state, d, s, true, "Done");
    health(state, job, p, d, previous.as_deref(), Some(&name)).await
}

fn skip_rest(state: &AppState, d: &mut Deploy, name: &str) {
    step_skip(state, d, "Switch release", &format!("ln -sfn releases/{name} current"), "Skipped");
    step_skip(state, d, "Reload & health check", "php-fpm reload · GET /", "Skipped");
}

/// Reload, probe; on failure go back to `previous` (and drop the new folder).
async fn health(
    state: &AppState,
    job: &JobCtx,
    p: &Project,
    d: &mut Deploy,
    previous: Option<&str>,
    built: Option<&str>,
) -> Result<bool> {
    let s = step_begin(state, d, "Reload & health check", &format!("php-fpm reload · GET {}/", p.site_id));
    reload_fpm(p, job).await;
    if p.kind == "laravel" && Path::new(&format!("{}/current/artisan", p.dir)).exists() {
        // Workers keep old code in memory; restart them on the new release.
        let _ = as_user_in(
            p,
            job,
            &[],
            &format!("{} artisan queue:restart", php_bin(&p.site_id)),
            Duration::from_secs(60),
            &format!("{}/current", p.dir),
        )
        .await;
    }
    match probe(&p.site_id).await {
        Some(code) => {
            step_end(state, d, s, true, &format!("HTTP {code}"));
            job.log(&format!("Health check: HTTP {code} — release {} is live", d.release.as_deref().unwrap_or("?")));
            let removed = prune(p, job).await;
            if !removed.is_empty() {
                job.log(&format!("Removed old releases: {}", removed.join(", ")));
            }
            Ok(false)
        }
        None => {
            step_end(state, d, s, false, "5xx");
            let Some(prev) = previous.filter(|pv| Some(*pv) != d.release.as_deref()) else {
                bail!("the site returns errors (5xx) and there is no earlier release to go back to");
            };
            job.log(&format!("Health check failed — switching back to release {prev}"));
            switch_to(p, job, prev).await?;
            reload_fpm(p, job).await;
            if let Some(b) = built {
                remove_release(p, job, b).await;
            }
            d.error = Some(format!(
                "the site returned errors (5xx) on the new release, so release {prev} is live again{}",
                if p.script.contains("migrate") { " (database migrations are not undone)" } else { "" }
            ));
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scripts_are_strict_shell() {
        // set -e friendly: no bare `[ … ] &&` chains that would abort the script.
        for s in [super::LINK_SHARED, super::SWITCH] {
            assert!(!s.lines().any(|l| l.trim_start().starts_with("[ ") && l.contains("] &&")));
        }
    }
}
