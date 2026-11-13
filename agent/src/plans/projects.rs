//! Project plans: .env editor, queue workers (Supervisor) and the Laravel
//! scheduler (cron). All through the change pipeline.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::changes::{ChangeEnv, ChangePlan};
use crate::deploys::routes::{cron_name, sup_name};
use crate::deploys::Project;

fn valid_env_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 80
        && k.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !k.as_bytes()[0].is_ascii_digit()
}

fn quote_env(v: &str) -> String {
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
    let path = format!("{}/.env", p.dir);
    let current = std::fs::read_to_string(env.host_path(&path)).unwrap_or_default();
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
    .write_file_with(&path, crate::changes::Content::Text(out), Some(0o640), None);
    if p.kind == "laravel" {
        b = b.command([
            "runuser",
            "-u",
            &p.run_as,
            "--",
            "php",
            &format!("{}/artisan", p.dir),
            "config:clear",
        ]);
    }
    b.note("Values are written to the project's .env; Laravel's config cache is cleared so they take effect.")
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
}
fn two() -> u32 {
    2
}
fn default_q() -> String {
    "default".into()
}

pub fn plan_worker(env: &ChangeEnv, p: &Project, w: &WorkerParams) -> Result<ChangePlan, String> {
    if p.kind != "laravel" {
        return Err("queue workers are for Laravel projects".into());
    }
    if !(1..=8).contains(&w.processes) {
        return Err("processes must be 1–8".into());
    }
    if !w.queue.split(',').all(|q| {
        !q.is_empty()
            && q.len() <= 40
            && q.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    }) {
        return Err("queue names: letters, digits, _ and -, comma separated".into());
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
    }
    if w.enabled {
        let conf = format!(
            "; Managed by NexaCloud — queue workers for {id}\n[program:nexacloud-{name}]\nprocess_name=%(program_name)s_%(process_num)02d\ncommand=php {dir}/artisan queue:work --queue={q} --sleep=3 --tries=3 --max-time=3600\ndirectory={dir}\nuser={user}\nnumprocs={n}\nautostart=true\nautorestart=true\nstopasgroup=true\nkillasgroup=true\nstopwaitsecs=3600\nredirect_stderr=true\nstdout_logfile={dir}/storage/logs/worker.log\nstdout_logfile_maxbytes=10MB\nstdout_logfile_backups=3\n",
            id = p.id, dir = p.dir, q = w.queue, user = p.run_as, n = w.processes
        );
        b = b.write_file(&path, conf);
    } else {
        b = b.delete_file(&path);
    }
    b.command(["supervisorctl", "reread"])
        .command(["supervisorctl", "update"])
        .on_revert(["supervisorctl", "update"])
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
    let b = if enabled {
        b.write_file_with(
            &path,
            crate::changes::Content::Text(format!(
                "# Managed by NexaCloud — Laravel scheduler for {}\nSHELL=/bin/sh\n* * * * * {} cd {} && php artisan schedule:run >> /dev/null 2>&1\n",
                p.id, p.run_as, p.dir
            )),
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
