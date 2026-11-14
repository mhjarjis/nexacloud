//! Visitor stats per site, from nginx access logs (no tracking script).
//!
//! `stats_enable` (a plan) adds one shared access log with the host in each
//! line, plus daily rotation for 30 days. Stats read that log (and rotated
//! copies) for one host.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::changes::{ChangeEnv, ChangePlan};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::cmd::Cmd;

pub const CONF: &str = "/etc/nginx/conf.d/nexacloud-stats.conf";
pub const LOG: &str = "/var/log/nginx/nexacloud-access.log";
const ROTATE: &str = "/etc/logrotate.d/nexacloud-access";

pub fn plan_enable(env: &ChangeEnv) -> Result<ChangePlan, String> {
    let conf = format!(
        "# Managed by NexaCloud — one access log with the site name, for visitor stats.\nlog_format nexacloud_stats '$host\\t$time_iso8601\\t$remote_addr\\t$status\\t$body_bytes_sent\\t$request_method\\t$request_uri\\t$http_referer\\t$http_user_agent';\naccess_log {LOG} nexacloud_stats;\n"
    );
    let rotate = format!(
        "{LOG} {{\n    daily\n    rotate 30\n    missingok\n    notifempty\n    compress\n    delaycompress\n    create 0640 www-data adm\n    sharedscripts\n    postrotate\n        [ -s /run/nginx.pid ] && kill -USR1 \"$(cat /run/nginx.pid)\"\n    endscript\n}}\n"
    );
    ChangePlan::builder("stats_enable", "Turn on visitor stats", "nginx:stats")
        .lock("nginx")
        .write_file(CONF, conf)
        .write_file(ROTATE, rotate)
        .command(["nginx", "-t"])
        .command(["systemctl", "reload", "nginx"])
        .verify_command(["systemctl", "is-active", "--quiet", "nginx"])
        .on_revert(["systemctl", "reload", "nginx"])
        .note("Adds one shared access log (30 days, rotated daily). Stats are computed on this server — no tracking script, nothing sent anywhere.")
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Deserialize)]
pub struct StatsQ {
    days: Option<u32>,
}

#[derive(Serialize, Default, Clone)]
struct Day {
    date: String,
    requests: u64,
    visitors: u64,
    bytes: u64,
}

fn is_bot(ua: &str) -> bool {
    let u = ua.to_ascii_lowercase();
    u.is_empty() || ["bot", "crawl", "spider", "slurp", "curl/", "wget", "python-requests", "go-http-client", "headless", "monitor", "uptime"].iter().any(|b| u.contains(b))
}

fn is_asset(uri: &str) -> bool {
    let u = uri.to_ascii_lowercase();
    [".css", ".js", ".mjs", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".avif", ".svg", ".ico", ".woff", ".woff2", ".map", ".txt", ".xml"].iter().any(|e| u.ends_with(e))
}

fn top(m: HashMap<String, u64>, n: usize) -> Vec<Value> {
    let mut v: Vec<(String, u64)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().take(n).map(|(k, c)| json!({"name": k, "count": c})).collect()
}

/// Aggregate log lines (tab-separated, see `log_format`) for `host` over the
/// last `days` days ending `today` (YYYY-MM-DD).
pub fn aggregate(text: &str, host: &str, days: u32, today: chrono::NaiveDate) -> Value {
    let first = today - chrono::Duration::days(days as i64 - 1);
    let mut per_day: BTreeMap<String, (u64, HashSet<u64>, u64)> = BTreeMap::new();
    let (mut pages, mut refs, mut statuses, mut agents) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let (mut bots, mut total, mut bytes) = (0u64, 0u64, 0u64);
    let mut all_visitors = HashSet::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 9 || !f[0].eq_ignore_ascii_case(host) {
            continue;
        }
        let Some(date) = f[1].get(..10) else { continue };
        let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { continue };
        if d < first || d > today {
            continue;
        }
        let (ip, status, size, referer, ua) = (f[2], f[3], f[4].parse::<u64>().unwrap_or(0), f[7], f[8]);
        let uri = f[6].split('?').next().unwrap_or(f[6]);
        total += 1;
        bytes += size;
        *statuses.entry(format!("{}xx", &status.get(..1).unwrap_or("?"))).or_insert(0u64) += 1;
        if is_bot(ua) {
            bots += 1;
            continue;
        }
        let visitor = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (ip, ua).hash(&mut h);
            h.finish()
        };
        all_visitors.insert(visitor);
        let e = per_day.entry(date.to_string()).or_insert((0, HashSet::new(), 0));
        e.0 += 1;
        e.1.insert(visitor);
        e.2 += size;
        if !is_asset(uri) && status.starts_with('2') {
            *pages.entry(uri.to_string()).or_insert(0u64) += 1;
        }
        if referer != "-" && !referer.is_empty() {
            let rhost = referer.split("://").nth(1).unwrap_or(referer).split('/').next().unwrap_or("").to_string();
            if !rhost.is_empty() && !rhost.eq_ignore_ascii_case(host) {
                *refs.entry(rhost).or_insert(0u64) += 1;
            }
        }
        let browser = if ua.contains("Edg/") { "Edge" } else if ua.contains("Firefox/") { "Firefox" } else if ua.contains("Chrome/") { "Chrome" } else if ua.contains("Safari/") { "Safari" } else { "Other" };
        *agents.entry(browser.to_string()).or_insert(0u64) += 1;
    }
    let mut series = Vec::new();
    for i in 0..days {
        let d = (first + chrono::Duration::days(i as i64)).format("%Y-%m-%d").to_string();
        let (r, v, b) = per_day.get(&d).map(|(r, v, b)| (*r, v.len() as u64, *b)).unwrap_or((0, 0, 0));
        series.push(Day { date: d, requests: r, visitors: v, bytes: b });
    }
    json!({
        "host": host,
        "days": series,
        "totals": {"requests": total, "human_requests": total - bots, "bot_requests": bots, "visitors": all_visitors.len(), "bytes": bytes},
        "top_pages": top(pages, 10),
        "top_referrers": top(refs, 10),
        "status": top(statuses, 6),
        "browsers": top(agents, 6),
    })
}

/// `GET /api/sites/{id}/stats?days=7`
pub async fn site_stats(State(state): State<AppState>, UrlPath(id): UrlPath<String>, Query(q): Query<StatsQ>) -> ApiResult<Json<Value>> {
    if !crate::plans::sites::valid_site_id(&id) {
        return Err(ApiError::bad_request("invalid site"));
    }
    let enabled = std::path::Path::new(&state.changes_env.host_path(CONF)).exists();
    if !enabled {
        return Ok(Json(json!({"enabled": false})));
    }
    let days = q.days.unwrap_or(7).clamp(1, 30);
    // current + rotated logs, only lines for this host (grep keeps memory small)
    let o = Cmd::new("sh")
        .args([
            "-c",
            "p=$(printf '%s\\t' \"$2\"); { cat \"$1\" \"$1.1\" 2>/dev/null; for f in $(ls -1 \"$1\".*.gz 2>/dev/null | sort -V | head -n \"$3\"); do zcat \"$f\"; done; } | grep -F -i -- \"$p\" | tail -n 2000000",
            "stats",
            LOG,
            &id,
            &days.to_string(),
        ])
        .timeout(Duration::from_secs(60))
        .run()
        .await;
    let mut v = aggregate(&o.stdout, &id, days, chrono::Utc::now().date_naive());
    v["enabled"] = json!(true);
    Ok(Json(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_one_host() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let ua = "Mozilla/5.0 Chrome/120 Safari/537";
        let lines = [
            format!("shop.test\t2026-11-14T10:00:00+00:00\t1.1.1.1\t200\t1000\tGET\t/\thttps://google.com/search\t{ua}"),
            format!("shop.test\t2026-11-14T10:01:00+00:00\t1.1.1.1\t200\t500\tGET\t/products\t-\t{ua}"),
            format!("shop.test\t2026-11-13T09:00:00+00:00\t2.2.2.2\t404\t100\tGET\t/nope\t-\t{ua}"),
            "shop.test\t2026-11-14T10:02:00+00:00\t3.3.3.3\t200\t100\tGET\t/\t-\tGooglebot/2.1".to_string(),
            format!("other.test\t2026-11-14T10:00:00+00:00\t1.1.1.1\t200\t1000\tGET\t/\t-\t{ua}"),
            format!("shop.test\t2026-10-01T10:00:00+00:00\t1.1.1.1\t200\t1000\tGET\t/old\t-\t{ua}"),
            format!("shop.test\t2026-11-14T10:03:00+00:00\t1.1.1.1\t200\t50\tGET\t/app.css\t-\t{ua}"),
        ]
        .join("\n");
        let v = aggregate(&lines, "shop.test", 7, today);
        assert_eq!(v["totals"]["requests"], 5);
        assert_eq!(v["totals"]["bot_requests"], 1);
        assert_eq!(v["totals"]["visitors"], 2);
        let days = v["days"].as_array().unwrap();
        assert_eq!(days.len(), 7);
        assert_eq!(days[6]["requests"], 3);
        assert_eq!(days[6]["visitors"], 1);
        assert_eq!(days[5]["requests"], 1);
        assert_eq!(v["top_pages"][0]["name"], "/");
        assert!(v["top_pages"].as_array().unwrap().iter().all(|p| p["name"] != "/app.css" && p["name"] != "/nope"));
        assert_eq!(v["top_referrers"][0]["name"], "google.com");
    }
}
