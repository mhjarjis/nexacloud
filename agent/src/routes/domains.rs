//! `/api/domains` (every hostname nginx serves, with DNS + certificate state)
//! and `/api/workspace` (every site as a project card).

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;
use crate::util::cmd::Cmd;

static PUBLIC_IP: LazyLock<Mutex<Option<(Instant, Vec<String>)>>> = LazyLock::new(Default::default);

/// This server's addresses: interface IPs plus the public IP (cached 1 h).
async fn server_ips() -> Vec<String> {
    if let Some((at, v)) = PUBLIC_IP.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if at.elapsed() < Duration::from_secs(3600) {
            return v;
        }
    }
    let mut ips: Vec<String> = Cmd::new("hostname")
        .arg("-I")
        .timeout(Duration::from_secs(5))
        .run()
        .await
        .stdout
        .split_whitespace()
        .map(String::from)
        .collect();
    let o = Cmd::new("curl")
        .args(["-fsS", "--max-time", "4", "https://api.ipify.org"])
        .timeout(Duration::from_secs(6))
        .run()
        .await;
    let pubip = o.stdout.trim().to_string();
    if o.ok && !pubip.is_empty() && pubip.len() < 46 && !ips.contains(&pubip) {
        ips.push(pubip);
    }
    *PUBLIC_IP.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), ips.clone()));
    ips
}

fn cached_ips() -> Vec<String> {
    PUBLIC_IP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// A / AAAA records of `name` through the system resolver.
async fn resolve(name: &str) -> Vec<String> {
    let o = Cmd::new("getent")
        .args(["ahosts", name])
        .timeout(Duration::from_secs(4))
        .run()
        .await;
    let mut v: Vec<String> = o
        .stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect();
    v.sort();
    v.dedup();
    v
}

#[derive(serde::Deserialize)]
pub struct DomainsQuery {
    /// `false`: skip DNS lookups (instant list; DNS state "unknown").
    dns: Option<bool>,
}

pub async fn domains(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<DomainsQuery>,
) -> Json<Value> {
    let with_dns = q.dns.unwrap_or(true);
    let (sites, ips) = tokio::join!(crate::routes::sites::scan(false), async {
        if with_dns { server_ips().await } else { cached_ips() }
    });
    let projects: HashSet<String> = crate::deploys::list_projects(&state.db)
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.id)
        .collect();
    let mut rows: Vec<(String, &crate::routes::sites::Site, bool)> = Vec::new();
    for s in &sites {
        for (i, n) in s.server_names.iter().enumerate() {
            if n == "_" || n == "localhost" || n.starts_with('~') || n.contains('*') {
                continue;
            }
            rows.push((n.clone(), s, i == 0));
        }
    }
    let resolved: Vec<Option<Vec<String>>> = if with_dns {
        let futs = rows
            .iter()
            .map(|(n, _, _)| {
                let n = n.clone();
                async move { resolve(&n).await }
            })
            .collect();
        crate::util::join_all(futs).await
    } else {
        rows.iter().map(|_| None).collect()
    };
    let out: Vec<Value> = rows
        .iter()
        .zip(resolved)
        .map(|((name, s, primary), r)| {
            let checked = r.is_some();
            let r = r.unwrap_or_default();
            let here = r.iter().any(|ip| ips.contains(ip));
            let kind = if s.proxy_pass.is_some() && s.root.is_none() {
                "Proxy"
            } else if !primary {
                "Alias"
            } else if name.split('.').count() > 2 && !name.starts_with("www.") {
                "Subdomain"
            } else {
                "Domain"
            };
            json!({
                "domain": name,
                "site_id": s.id,
                "primary": primary,
                "kind": kind,
                "site_kind": s.kind,
                "serves": s.root.clone().or_else(|| s.proxy_pass.clone()),
                "php_version": s.php_version,
                "ssl": s.ssl,
                "ssl_expires": s.ssl_expires,
                "managed": s.config_path.starts_with(crate::plans::sites::MANAGED_PREFIX),
                "project": projects.contains(&s.id),
                "dns": {
                    "records": r,
                    "points_here": here,
                    "state": if !checked { "unknown" } else if r.is_empty() { "missing" } else if here { "ok" } else { "elsewhere" },
                },
            })
        })
        .collect();
    Json(json!({"server_ips": ips, "domains": out}))
}

/// Every site as a project card: site info, its Git project (if any) and
/// the last 24 h of monitoring.
pub async fn workspace(State(state): State<AppState>) -> Json<Value> {
    let sites = crate::routes::sites::scan(true).await;
    let projects = crate::deploys::list_projects(&state.db).unwrap_or_default();
    let out: Vec<Value> = sites
        .iter()
        .filter(|s| s.id != "_" && s.id != "localhost")
        .map(|s| {
            let p = projects.iter().find(|p| p.site_id == s.id);
            let last = p.and_then(|p| {
                crate::deploys::list_deploys(&state.db, Some(&p.id), 1)
                    .ok()
                    .and_then(|v| v.into_iter().next())
            });
            let st = crate::monitor::stats(&state.db, &s.id, 24);
            let p95 = st.buckets.iter().rev().find_map(|b| b.p95);
            json!({
                "id": s.id,
                "site": s,
                "project": p.map(|p| json!({
                    "kind": p.kind, "repo": p.repo, "branch": p.branch, "strategy": p.strategy,
                    "auto_deploy": p.auto_deploy, "last_commit": p.last_commit, "dir": p.dir,
                })),
                "last_deploy": last,
                "monitor": {"uptime": st.uptime, "last_code": st.last_code, "last_ms": st.last_ms, "p95": p95},
                "managed": s.config_path.starts_with(crate::plans::sites::MANAGED_PREFIX),
            })
        })
        .collect();
    Json(json!(out))
}
