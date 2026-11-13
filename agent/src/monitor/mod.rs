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

fn prune(db: &Db) {
    let cutoff = chrono::Utc::now().timestamp() - KEEP_SECS;
    let _ = db
        .conn()
        .execute("DELETE FROM monitor_samples WHERE ts < ?1", [cutoff]);
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

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let every = interval_secs();
        let hourly = (3600 / every).max(1);
        let mut tick: u64 = 0;
        let mut interval = tokio::time::interval(Duration::from_secs(every));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
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
    pub interval_secs: u64,
}

fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(idx).copied()
}

pub fn compute_stats(site: &str, now: i64, hours: i64, samples: &[(i64, i64, i64)]) -> Stats {
    let start_hour = (now / 3600 - hours + 1) * 3600;
    let mut buckets = Vec::new();
    for h in 0..hours {
        let from = start_hour + h * 3600;
        let to = from + 3600;
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
