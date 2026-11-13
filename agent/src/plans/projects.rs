//! Project plans: .env editor, queue workers (Supervisor) and the Laravel
//! scheduler (cron). All through the change pipeline.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::changes::{ChangeEnv, ChangePlan};
use crate::deploys::routes::{cron_name, sup_name};
use crate::deploys::Project;

pub fn valid_env_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 80
        && k.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !k.as_bytes()[0].is_ascii_digit()
}

pub fn quote_env(v: &str) -> String {
    if v.is_empty()
        || v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/@+,".contains(&b))
    {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvParams {
    pub id: String,
    /// key → new value (`null` removes the key). Keys not listed are kept.
    pub set: BTreeMap<String, Option<String>>,
}

pub fn plan_env(env: &ChangeEnv, p: &Project, params: &EnvParams) -> Result<ChangePlan, String> {
    if params.set.is_empty() {
        return Err("nothing to change".into());
    }
    let path = format!("{}/.env", p.shared_dir());
    let current = std::fs::read_to_string(env.host_path(&path)).unwrap_or_default();
    let exists = env.host_path(&path).exists();
    let owner = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(env.host_path(&p.shared_dir()))
            .or_else(|_| std::fs::metadata(env.host_path(&p.dir)))
            .ok()
            .map(|m| (m.uid(), m.gid()))
    };
    let shared_missing = !env.host_path(&p.shared_dir()).exists();
    let isolated = owner.and_then(|(uid, _)| crate::util::user_name(uid)).is_some_and(|n| n.starts_with("nc_"));
    let mut remaining = params.set.clone();
    for k in remaining.keys() {
        if !valid_env_key(k) {
            return Err(format!(
                "invalid variable name {k:?} (use UPPER_SNAKE_CASE)"
            ));
        }
    }
    for v in remaining.values().flatten() {
        if v.contains('\n') || v.len() > 4000 {
            return Err("values must be a single line".into());
        }
    }
    let mut out = String::new();
    for line in current.lines() {
        let key = line
            .split_once('=')
            .map(|(k, _)| k.trim())
            .filter(|_| !line.trim_start().starts_with('#'));
        match key.and_then(|k| remaining.remove_entry(k)) {
            Some((k, Some(v))) => out.push_str(&format!("{k}={}\n", quote_env(&v))),
            Some((_, None)) => {}
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for (k, v) in remaining {
        if let Some(v) = v {
            out.push_str(&format!("{k}={}\n", quote_env(&v)));
        }
    }
    let mut b = ChangePlan::builder(
        "project_env",
        &format!("Update .env of {}", p.id),
        &format!("site:{}", p.site_id),
    )
    .lock(&format!("site:{}", p.site_id))
    .sensitive();
    if shared_missing {
        b = b.create_parents().pre_command([
            "install", "-d", "-m", "0750", "-o", &p.run_as, "-g", &p.run_as, &p.shared_dir(),
        ]);
    }
    // Keep the existing mode/owner; a new .env belongs to the site user
    // (readable by that user only on isolated sites).
    b = b.write_file_with(
        &path,
        crate::changes::Content::Text(out),
        if exists { None } else { Some(if isolated { 0o600 } else { 0o640 }) },
        if exists { None } else { owner },
    );
    if p.kind == "laravel" && env.host_path(&format!("{}/artisan", p.app_dir())).exists() {
        b = b.command([
            "runuser",
            "-u",
            &p.run_as,
            "--",
            "php",
            &format!("{}/artisan", p.app_dir()),
            "config:clear",
        ]);
    }
    b = b.verify_command([
        "sh",
        "-c",
        "for i in 1 2 3 4 5 6; do code=$(curl -ks -o /dev/null -w '%{http_code}' --resolve \"$1:80:127.0.0.1\" --resolve \"$1:443:127.0.0.1\" -L \"http://$1/\"); if [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ]; then echo \"HTTP $code\"; exit 0; fi; sleep 1; done; echo \"HTTP $code\"; exit 1",
        "probe",
        &p.site_id,
    ]);
    b.note("Values are written to the project's .env; Laravel's config cache is cleared so they take effect. If the site then returns errors, the old .env is restored automatically.")
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvRawParams {
    pub id: String,
    pub content: String,
}

/// Replace the whole .env ("Edit as text"). Same safety net as [`plan_env`].
pub fn plan_env_raw(env: &ChangeEnv, p: &Project, params: &EnvRawParams) -> Result<ChangePlan, String> {
    if params.content.len() > 64_000 {
        return Err(".env is too large".into());
    }
    for (i, l) in params.content.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        match t.split_once('=') {
            Some((k, _)) if valid_env_key(k.trim().trim_start_matches("export ")) => {}
            _ => return Err(format!("line {}: expected NAME=value", i + 1)),
        }
    }
    let path = format!("{}/.env", p.shared_dir());
    let exists = env.host_path(&path).exists();
    let owner = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(env.host_path(&p.shared_dir()))
            .or_else(|_| std::fs::metadata(env.host_path(&p.dir)))
            .ok()
            .map(|m| (m.uid(), m.gid()))
    };
    let mut content = params.content.replace("\r\n", "\n");
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let mut b = ChangePlan::builder("project_env", &format!("Replace .env of {}", p.id), &format!("site:{}", p.site_id))
        .lock(&format!("site:{}", p.site_id))
        .sensitive()
        .write_file_with(
            &path,
            crate::changes::Content::Text(content),
            if exists { None } else { Some(0o640) },
            if exists { None } else { owner },
        );
    if p.kind == "laravel" && env.host_path(&format!("{}/artisan", p.app_dir())).exists() {
        b = b.command(["runuser", "-u", &p.run_as, "--", "php", &format!("{}/artisan", p.app_dir()), "config:clear"]);
    }
    b.verify_command([
        "sh",
        "-c",
        "for i in 1 2 3 4 5 6; do code=$(curl -ks -o /dev/null -w '%{http_code}' --resolve \"$1:80:127.0.0.1\" --resolve \"$1:443:127.0.0.1\" -L \"http://$1/\"); if [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ]; then echo \"HTTP $code\"; exit 0; fi; sleep 1; done; echo \"HTTP $code\"; exit 1",
        "probe",
        &p.site_id,
    ])
    .note("The whole .env is replaced; Laravel's config cache is cleared. If the site then returns errors, the old file is restored automatically.")
    .build(env)
    .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerParams {
    pub id: String,
    pub enabled: bool,
    #[serde(default = "two")]
    pub processes: u32,
    #[serde(default = "default_q")]
    pub queue: String,
    /// Several named workers (each with its own queues). When absent, one
    /// worker named "default" from `processes` + `queue`.
    #[serde(default)]
    pub workers: Option<Vec<WorkerSpec>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerSpec {
    pub name: String,
    pub queue: String,
    pub processes: u32,
}

fn two() -> u32 {
    2
}
fn default_q() -> String {
    "default".into()
}

fn valid_queue_list(q: &str) -> bool {
    q.split(',').all(|q| {
        !q.is_empty()
            && q.len() <= 40
            && q.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    })
}

pub fn worker_conf(p: &Project, workers: &[WorkerSpec], php: &str) -> String {
    let base = format!("nexacloud-{}", sup_name(&p.id));
    let app = p.app_dir();
    let programs: Vec<String> = workers.iter().map(|w| format!("{base}-{}", w.name)).collect();
    let mut s = format!(
        "; Managed by NexaCloud — queue workers for {id}\n[group:{base}]\nprograms={progs}\n",
        id = p.id,
        progs = programs.join(",")
    );
    for w in workers {
        s.push_str(&format!(
            "\n[program:{base}-{name}]\nprocess_name=%(program_name)s_%(process_num)02d\ncommand={php} {app}/artisan queue:work --queue={q} --sleep=3 --tries=3 --max-time=3600\ndirectory={app}\nuser={user}\nnumprocs={n}\nautostart=true\nautorestart=true\nstopasgroup=true\nkillasgroup=true\nstopwaitsecs=3600\nredirect_stderr=true\nstdout_logfile={shared}/storage/logs/worker-{name}.log\nstdout_logfile_maxbytes=10MB\nstdout_logfile_backups=3\n",
            name = w.name,
            q = w.queue,
            user = p.run_as,
            n = w.processes,
            shared = p.shared_dir(),
        ));
    }
    s
}

pub fn plan_worker(env: &ChangeEnv, p: &Project, w: &WorkerParams) -> Result<ChangePlan, String> {
    if p.kind != "laravel" {
        return Err("queue workers are for Laravel projects".into());
    }
    let workers: Vec<WorkerSpec> = w.workers.clone().unwrap_or_else(|| {
        vec![WorkerSpec {
            name: "default".into(),
            queue: w.queue.clone(),
            processes: w.processes,
        }]
    });
    if w.enabled && workers.is_empty() {
        return Err("add at least one worker".into());
    }
    if workers.len() > 8 {
        return Err("at most 8 workers".into());
    }
    let mut seen = std::collections::HashSet::new();
    for x in &workers {
        if !(1..=8).contains(&x.processes) {
            return Err("processes must be 1–8".into());
        }
        if !valid_queue_list(&x.queue) {
            return Err("queue names: letters, digits, _ and -, comma separated".into());
        }
        if x.name.is_empty()
            || x.name.len() > 30
            || !x
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !seen.insert(x.name.clone())
        {
            return Err(format!("worker name {:?}: unique, lowercase letters, digits and -", x.name));
        }
    }
    let name = sup_name(&p.id);
    let path = format!("/etc/supervisor/conf.d/nexacloud-{name}.conf");
    let mut b = ChangePlan::builder(
        "project_worker",
        &format!(
            "{} queue workers for {}",
            if w.enabled { "Configure" } else { "Remove" },
            p.id
        ),
        &format!("site:{}", p.site_id),
    )
    .lock(&format!("site:{}", p.site_id));
    if w.enabled && !env.host_path("/usr/bin/supervisord").exists() {
        b = b
            .lock("apt")
            .pre_command_timeout(["apt-get", "update", "-q"], 600)
            .pre_command_timeout(["apt-get", "install", "-y", "-q", "supervisor"], 900)
            .pre_command(["systemctl", "enable", "--now", "supervisor"]);
    } else if w.enabled {
        b = b.pre_command(["systemctl", "enable", "--now", "supervisor"]);
    }
    if w.enabled {
        b = b.write_file(&path, worker_conf(p, &workers, &crate::deploys::php_bin(&p.site_id)));
    } else {
        b = b.delete_file(&path);
    }
    b.command(["supervisorctl", "reread"])
        .command(["supervisorctl", "update"])
        .on_revert(["supervisorctl", "update"])
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

/// Keep a folder (e.g. `uploads`) between releases: create it in shared/
/// (copying what the live release has) so every deploy links it.
pub fn plan_shared(env: &ChangeEnv, p: &Project, name: &str) -> Result<ChangePlan, String> {
    if p.strategy != "releases" {
        return Err("in-place projects keep their files between deploys already".into());
    }
    if name.is_empty()
        || name.len() > 60
        || name.starts_with('.')
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    {
        return Err("folder name: letters, digits, _ - .".into());
    }
    if env.host_path(&format!("{}/{name}", p.shared_dir())).exists() {
        return Err(format!("{name}/ is already kept between releases"));
    }
    let script = r#"set -e
mkdir -p "$1/shared/$2"
if [ -d "$1/current/$2" ] && [ ! -L "$1/current/$2" ]; then cp -a "$1/current/$2/." "$1/shared/$2/"; fi
echo "shared/$2 ready — linked into every new release from the next deploy"
"#;
    ChangePlan::builder("project_shared", &format!("Keep {name}/ between releases of {}", p.id), &format!("site:{}", p.site_id))
        .lock(&format!("site:{}", p.site_id))
        .command(["runuser", "-u", &p.run_as, "--", "bash", "-c", script, "shared", &p.dir, name])
        .note(format!("Creates {}/shared/{name} (with the files the live release has). From the next deploy, every release links it, so uploads survive deploys and rollbacks.", p.dir))
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

/// "Fix permissions": the PHP user owns the project, storage/ writable.
pub fn plan_permissions(env: &ChangeEnv, p: &Project) -> Result<ChangePlan, String> {
    let args = crate::deploys::permissions::script_args(p);
    let web = &args[5];
    let mut p2 = p.clone();
    p2.run_as = web.clone();
    let b = ChangePlan::builder("project_permissions", &format!("Fix file permissions of {}", p.id), &format!("site:{}", p.site_id))
        .lock(&format!("site:{}", p.site_id))
        .command_timeout(args.clone(), 900);
    // Workers and cron follow the owner.
    let b = follow_project(b, env, &p2);
    b.verify_command([
        "sh",
        "-c",
        "for i in 1 2 3 4 5 6; do code=$(curl -ks -o /dev/null -w '%{http_code}' --resolve \"$1:80:127.0.0.1\" --resolve \"$1:443:127.0.0.1\" -L \"http://$1/\"); if [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ]; then echo \"HTTP $code\"; exit 0; fi; sleep 1; done; echo \"HTTP $code\"; exit 1",
        "probe",
        &p.site_id,
    ])
    .note(format!("Makes {web} (the user PHP runs as) the owner of {}, makes storage/, bootstrap/cache and upload folders writable, and sets .env to owner-only. Deploys, Artisan, workers and cron run as {web} from now on.", p.dir))
    .build(env)
    .map_err(|e| format!("{e:#}"))
}

pub fn cron_content(p: &Project) -> String {
    format!(
        "# Managed by NexaCloud — Laravel scheduler for {}\nSHELL=/bin/sh\n* * * * * {} cd {} && touch storage/framework/.nexacloud-schedule 2>/dev/null; {} artisan schedule:run >> storage/logs/nexacloud-schedule.log 2>&1\n",
        p.id, p.run_as, p.app_dir(), crate::deploys::php_bin(&p.site_id)
    )
}

/// Rewrite the project's Supervisor workers and cron file (if present) for
/// a changed user or app folder.
pub fn follow_project(
    mut b: crate::changes::ChangePlanBuilder,
    env: &ChangeEnv,
    p: &Project,
) -> crate::changes::ChangePlanBuilder {
    let wpath = crate::monitor::project::worker_conf_path(&p.id);
    if env.host_path(&wpath).exists() {
        let specs: Vec<WorkerSpec> = crate::monitor::project::worker_defs(&p.id)
            .into_iter()
            .map(|w| WorkerSpec { name: w.name, queue: w.queue, processes: w.processes })
            .collect();
        b = b
            .write_file(&wpath, worker_conf(p, &specs, &crate::deploys::php_bin(&p.site_id)))
            .command(["supervisorctl", "reread"])
            .command(["supervisorctl", "update"])
            .on_revert(["supervisorctl", "update"]);
    }
    let cpath = format!("/etc/cron.d/{}", cron_name(&p.id));
    if env.host_path(&cpath).exists() {
        b = b.write_file_with(&cpath, crate::changes::Content::Text(cron_content(p)), Some(0o644), Some((0, 0)));
    }
    b
}

/// Move an in-place project to release folders (`releases/1` = a copy of the
/// current code, `.env` and `storage/` copied to `shared/`, nginx root →
/// `current/…`). The old files stay where they are.
pub fn plan_releases(env: &ChangeEnv, p: &Project) -> Result<ChangePlan, String> {
    if p.strategy == "releases" {
        return Err("this project already uses release folders".into());
    }
    let old = crate::plans::sites::load_model(env, &p.site_id)
        .map_err(|_| "only sites managed by NexaCloud can switch (use “Manage with NexaCloud” on the Sites page first)".to_string())?;
    if old.runtime == crate::plans::sites::Runtime::Frankenphp {
        return Err("switch the site back to PHP-FPM first".into());
    }
    let root = old.root.clone().ok_or("this site has no web root")?;
    let rel_root = root
        .strip_prefix(&p.dir)
        .ok_or_else(|| format!("the web root {root} is not inside {}", p.dir))?;
    let mut new = old.clone();
    new.root = Some(format!("{}/current{rel_root}", p.dir));
    let mut p2 = p.clone();
    p2.strategy = "releases".into();
    let script = r#"set -e
cd "$1"
mkdir -p releases/1 shared
tar --exclude=./releases --exclude=./shared --exclude=./current --exclude=./.nexacloud-repo -cf - . | tar -xf - -C releases/1
if [ -e .env ]; then cp -a .env shared/.env; fi
if [ -d storage ]; then cp -a storage shared/storage; fi
if [ -d .git ]; then mkdir -p .nexacloud-repo && cp -a .git .nexacloud-repo/.git; fi
c="$(git rev-parse HEAD 2>/dev/null || true)"
cd releases/1
rm -rf .git
for f in ../../shared/* ../../shared/.[!.]*; do
  [ -e "$f" ] || continue
  n="$(basename "$f")"; rm -rf "./$n"; ln -s "../../shared/$n" "./$n"
done
printf '{"commit":"%s","message":"converted from in-place","author":"","at":"%s"}' "$c" "$(date -u +%FT%TZ)" > .nexacloud-release
cd "$1"
ln -sfn releases/1 current
"#;
    let b = ChangePlan::builder(
        "project_releases",
        &format!("Use zero-downtime release folders for {}", p.id),
        &format!("site:{}", p.site_id),
    )
    .lock(&format!("site:{}", p.site_id))
    .pre_command_timeout(["runuser", "-u", &p.run_as, "--", "bash", "-c", script, "convert", &p.dir], 1800)
    .write_file(&crate::plans::sites::conf_path(&p.site_id), crate::plans::sites::render(&new))
    .on_revert(["rm", "-f", &format!("{}/current", p.dir)]);
    let b = follow_project(b, env, &p2);
    crate::plans::sites::apply_and_probe(b, &new)
        .note(format!(
            "Copies the current code to {dir}/releases/1 and .env + storage/ to {dir}/shared, then serves {dir}/current. Your files in {dir} are not touched or deleted. Needs free disk space for one copy of the project.",
            dir = p.dir
        ))
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

pub fn plan_scheduler(env: &ChangeEnv, p: &Project, enabled: bool) -> Result<ChangePlan, String> {
    if p.kind != "laravel" {
        return Err("the scheduler is for Laravel projects".into());
    }
    let path = format!("/etc/cron.d/{}", cron_name(&p.id));
    let b = ChangePlan::builder(
        "project_scheduler",
        &format!(
            "{} the scheduler for {}",
            if enabled { "Enable" } else { "Disable" },
            p.id
        ),
        &format!("site:{}", p.site_id),
    );
    let mut b = b;
    if enabled && !env.host_path("/usr/sbin/cron").exists() {
        b = b
            .lock("apt")
            .pre_command_timeout(["apt-get", "update", "-q"], 600)
            .pre_command_timeout(["apt-get", "install", "-y", "-q", "cron"], 900);
    }
    if enabled {
        b = b.pre_command(["systemctl", "enable", "--now", "cron"]);
    }
    let b = if enabled {
        b.write_file_with(
            &path,
            crate::changes::Content::Text(cron_content(p)),
            Some(0o644),
            Some((0, 0)),
        )
    } else {
        b.delete_file(&path)
    };
    b.note("cron picks this up automatically (every minute).")
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_quoting_and_keys() {
        assert_eq!(quote_env("simple-value"), "simple-value");
        assert_eq!(quote_env("has space"), "\"has space\"");
        assert_eq!(quote_env("a\"b"), "\"a\\\"b\"");
        assert!(valid_env_key("APP_DEBUG"));
        assert!(!valid_env_key("app_debug"));
        assert!(!valid_env_key("A B"));
    }
}
