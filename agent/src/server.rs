//! Server tools: package updates, reboot, timezone, root SSH keys.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::jobs::JobSpec;
use crate::state::AppState;
use crate::util::cmd::Cmd;

const AUTH_KEYS: &str = "/root/.ssh/authorized_keys";

fn done(res: ApiResult<Response>, info: AuditInfo) -> Response {
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn mtime(p: &str) -> Option<i64> {
    std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

pub fn current_timezone() -> String {
    if let Ok(t) = std::fs::read_link("/etc/localtime") {
        let s = t.to_string_lossy().to_string();
        if let Some(i) = s.find("zoneinfo/") {
            return s[i + 9..].to_string();
        }
    }
    std::fs::read_to_string("/etc/timezone").map(|s| s.trim().to_string()).unwrap_or_else(|_| "UTC".into())
}

/// `GET /api/server` — timezone, reboot needed, last update check, uptime.
pub async fn info() -> Json<Value> {
    let reboot = Path::new("/var/run/reboot-required").exists();
    let pkgs: Vec<String> = std::fs::read_to_string("/var/run/reboot-required.pkgs").unwrap_or_default().lines().map(str::to_string).collect();
    let uptime = std::fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok())).map(|x| x as i64);
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
    let auto = std::fs::read_to_string("/etc/apt/apt.conf.d/20auto-upgrades").unwrap_or_default().contains("Unattended-Upgrade \"1\"");
    Json(json!({
        "timezone": current_timezone(),
        "reboot_required": reboot,
        "reboot_packages": pkgs,
        "uptime_secs": uptime,
        "kernel": kernel,
        "last_apt_update": mtime("/var/lib/apt/periodic/update-success-stamp").or_else(|| mtime("/var/cache/apt/pkgcache.bin")),
        "auto_security_updates": auto,
    }))
}

/// `GET /api/server/timezones`
pub async fn timezones() -> Json<Value> {
    let o = Cmd::new("timedatectl").arg("list-timezones").timeout(Duration::from_secs(10)).run().await;
    let mut v: Vec<String> = if o.ok { o.stdout.lines().map(str::to_string).collect() } else { Vec::new() };
    if v.is_empty() {
        v = vec!["UTC".into()];
    }
    Json(json!(v))
}

#[derive(Deserialize)]
pub struct TzBody {
    timezone: String,
}

pub async fn set_timezone(Json(b): Json<TzBody>) -> Response {
    let info = AuditInfo::action("server.timezone", b.timezone.clone());
    let res = async {
        let tz = b.timezone.trim();
        let ok_chars = !tz.is_empty() && tz.len() < 64 && !tz.contains("..") && tz.bytes().all(|c| c.is_ascii_alphanumeric() || b"/_+-".contains(&c));
        if !ok_chars || !Path::new("/usr/share/zoneinfo").join(tz).is_file() {
            return Err(ApiError::bad_request("unknown timezone"));
        }
        let o = Cmd::new("timedatectl").args(["set-timezone", tz]).timeout(Duration::from_secs(20)).run().await;
        if !o.ok {
            // containers / no timedated: set the link ourselves
            let _ = std::fs::remove_file("/etc/localtime");
            std::os::unix::fs::symlink(format!("/usr/share/zoneinfo/{tz}"), "/etc/localtime").map_err(|e| ApiError::internal(e.to_string()))?;
            let _ = std::fs::write("/etc/timezone", format!("{tz}\n"));
        }
        // cron and PHP read the zone at start
        let _ = Cmd::new("sh").args(["-c", "systemctl try-restart cron 2>/dev/null; true"]).run().await;
        Ok(Json(json!({"timezone": current_timezone()})).into_response())
    }
    .await;
    done(res, info)
}

// ------------------------------------------------------------------ updates

#[derive(Serialize, Debug, PartialEq)]
pub struct Upgradable {
    name: String,
    from: String,
    to: String,
    security: bool,
}

pub fn parse_upgradable(text: &str) -> Vec<Upgradable> {
    text.lines()
        .filter(|l| l.contains("[upgradable from:"))
        .filter_map(|l| {
            let (name_origin, rest) = l.split_once(' ')?;
            let (name, origin) = name_origin.split_once('/')?;
            let to = rest.split_whitespace().next()?.to_string();
            let from = l.split("[upgradable from: ").nth(1)?.trim_end_matches(']').trim().to_string();
            Some(Upgradable { name: name.to_string(), from, to, security: origin.contains("-security") })
        })
        .collect()
}

/// `GET /api/server/updates` — what `apt` would upgrade (from the last check).
pub async fn updates() -> ApiResult<Json<Value>> {
    let o = Cmd::new("apt").args(["list", "--upgradable"]).env("LANG", "C").timeout(Duration::from_secs(60)).run().await;
    let list = parse_upgradable(&o.stdout);
    let security = list.iter().filter(|u| u.security).count();
    Ok(Json(json!({"packages": list, "security": security, "checked_at": mtime("/var/cache/apt/pkgcache.bin")})))
}

#[derive(Deserialize, Default)]
pub struct ApplyBody {
    #[serde(default)]
    security_only: bool,
}

fn apt_job(state: &AppState, addr: SocketAddr, kind: &str, title: &str, script: String) -> ApiResult<String> {
    let spec = JobSpec::new(kind, title).heavy().lock("apt").timeout(Duration::from_secs(3600)).actor(Some(addr.ip().to_canonical().to_string()));
    Ok(state.jobs.submit(spec, move |ctx| async move {
        let o = Cmd::new("bash")
            .args(["-c", &script])
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NEEDRESTART_MODE", "a")
            .env("LANG", "C")
            .timeout(Duration::from_secs(3600))
            .run()
            .await;
        for l in o.combined().lines().filter(|l| !l.trim().is_empty()).take(400) {
            ctx.log(l);
        }
        if !o.ok {
            anyhow::bail!("apt failed (exit {:?})", o.exit_code);
        }
        if Path::new("/var/run/reboot-required").exists() {
            ctx.log("A reboot is needed to finish (new kernel or system library).");
        }
        ctx.log("Done.");
        Ok(())
    })?)
}

/// `POST /api/server/updates/check` — `apt-get update` in a job.
pub async fn check(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    let mut info = AuditInfo::action("server.updates_check", "apt");
    let res = apt_job(&state, addr, "apt_update", "Check for updates", "apt-get update -q".into());
    if let Ok(j) = &res {
        info.job_id = Some(j.clone());
    }
    done(res.map(|j| Json(json!({"job_id": j})).into_response()), info)
}

/// `POST /api/server/updates/apply {"security_only"}` — install updates.
pub async fn apply(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<SocketAddr>, body: Option<Json<ApplyBody>>) -> Response {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let mut info = AuditInfo::action("server.updates_apply", if b.security_only { "security" } else { "all" });
    let res = async {
        let opts = "-y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";
        let script = if b.security_only {
            let o = Cmd::new("apt").args(["list", "--upgradable"]).env("LANG", "C").run().await;
            let names: Vec<String> = parse_upgradable(&o.stdout).into_iter().filter(|u| u.security).map(|u| u.name).filter(|n| n.bytes().all(|c| c.is_ascii_alphanumeric() || b".+-:".contains(&c))).collect();
            if names.is_empty() {
                return Err(ApiError::bad_request("no security updates waiting"));
            }
            format!("apt-get install --only-upgrade {opts} {}", names.join(" "))
        } else {
            format!("apt-get upgrade {opts}")
        };
        let title = if b.security_only { "Install security updates" } else { "Install all updates" };
        let j = apt_job(&state, addr, "apt_upgrade", title, script)?;
        info.job_id = Some(j.clone());
        Ok(Json(json!({"job_id": j})).into_response())
    }
    .await;
    done(res, info.clone())
}

#[derive(Deserialize)]
pub struct RebootBody {
    /// Must equal the hostname (typed by the person).
    confirm: String,
}

pub async fn reboot(State(state): State<AppState>, Json(b): Json<RebootBody>) -> Response {
    let info = AuditInfo::action("server.reboot", b.confirm.clone());
    let res = async {
        let host = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_default();
        if b.confirm.trim() != host || host.is_empty() {
            return Err(ApiError::bad_request("type the server name exactly to confirm"));
        }
        if state.test_hooks {
            return Ok(Json(json!({"ok": true, "dry_run": true})).into_response());
        }
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let _ = Cmd::new("systemctl").arg("reboot").run().await;
        });
        Ok(Json(json!({"ok": true, "in_secs": 3})).into_response())
    }
    .await;
    done(res, info)
}

// ------------------------------------------------------------------ root SSH keys

#[derive(Serialize)]
pub struct SshKey {
    fingerprint: String,
    kind: String,
    comment: String,
    bits: String,
}

async fn fingerprint(line: &str) -> Option<(String, String)> {
    let o = Cmd::new("ssh-keygen").args(["-l", "-f", "-"]).stdin(format!("{line}\n")).timeout(Duration::from_secs(5)).run().await;
    if !o.ok {
        return None;
    }
    let f: Vec<&str> = o.stdout.split_whitespace().collect();
    Some((f.get(1)?.to_string(), f.first()?.to_string()))
}

fn key_lines() -> Vec<String> {
    std::fs::read_to_string(AUTH_KEYS).unwrap_or_default().lines().map(str::to_string).collect()
}

pub async fn ssh_keys() -> ApiResult<Json<Value>> {
    let mut v = Vec::new();
    for l in key_lines() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((fp, bits)) = fingerprint(t).await else { continue };
        // options may come first: find the key type token
        let parts: Vec<&str> = t.split_whitespace().collect();
        let i = parts.iter().position(|p| p.starts_with("ssh-") || p.starts_with("ecdsa-") || p.starts_with("sk-")).unwrap_or(0);
        v.push(SshKey { fingerprint: fp, kind: parts.get(i).unwrap_or(&"").to_string(), comment: parts.get(i + 2..).map(|c| c.join(" ")).unwrap_or_default(), bits });
    }
    let pw = std::fs::read_to_string("/etc/ssh/sshd_config").unwrap_or_default();
    Ok(Json(json!({"keys": v, "file": AUTH_KEYS, "root_password_login": !pw.lines().any(|l| l.trim().starts_with("PermitRootLogin") && (l.contains("prohibit-password") || l.contains("no")))})))
}

#[derive(Deserialize)]
pub struct KeyBody {
    key: String,
}

fn write_keys(lines: &[String]) -> ApiResult<()> {
    std::fs::create_dir_all("/root/.ssh").map_err(|e| ApiError::internal(e.to_string()))?;
    let _ = std::fs::set_permissions("/root/.ssh", std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let mut body = lines.join("\n");
    body.push('\n');
    crate::util::atomic_write(Path::new(AUTH_KEYS), body.as_bytes(), 0o600, Some((0, 0))).map_err(|e| ApiError::internal(format!("{e:#}")))
}

pub async fn ssh_key_add(Json(b): Json<KeyBody>) -> Response {
    let info = AuditInfo::action("server.ssh_key_add", b.key.split_whitespace().nth(2).unwrap_or("").to_string());
    let res = async {
        let k = b.key.trim();
        if k.contains('\n') || k.len() > 16 * 1024 {
            return Err(ApiError::bad_request("paste one key line"));
        }
        if !["ssh-ed25519 ", "ssh-rsa ", "ecdsa-sha2-", "sk-ssh-ed25519@", "sk-ecdsa-"].iter().any(|p| k.starts_with(p)) {
            return Err(ApiError::bad_request("paste a public key line (ssh-ed25519 AAAA… you@laptop)"));
        }
        let (fp, _) = fingerprint(k).await.ok_or_else(|| ApiError::bad_request("that is not a valid public key"))?;
        let mut lines = key_lines();
        for l in &lines {
            if fingerprint(l.trim()).await.is_some_and(|(f, _)| f == fp) {
                return Err(ApiError::bad_request("that key is already there"));
            }
        }
        lines.push(k.to_string());
        write_keys(&lines)?;
        Ok(Json(json!({"ok": true, "fingerprint": fp})).into_response())
    }
    .await;
    done(res, info)
}

pub async fn ssh_key_remove(UrlPath(fp): UrlPath<String>) -> Response {
    let info = AuditInfo::action("server.ssh_key_remove", fp.clone());
    let res = async {
        let lines = key_lines();
        let mut keep = Vec::new();
        let mut removed = false;
        let mut remaining = 0;
        for l in lines {
            match fingerprint(l.trim()).await {
                Some((f, _)) if f == fp => removed = true,
                Some(_) => {
                    remaining += 1;
                    keep.push(l)
                }
                None => keep.push(l),
            }
        }
        if !removed {
            return Err(ApiError::not_found("key not found"));
        }
        if remaining == 0 {
            return Err(ApiError::bad_request("that is the last SSH key for root — add another one first so you can't lock yourself out"));
        }
        write_keys(&keep)?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    done(res, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apt_list() {
        let t = "Listing...\nnginx/noble-updates 1.24.0-2ubuntu7.1 amd64 [upgradable from: 1.24.0-2ubuntu7]\nopenssl/noble-security,noble-updates 3.0.13-0ubuntu3.5 amd64 [upgradable from: 3.0.13-0ubuntu3.4]\n";
        let v = parse_upgradable(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], Upgradable { name: "nginx".into(), from: "1.24.0-2ubuntu7".into(), to: "1.24.0-2ubuntu7.1".into(), security: false });
        assert!(v[1].security);
    }
}
