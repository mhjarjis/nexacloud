//! Health of one project for its page.
//!
//! Cheap facts (certificate, maintenance flag, workers, log errors, uptime
//! samples) are read on request. Laravel internals — framework version, queue
//! sizes, failed jobs, the schedule — come from a small PHP snippet that boots
//! the app as the site user; that result is cached for two minutes and
//! refreshed by the monitor loop, so page polls never shell out to PHP.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::deploys::{list_projects, routes::sup_name, Project};
use crate::state::AppState;
use crate::util::cmd::Cmd;

const TTL: Duration = Duration::from_secs(120);

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> = LazyLock::new(Default::default);
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Boots the Laravel app and prints `NCJSON:{...}`. Runs as the site user.
const SNIPPET: &str = r#"error_reporting(0);
$dir = $argv[1]; chdir($dir);
$out = ['laravel' => null, 'queue' => null, 'db' => null, 'queues' => [], 'schedule' => [], 'failed_24h' => null, 'failed_by_queue' => (object)[], 'php' => PHP_VERSION, 'env' => null, 'debug' => null];
try {
  require $dir.'/vendor/autoload.php';
  $app = require $dir.'/bootstrap/app.php';
  $app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();
  $out['laravel'] = $app->version();
  $out['env'] = $app->environment();
  $out['debug'] = (bool) config('app.debug');
  $out['queue'] = config('queue.default');
  $c = config('database.default');
  $out['db'] = ['connection' => $c, 'database' => config("database.connections.$c.database")];
  foreach ((json_decode($argv[2], true) ?: ['default']) as $q) {
    try { $out['queues'][$q] = \Illuminate\Support\Facades\Queue::size($q); } catch (\Throwable $e) { $out['queues'][$q] = null; }
  }
  try {
    foreach ($app->make(\Illuminate\Console\Scheduling\Schedule::class)->events() as $e) {
      $cmd = $e->command ? preg_replace("/^.*artisan'?\s+/", '', $e->command) : ($e->description ?: 'Closure');
      $out['schedule'][] = ['command' => $cmd, 'expression' => $e->expression, 'description' => $e->description, 'next' => $e->nextRunDate()->format(DATE_ATOM)];
    }
  } catch (\Throwable $e) {}
  try {
    $t = config('queue.failed.table', 'failed_jobs');
    if (\Illuminate\Support\Facades\Schema::hasTable($t)) {
      $since = now()->subDay();
      $out['failed_24h'] = \Illuminate\Support\Facades\DB::table($t)->where('failed_at', '>=', $since)->count();
      $out['failed_by_queue'] = (object) \Illuminate\Support\Facades\DB::table($t)->where('failed_at', '>=', $since)->selectRaw('queue, count(*) as c')->groupBy('queue')->pluck('c', 'queue')->all();
    }
  } catch (\Throwable $e) {}
} catch (\Throwable $e) { $out['error'] = get_class($e).': '.substr($e->getMessage(), 0, 300); }
echo "\nNCJSON:".json_encode($out);
"#;

// ------------------------------------------------------------------ workers

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkerDef {
    pub name: String,
    pub queue: String,
    pub processes: u32,
}

pub fn worker_conf_path(id: &str) -> String {
    format!("/etc/supervisor/conf.d/nexacloud-{}.conf", sup_name(id))
}

/// Workers defined in the project's Supervisor file.
pub fn parse_worker_conf(id: &str, text: &str) -> Vec<WorkerDef> {
    let base = format!("nexacloud-{}", sup_name(id));
    let mut out: Vec<WorkerDef> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(prog) = l
            .strip_prefix("[program:")
            .and_then(|r| r.strip_suffix(']'))
        {
            let name = prog
                .strip_prefix(&format!("{base}-"))
                .unwrap_or(if prog == base { "default" } else { prog });
            out.push(WorkerDef {
                name: name.to_string(),
                queue: "default".into(),
                processes: 1,
            });
        } else if let Some(w) = out.last_mut() {
            if let Some(n) = l.strip_prefix("numprocs=") {
                w.processes = n.trim().parse().unwrap_or(1);
            } else if l.starts_with("command=") {
                if let Some(q) = l.split_whitespace().find_map(|a| a.strip_prefix("--queue=")) {
                    w.queue = q.to_string();
                }
            }
        }
    }
    out
}

pub fn worker_defs(id: &str) -> Vec<WorkerDef> {
    std::fs::read_to_string(worker_conf_path(id))
        .map(|t| parse_worker_conf(id, &t))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProcStatus {
    /// `group:process`
    pub name: String,
    pub program: String,
    pub state: String,
    pub uptime: Option<String>,
}

/// `supervisorctl status` lines for this project.
pub fn parse_supervisor_status(text: &str) -> Vec<ProcStatus> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?.to_string();
            let state = it.next()?.to_string();
            let rest: Vec<&str> = it.collect();
            let rest = rest.join(" ");
            let uptime = rest
                .split_once("uptime ")
                .map(|(_, u)| u.trim().to_string());
            let program = name
                .split_once(':')
                .map(|(_, p)| p)
                .unwrap_or(&name)
                .rsplit_once('_')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| name.clone());
            Some(ProcStatus {
                name,
                program,
                state,
                uptime,
            })
        })
        .collect()
}

pub async fn supervisor_status(id: &str) -> Vec<ProcStatus> {
    let o = Cmd::new("supervisorctl")
        .args(["status", &format!("nexacloud-{}:*", sup_name(id))])
        .timeout(Duration::from_secs(10))
        .run()
        .await;
    parse_supervisor_status(&o.stdout)
}

// ------------------------------------------------------------------ errors

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ErrGroup {
    pub level: String,
    pub when: String,
    pub count: usize,
    pub msg: String,
}

const LEVELS: [&str; 5] = ["EMERGENCY", "ALERT", "CRITICAL", "ERROR", "WARNING"];

fn push_group(groups: &mut Vec<ErrGroup>, level: &str, when: &str, msg: &str) {
    let msg = crate::util::cmd::truncate(msg.trim(), 300);
    if let Some(g) = groups.iter_mut().find(|g| g.msg == msg) {
        g.count += 1;
        g.when = when.to_string();
        g.level = level.to_string();
    } else {
        groups.push(ErrGroup {
            level: level.to_string(),
            when: when.to_string(),
            count: 1,
            msg,
        });
    }
}

/// Group `laravel.log` entries by message. Returns (groups newest first,
/// entries at or after `since` — "YYYY-MM-DD HH:MM:SS").
pub fn laravel_errors(text: &str, since: &str) -> (Vec<ErrGroup>, usize) {
    let mut groups = Vec::new();
    let mut recent = 0;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some((ts, rest)) = rest.split_once("] ") else {
            continue;
        };
        if ts.len() < 19 {
            continue;
        }
        let Some((chan, msg)) = rest.split_once(": ") else {
            continue;
        };
        let Some(level) = chan.rsplit('.').next().filter(|l| LEVELS.contains(l)) else {
            continue;
        };
        let msg = msg
            .split(" {\"")
            .next()
            .unwrap_or(msg)
            .split(" [stacktrace]")
            .next()
            .unwrap_or(msg);
        let ts = &ts[..19];
        if ts >= since {
            recent += 1;
        }
        push_group(&mut groups, level, ts, msg);
    }
    groups.sort_by(|a, b| b.when.cmp(&a.when));
    groups.truncate(6);
    (groups, recent)
}

/// nginx error.log lines for one server name (PHP warnings of plain sites).
pub fn nginx_errors(text: &str, server: &str, since: &str) -> (Vec<ErrGroup>, usize) {
    let mut groups = Vec::new();
    let mut recent = 0;
    let needle = format!("server: {server},");
    for line in text.lines().filter(|l| l.contains(&needle)) {
        if line.len() < 20 {
            continue;
        }
        let ts = line[..19].replace('/', "-");
        let level = if line.contains("[warn]") {
            "WARNING"
        } else {
            "ERROR"
        };
        let msg = if let Some((_, m)) = line.split_once("PHP message: ") {
            m.split("\" while ")
                .next()
                .unwrap_or(m)
                .split("; PHP message:")
                .next()
                .unwrap_or(m)
                .trim_start_matches("PHP ")
        } else {
            line.split_once(": *")
                .and_then(|(_, r)| r.split_once(' '))
                .map(|(_, r)| r)
                .unwrap_or(line)
                .split(", client:")
                .next()
                .unwrap_or(line)
        };
        let level = if msg.starts_with("Deprecated") {
            "DEPRECATED"
        } else if msg.starts_with("Warning") || msg.starts_with("Notice") {
            "WARNING"
        } else {
            level
        };
        if ts.as_str() >= since {
            recent += 1;
        }
        push_group(&mut groups, level, &ts, msg);
    }
    groups.sort_by(|a, b| b.when.cmp(&a.when));
    groups.truncate(6);
    (groups, recent)
}

/// Newest `laravel*.log` in storage/logs.
pub fn newest_laravel_log(p: &Project) -> Option<std::path::PathBuf> {
    let dir = Path::new(&p.shared_dir()).join("storage/logs");
    let mut v: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("laravel") && n.ends_with(".log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    v.sort();
    v.pop().map(|(_, p)| p)
}

fn errors(p: &Project) -> Value {
    let since = (chrono::Utc::now() - chrono::Duration::hours(1))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let (file, (items, last_hour)) = if p.kind == "laravel" {
        match newest_laravel_log(p) {
            Some(f) => {
                let t = crate::jobs::read_log_tail(&f, 1_000_000);
                (
                    f.strip_prefix(p.shared_dir())
                        .map(|r| r.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| f.to_string_lossy().into_owned()),
                    laravel_errors(&t, &since),
                )
            }
            None => ("storage/logs/laravel.log".into(), (Vec::new(), 0)),
        }
    } else {
        let t = crate::jobs::read_log_tail(Path::new("/var/log/nginx/error.log"), 1_000_000);
        ("/var/log/nginx/error.log".into(), nginx_errors(&t, &p.site_id, &since))
    };
    json!({"file": file, "items": items, "last_hour": last_hour})
}

// ------------------------------------------------------------------ laravel facts

async fn du_bytes(path: &str) -> Option<u64> {
    if !Path::new(path).exists() {
        return None;
    }
    let o = Cmd::new("du")
        .args(["-sb", path])
        .timeout(Duration::from_secs(20))
        .run()
        .await;
    o.stdout.split_whitespace().next()?.parse().ok()
}

async fn collect(p: &Project) -> Value {
    let app = p.app_dir();
    let mut facts = if p.kind != "laravel" {
        json!({})
    } else if !Path::new(&app).join("vendor/autoload.php").exists() {
        json!({"error": "not installed yet — deploy first (no vendor/ folder)"})
    } else {
        let queues: Vec<String> = {
            let mut q: Vec<String> = worker_defs(&p.id)
                .iter()
                .flat_map(|w| w.queue.split(',').map(String::from).collect::<Vec<_>>())
                .collect();
            if q.is_empty() {
                q.push("default".into());
            }
            q.sort();
            q.dedup();
            q
        };
        let php = crate::deploys::php_bin(&p.site_id);
        let o = Cmd::new("runuser")
            .args([
                "-u",
                &p.run_as,
                "--",
                &php,
                "-d",
                "memory_limit=256M",
                "-d",
                "display_errors=0",
                "-r",
                SNIPPET,
                "--",
                &app,
                &serde_json::to_string(&queues).unwrap_or_default(),
            ])
            .current_dir(&app)
            .timeout(Duration::from_secs(30))
            .run()
            .await;
        o.stdout
            .rsplit_once("NCJSON:")
            .and_then(|(_, j)| serde_json::from_str::<Value>(j.trim()).ok())
            .unwrap_or_else(|| json!({"error": o.error_summary("php")}))
    };
    let logs_dir = format!("{}/storage/logs", p.shared_dir());
    let (dir_bytes, logs_bytes) = tokio::join!(du_bytes(&p.dir), du_bytes(&logs_dir));
    facts["dir_bytes"] = json!(dir_bytes);
    facts["logs_bytes"] = json!(logs_bytes);
    facts["collected_at"] = json!(chrono::Utc::now().timestamp());
    facts
}

fn cached(id: &str) -> Option<(Instant, Value)> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
}

pub async fn refresh(p: &Project) -> Value {
    if !RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.id.clone())
    {
        return cached(&p.id).map(|c| c.1).unwrap_or(Value::Null);
    }
    let v = collect(p).await;
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.id.clone(), (Instant::now(), v.clone()));
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&p.id);
    v
}

pub fn invalidate(id: &str) {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

pub async fn refresh_all(state: &AppState) {
    let Ok(projects) = list_projects(&state.db) else {
        return;
    };
    for p in projects {
        if cached(&p.id).is_none_or(|(at, _)| at.elapsed() > TTL) {
            refresh(&p).await;
        }
    }
}

/// Facts for the page: from cache, or collected now when missing (`fresh`
/// forces it).
pub async fn facts(p: &Project, fresh: bool) -> Value {
    match cached(&p.id) {
        Some((at, v)) if !fresh && at.elapsed() < TTL * 3 => v,
        _ => refresh(p).await,
    }
}

// ------------------------------------------------------------------ overview

pub async fn overview(state: &AppState, p: &Project, fresh: bool) -> Value {
    let sites = crate::routes::sites::scan(false).await;
    let site = sites.iter().find(|s| s.id == p.site_id);
    let now = chrono::Utc::now();
    let ssl_days = site
        .and_then(|s| s.ssl_expires.as_deref())
        .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
        .map(|e| (e.with_timezone(&chrono::Utc) - now).num_days());
    let model = crate::plans::sites::load_model(&state.changes_env, &p.site_id).ok();
    let (facts, procs) = tokio::join!(facts(p, fresh), supervisor_status(&p.id));
    let defs = worker_defs(&p.id);
    let workers: Vec<Value> = defs
        .iter()
        .map(|w| {
            let prog = if w.name == "default" && procs.iter().any(|s| s.program == format!("nexacloud-{}", sup_name(&p.id))) {
                format!("nexacloud-{}", sup_name(&p.id))
            } else {
                format!("nexacloud-{}-{}", sup_name(&p.id), w.name)
            };
            let mine: Vec<&ProcStatus> = procs.iter().filter(|s| s.program == prog).collect();
            let running = mine.iter().filter(|s| s.state == "RUNNING").count();
            let state = if mine.is_empty() {
                "UNKNOWN".to_string()
            } else if running == mine.len() {
                "RUNNING".to_string()
            } else {
                mine.iter()
                    .find(|s| s.state != "RUNNING")
                    .map(|s| s.state.clone())
                    .unwrap_or_default()
            };
            let pending: Option<i64> = w
                .queue
                .split(',')
                .map(|q| facts["queues"][q].as_i64())
                .sum();
            let failed: Option<i64> = if facts["failed_24h"].is_null() {
                None
            } else {
                Some(
                    w.queue
                        .split(',')
                        .map(|q| facts["failed_by_queue"][q].as_i64().unwrap_or(0))
                        .sum(),
                )
            };
            json!({
                "name": w.name, "queue": w.queue, "processes": w.processes, "running": running,
                "state": state, "uptime": mine.first().and_then(|s| s.uptime.clone()),
                "pending": pending, "failed_24h": failed,
            })
        })
        .collect();
    let shared = p.shared_dir();
    let sched_mark = Path::new(&shared).join("storage/framework/.nexacloud-schedule");
    let sched_last = std::fs::metadata(&sched_mark)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp());
    let scheduler = Path::new(&format!(
        "/etc/cron.d/{}",
        crate::deploys::routes::cron_name(&p.id)
    ))
    .exists();
    let maintenance = Path::new(&p.app_dir())
        .join("storage/framework/down")
        .exists()
        || Path::new(&shared).join("storage/framework/down").exists();
    json!({
        "site": {
            "id": p.site_id,
            "url": format!("{}://{}", if site.is_some_and(|s| s.ssl) { "https" } else { "http" }, p.site_id),
            "ssl": site.is_some_and(|s| s.ssl),
            "ssl_days": ssl_days,
            "php_version": site.and_then(|s| s.php_version.clone()).or_else(|| model.as_ref().and_then(|m| m.php_version.clone())),
            "runtime": model.as_ref().map(|m| m.runtime.clone()),
            "managed": model.is_some(),
        },
        "facts": facts,
        "workers": workers,
        "scheduler": {"enabled": scheduler, "last_run": sched_last},
        "maintenance": maintenance,
        "errors": errors(p),
        "monitor": super::stats(&state.db, &p.site_id, 24),
        "app_dir": p.app_dir(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worker_conf_old_and_new() {
        let old = "[program:nexacloud-crm-test]\nprocess_name=%(program_name)s_%(process_num)02d\ncommand=php8.3 /var/www/crm/artisan queue:work --queue=default,mail --sleep=3\nnumprocs=2\n";
        let w = parse_worker_conf("crm.test", old);
        assert_eq!(w, vec![WorkerDef { name: "default".into(), queue: "default,mail".into(), processes: 2 }]);
        let new = "[group:nexacloud-crm-test]\nprograms=nexacloud-crm-test-default,nexacloud-crm-test-mail\n[program:nexacloud-crm-test-default]\ncommand=php a queue:work --queue=default\nnumprocs=2\n[program:nexacloud-crm-test-mail]\ncommand=php a queue:work --queue=mail\nnumprocs=1\n";
        let w = parse_worker_conf("crm.test", new);
        assert_eq!(w.len(), 2);
        assert_eq!(w[1].name, "mail");
        assert_eq!(w[1].queue, "mail");
        assert_eq!(w[0].processes, 2);
    }

    #[test]
    fn parses_supervisor_status() {
        let t = "nexacloud-crm-test:nexacloud-crm-test-mail_00   RUNNING   pid 12, uptime 6 days, 4:01:02\nnexacloud-crm-test:nexacloud-crm-test-mail_01   FATAL     Exited too quickly\n";
        let s = parse_supervisor_status(t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].program, "nexacloud-crm-test-mail");
        assert_eq!(s[0].uptime.as_deref(), Some("6 days, 4:01:02"));
        assert_eq!(s[1].state, "FATAL");
    }

    #[test]
    fn groups_laravel_errors() {
        let log = "[2026-11-12 14:02:11] production.ERROR: Connection timed out {\"job\":\"X\"}\n#0 stack\n[2026-11-12 14:03:11] production.ERROR: Connection timed out {\"job\":\"Y\"}\n[2026-11-12 14:04:00] production.INFO: hello\n[2026-11-12 11:20:00] production.WARNING: Slow query\n";
        let (g, recent) = laravel_errors(log, "2026-11-12 14:00:00");
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].msg, "Connection timed out");
        assert_eq!(g[0].count, 2);
        assert_eq!(g[0].when, "2026-11-12 14:03:11");
        assert_eq!(g[1].level, "WARNING");
        assert_eq!(recent, 2);
    }

    #[test]
    fn groups_nginx_php_errors() {
        let log = "2026/11/12 13:40:01 [error] 9#9: *5 FastCGI sent in stderr: \"PHP message: PHP Deprecated:  Creation of dynamic property in /x.php on line 4\" while reading response header from upstream, client: 1.2.3.4, server: portal.test, request: \"GET / HTTP/1.1\"\n2026/11/12 13:41:01 [error] 9#9: *6 something else, client: 1.2.3.4, server: other.test, request: \"GET /\"\n";
        let (g, _) = nginx_errors(log, "portal.test", "2026-11-12 13:00:00");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].level, "DEPRECATED");
        assert!(g[0].msg.starts_with("Deprecated:  Creation"));
    }
}
