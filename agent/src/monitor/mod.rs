//! Uptime monitoring and notifications.
//!
//! - Every `NEXACLOUD_MONITOR_INTERVAL` seconds (default 60) each site is
//!   requested over loopback (`curl --resolve host:port:127.0.0.1`, no
//!   redirects followed) and the status code + response time are stored in
//!   `monitor_samples` (kept 8 days).
//! - Three failed checks in a row (no answer or 5xx) send a "site down"
//!   notification; the first good check afterwards sends "recovered".
//! - Hourly, the dashboard's own warnings (SSL expiry, disk, services down,
//!   end-of-life PHP…) are sent once a day each while they persist.
//! - Deploy and backup failures notify from their own jobs.

pub mod notify;
pub mod project;
pub mod routes;

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::db::Db;
use crate::state::AppState;
use crate::util::cmd::Cmd;

const FAILS_FOR_DOWN: u32 = 3;
const KEEP_SECS: i64 = 8 * 86400;

static FAILS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);
static DOWN: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn interval_secs() -> u64 {
    std::env::var("NEXACLOUD_MONITOR_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u64| *v >= 2)
        .unwrap_or(60)
}

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub ssl: bool,
}

/// Sites worth probing: real hostnames serving something (not redirects or
/// catch-all blocks).
pub async fn targets() -> Vec<Target> {
    crate::routes::sites::scan(false)
        .await
        .into_iter()
        .filter(|s| {
            s.id != "_"
                && s.id != "localhost"
                && s.id.contains('.')
                && (s.root.is_some() || s.proxy_pass.is_some())
        })
        .map(|s| Target {
            id: s.id,
            ssl: s.ssl,
        })
        .collect()
}

/// One request: `(http_code, milliseconds)`; code 0 = no answer.
pub async fn probe(t: &Target) -> (i64, i64) {
    let (scheme, port) = if t.ssl {
        ("https", "443")
    } else {
        ("http", "80")
    };
    let out = Cmd::new("curl")
        .args([
            "-ks",
            "-o",
            "/dev/null",
            "--max-time",
            "10",
            "-w",
            "%{http_code} %{time_total}",
            "--resolve",
            &format!("{}:{port}:127.0.0.1", t.id),
            &format!("{scheme}://{}/", t.id),
        ])
        .timeout(Duration::from_secs(15))
        .run()
        .await;
    let mut it = out.stdout.split_whitespace();
    let code = it.next().and_then(|c| c.parse().ok()).unwrap_or(0);
    let secs: f64 = it.next().and_then(|c| c.parse().ok()).unwrap_or(0.0);
    (code, (secs * 1000.0).round() as i64)
}

pub fn is_failure(code: i64) -> bool {
    code == 0 || code >= 500
}

fn record(db: &Db, site: &str, ts: i64, code: i64, ms: i64) {
    let _ = db.conn().execute(
        "INSERT INTO monitor_samples (site_id, ts, code, ms) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![site, ts, code, ms],
    );
}

async fn probe_all(state: &AppState) {
    let ts = chrono::Utc::now().timestamp();
    let targets = targets().await;
    let futs = targets
        .into_iter()
        .map(|t| async move { (t.id.clone(), probe(&t).await) })
        .collect();
    for (site, (code, ms)) in crate::util::join_all(futs).await.into_iter().flatten() {
        record(&state.db, &site, ts, code, ms);
        if code == 503 && in_maintenance(state, &site) {
            // On purpose: not an outage.
            continue;
        }
        let fails = {
            let mut f = FAILS.lock().unwrap_or_else(|e| e.into_inner());
            let n = f.entry(site.clone()).or_default();
            if is_failure(code) {
                *n += 1;
            } else {
                *n = 0;
            }
            *n
        };
        let was_down = DOWN.lock().unwrap_or_else(|e| e.into_inner()).contains(&site);
        if fails >= FAILS_FOR_DOWN && !was_down {
            DOWN.lock().unwrap_or_else(|e| e.into_inner()).insert(site.clone());
            let what = if code == 0 {
                "does not answer".to_string()
            } else {
                format!("returns HTTP {code}")
            };
            notify::send(
                state,
                notify::Event::SiteDown,
                &format!("site_down:{site}"),
                "critical",
                &format!("{site} is down"),
                &format!("{site} {what} ({fails} checks in a row)."),
                None,
            );
        } else if fails == 0 && was_down {
            DOWN.lock().unwrap_or_else(|e| e.into_inner()).remove(&site);
            notify::send(
                state,
                notify::Event::SiteDown,
                &format!("site_up:{site}"),
                "info",
                &format!("{site} is back up"),
                &format!("{site} answers again (HTTP {code}, {ms} ms)."),
                None,
            );
        }
    }
}

/// Maintenance mode is on (nginx option, or `artisan down`).
fn in_maintenance(state: &AppState, site: &str) -> bool {
    if crate::plans::sites::load_model(&state.changes_env, site).is_ok_and(|m| m.options.maintenance) {
        return true;
    }
    crate::deploys::get_project(&state.db, site)
        .ok()
        .flatten()
        .is_some_and(|p| std::path::Path::new(&p.app_dir()).join("storage/framework/down").exists())
}

fn prune(db: &Db) {
    let cutoff = chrono::Utc::now().timestamp() - KEEP_SECS;
    let _ = db
        .conn()
        .execute("DELETE FROM monitor_samples WHERE ts < ?1", [cutoff]);
    let _ = db
        .conn()
        .execute("DELETE FROM server_samples WHERE ts < ?1", [cutoff - 32 * 86400 + KEEP_SECS]);
    let _ = db.conn().execute(
        "DELETE FROM notifications WHERE at < ?1",
        [crate::util::fmt_ts(chrono::Utc::now() - chrono::Duration::days(30))],
    );
}

/// Dashboard warnings → notifications (deduplicated per day inside `send`).
async fn server_checks(state: &AppState) {
    let alerts = crate::routes::current_alerts().await;
    for a in alerts {
        let level = match a.level {
            crate::alerts::Level::Critical => "critical",
            crate::alerts::Level::Warning => "warning",
            crate::alerts::Level::Info => continue,
        };
        notify::send(
            state,
            notify::Event::Server,
            &format!("server:{}", a.title),
            level,
            &a.title,
            &a.detail,
            Some(Duration::from_secs(86400)),
        );
    }
}

// ------------------------------------------------------------------ server history

/// (rx, tx) bytes of all non-loopback interfaces since boot.
pub fn net_counters() -> (i64, i64) {
    let t = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let mut rx = 0i64;
    let mut tx = 0i64;
    for l in t.lines().skip(2) {
        let Some((name, rest)) = l.split_once(':') else { continue };
        let name = name.trim();
        if name == "lo" || name.starts_with("docker") || name.starts_with("veth") || name.starts_with("br-") {
            continue;
        }
        let f: Vec<i64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if f.len() >= 9 {
            rx += f[0];
            tx += f[8];
        }
    }
    (rx, tx)
}

fn mem_percent() -> f64 {
    let t = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |k: &str| {
        t.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    let total = get("MemTotal:");
    let avail = get("MemAvailable:");
    if total > 0.0 { (total - avail) / total * 100.0 } else { 0.0 }
}

static LAST_SERVER_SAMPLE: LazyLock<Mutex<i64>> = LazyLock::new(Default::default);

fn record_server(state: &AppState) {
    let now = chrono::Utc::now().timestamp();
    {
        let mut last = LAST_SERVER_SAMPLE.lock().unwrap_or_else(|e| e.into_inner());
        if now - *last < 55 {
            return;
        }
        *last = now;
    }
    let cpu = state.cpu.get().0 as f64;
    let (rx, tx) = net_counters();
    let _ = state.db.conn().execute(
        "INSERT INTO server_samples (ts, cpu, mem, rx, tx) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![now, cpu, mem_percent(), rx, tx],
    );
}

/// Bytes transferred between samples, ignoring counter resets (reboots).
pub fn traffic(samples: &[(i64, i64, i64)]) -> (i64, i64) {
    let (mut rx, mut tx) = (0, 0);
    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b.1 >= a.1 { rx += b.1 - a.1 }
        if b.2 >= a.2 { tx += b.2 - a.2 }
    }
    (rx, tx)
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let every = interval_secs();
        let hourly = (3600 / every).max(1);
        let mut tick: u64 = 0;
        let mut interval = tokio::time::interval(Duration::from_secs(every));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            record_server(&state);
            probe_all(&state).await;
            if tick % hourly == 1 {
                prune(&state.db);
                server_checks(&state).await;
            }
            project::refresh_all(&state).await;
            tick += 1;
        }
    });
}

// ------------------------------------------------------------------ stats

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Bucket {
    /// Start of the hour (unix seconds).
    pub ts: i64,
    pub p50: Option<i64>,
    pub p95: Option<i64>,
    pub checks: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stats {
    pub site: String,
    pub checks: usize,
    /// Share of good checks in the window (None: no data yet).
    pub uptime: Option<f64>,
    pub last_code: Option<i64>,
    pub last_ms: Option<i64>,
    pub last_at: Option<i64>,
    /// Failed checks in the last hour.
    pub failures_1h: usize,
    pub checks_1h: usize,
    pub buckets: Vec<Bucket>,
    /// Last 2 h in 5-minute buckets.
    pub recent: Vec<Bucket>,
    pub interval_secs: u64,
}

fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(idx).copied()
}

fn bucketize(samples: &[(i64, i64, i64)], first: i64, step: i64, n: i64) -> Vec<Bucket> {
    let mut buckets = Vec::new();
    for h in 0..n {
        let from = first + h * step;
        let to = from + step;
        let mut ms: Vec<i64> = samples
            .iter()
            .filter(|s| s.0 >= from && s.0 < to && !is_failure(s.1))
            .map(|s| s.2)
            .collect();
        ms.sort_unstable();
        let in_hour = samples.iter().filter(|s| s.0 >= from && s.0 < to);
        let checks = in_hour.clone().count();
        let failures = in_hour.filter(|s| is_failure(s.1)).count();
        buckets.push(Bucket {
            ts: from,
            p50: percentile(&ms, 0.5),
            p95: percentile(&ms, 0.95),
            checks,
            failures,
        });
    }
    buckets
}

pub fn compute_stats(site: &str, now: i64, hours: i64, samples: &[(i64, i64, i64)]) -> Stats {
    let buckets = bucketize(samples, (now / 3600 - hours + 1) * 3600, 3600, hours);
    // Last two hours in 5-minute steps (a new server has no hourly history yet).
    let recent = bucketize(samples, (now / 300 - 23) * 300, 300, 24);
    let good = samples.iter().filter(|s| !is_failure(s.1)).count();
    let last = samples.iter().max_by_key(|s| s.0);
    let last_hour = samples.iter().filter(|s| s.0 >= now - 3600);
    Stats {
        site: site.to_string(),
        checks: samples.len(),
        uptime: (!samples.is_empty()).then(|| good as f64 / samples.len() as f64),
        last_code: last.map(|s| s.1),
        last_ms: last.map(|s| s.2),
        last_at: last.map(|s| s.0),
        failures_1h: last_hour.clone().filter(|s| is_failure(s.1)).count(),
        checks_1h: last_hour.count(),
        buckets,
        recent,
        interval_secs: interval_secs(),
    }
}

pub fn stats(db: &Db, site: &str, hours: i64) -> Stats {
    let hours = hours.clamp(1, 168);
    let now = chrono::Utc::now().timestamp();
    let since = now - hours * 3600;
    let samples: Vec<(i64, i64, i64)> = {
        let c = db.conn();
        let mut st = match c.prepare(
            "SELECT ts, code, ms FROM monitor_samples WHERE site_id = ?1 AND ts >= ?2 ORDER BY ts",
        ) {
            Ok(s) => s,
            Err(_) => return compute_stats(site, now, hours, &[]),
        };
        st.query_map(rusqlite::params![site, since], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
    };
    compute_stats(site, now, hours, &samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_ignores_resets() {
        let s = vec![(0, 100, 10), (60, 300, 20), (120, 50, 5), (180, 150, 25)];
        assert_eq!(traffic(&s), (300, 30));
    }

    #[test]
    fn stats_buckets_and_uptime() {
        let now = 10 * 3600 + 1800; // 10:30
        let s = vec![
            (now - 60, 200, 100),
            (now - 120, 200, 300),
            (now - 180, 502, 50),
            (now - 2 * 3600, 200, 80),
        ];
        let st = compute_stats("a.test", now, 24, &s);
        assert_eq!(st.buckets.len(), 24);
        let cur = st.buckets.last().unwrap();
        assert_eq!(cur.ts, 10 * 3600);
        assert_eq!(cur.checks, 3);
        assert_eq!(cur.failures, 1);
        // failures are excluded from response times: [100, 300]
        assert_eq!(cur.p50, Some(300));
        assert_eq!(cur.p95, Some(300));
        assert_eq!(st.uptime, Some(0.75));
        assert_eq!(st.last_code, Some(200));
        assert_eq!(st.failures_1h, 1);
        assert_eq!(st.checks_1h, 3);
        assert!(is_failure(0) && is_failure(503) && !is_failure(301) && !is_failure(404));
    }
}
