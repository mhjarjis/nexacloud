//! systemd services: status of the known units, and restart.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::Path;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::routes::php;
use crate::util::cmd::{extract_version, Cmd};
use crate::util::join_all;

/// Fixed known units (plus every `phpX.Y-fpm` found on disk).
pub const FIXED_UNITS: [(&str, &str); 9] = [
    ("nginx", "Nginx"),
    ("mysql", "MySQL"),
    ("mariadb", "MariaDB"),
    ("postgresql", "PostgreSQL"),
    ("redis-server", "Redis"),
    ("supervisor", "Supervisor"),
    ("cron", "Cron"),
    ("fail2ban", "Fail2ban"),
    ("ufw", "UFW firewall"),
];

const RESTART_TIMEOUT: Duration = Duration::from_secs(90);
const VERSION_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub unit: String,
    pub label: String,
    pub installed: bool,
    pub active: bool,
    pub state: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnitState {
    pub installed: bool,
    pub active: bool,
    pub state: String,
}

/// Parse `systemctl show --property=LoadState,ActiveState` output.
pub fn parse_systemctl_show(out: &str) -> UnitState {
    let mut load = "";
    let mut active = "";
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("LoadState=") {
            load = v.trim();
        } else if let Some(v) = line.strip_prefix("ActiveState=") {
            active = v.trim();
        }
    }
    let installed = load == "loaded";
    UnitState {
        installed,
        active: installed && active == "active",
        state: if installed {
            if active.is_empty() {
                "unknown".into()
            } else {
                active.to_string()
            }
        } else if load == "masked" {
            "masked".into()
        } else {
            "not-installed".into()
        },
    }
}

pub async fn unit_state(unit: &str) -> UnitState {
    let out = Cmd::new("systemctl")
        .args(["show", "--no-pager", "--property=LoadState,ActiveState"])
        .arg(format!("{unit}.service"))
        .timeout(Duration::from_secs(10))
        .run()
        .await;
    if !out.ok {
        return UnitState {
            installed: false,
            active: false,
            state: "not-installed".into(),
        };
    }
    parse_systemctl_show(&out.stdout)
}

/// Known units: the fixed list plus `phpX.Y-fpm` for each PHP on disk.
pub fn known_units() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = FIXED_UNITS
        .iter()
        .map(|(u, l)| (u.to_string(), l.to_string()))
        .collect();
    for ver in php::versions_on_disk(std::path::Path::new(php::PHP_ETC)) {
        v.push((format!("php{ver}-fpm"), format!("PHP {ver} FPM")));
    }
    v
}

pub fn is_known_unit(unit: &str, known: &[(String, String)]) -> bool {
    known.iter().any(|(u, _)| u == unit)
}

fn version_command(unit: &str) -> Option<(String, Vec<&'static str>)> {
    let c = match unit {
        "nginx" => ("nginx".to_string(), vec!["-v"]),
        "mysql" => ("mysqld".to_string(), vec!["--version"]),
        "mariadb" => ("mariadbd".to_string(), vec!["--version"]),
        "postgresql" => ("psql".to_string(), vec!["--version"]),
        "redis-server" => ("redis-server".to_string(), vec!["--version"]),
        "supervisor" => ("supervisord".to_string(), vec!["--version"]),
        "fail2ban" => ("fail2ban-client".to_string(), vec!["--version"]),
        "ufw" => ("ufw".to_string(), vec!["--version"]),
        u => {
            let ver = u.strip_prefix("php")?.strip_suffix("-fpm")?;
            php::parse_version(ver)?;
            (format!("php-fpm{ver}"), vec!["-v"])
        }
    };
    Some(c)
}

type VersionCache = Mutex<HashMap<String, (Option<String>, Instant)>>;
static VERSIONS: LazyLock<VersionCache> = LazyLock::new(Default::default);

async fn unit_version(unit: &str) -> Option<String> {
    if let Some((v, at)) = VERSIONS.lock().unwrap_or_else(|e| e.into_inner()).get(unit) {
        if at.elapsed() < VERSION_TTL {
            return v.clone();
        }
    }
    let (prog, args) = version_command(unit)?;
    let out = Cmd::new(&prog)
        .args(args)
        .timeout(Duration::from_secs(10))
        .run()
        .await;
    let v = if out.missing || out.timed_out {
        None
    } else {
        extract_version(&out.combined())
    };
    VERSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(unit.to_string(), (v.clone(), Instant::now()));
    v
}

fn forget_version(unit: &str) {
    VERSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(unit);
}

pub async fn statuses(with_versions: bool) -> Vec<ServiceStatus> {
    let futs = known_units()
        .into_iter()
        .map(|(unit, label)| async move {
            let st = unit_state(&unit).await;
            let version = if with_versions && st.installed {
                unit_version(&unit).await
            } else {
                None
            };
            ServiceStatus {
                unit,
                label,
                installed: st.installed,
                active: st.active,
                state: st.state,
                version,
            }
        })
        .collect();
    join_all(futs).await.into_iter().flatten().collect()
}

pub async fn list() -> Json<Vec<ServiceStatus>> {
    Json(statuses(true).await)
}

/// `POST /api/services/{unit}/restart` (sudo).
pub async fn restart(Path(unit): Path<String>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let info = crate::audit::AuditInfo::action("service.restart", unit.clone());
    let res = restart_inner(unit).await;
    crate::audit::audited(
        res.map(IntoResponse::into_response)
            .unwrap_or_else(IntoResponse::into_response),
        info,
    )
}

async fn restart_inner(unit: String) -> ApiResult<Json<Value>> {
    let known = known_units();
    if !is_known_unit(&unit, &known) {
        return Err(ApiError::bad_request(format!("unknown service: {unit}")));
    }
    tracing::info!(unit, "restarting service");
    let out = Cmd::new("systemctl")
        .arg("restart")
        .arg(format!("{unit}.service"))
        .timeout(RESTART_TIMEOUT)
        .run()
        .await;
    forget_version(&unit);
    if out.ok {
        Ok(Json(json!({"ok": true})))
    } else {
        tracing::warn!(unit, "restart failed: {}", out.stderr.trim());
        Err(ApiError::internal(out.error_summary("systemctl")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemctl_show_parsing() {
        let s = parse_systemctl_show("LoadState=loaded\nActiveState=active\n");
        assert_eq!(
            s,
            UnitState {
                installed: true,
                active: true,
                state: "active".into()
            }
        );
        let s = parse_systemctl_show("ActiveState=failed\nLoadState=loaded\n");
        assert!(s.installed && !s.active);
        assert_eq!(s.state, "failed");
        let s = parse_systemctl_show("LoadState=not-found\nActiveState=inactive\n");
        assert!(!s.installed && !s.active);
        assert_eq!(s.state, "not-installed");
        let s = parse_systemctl_show("LoadState=masked\nActiveState=inactive\n");
        assert!(!s.installed);
        assert_eq!(s.state, "masked");
    }

    #[test]
    fn unit_validation() {
        let known = vec![
            ("nginx".to_string(), "Nginx".to_string()),
            ("php8.3-fpm".to_string(), "PHP 8.3 FPM".to_string()),
        ];
        assert!(is_known_unit("nginx", &known));
        assert!(is_known_unit("php8.3-fpm", &known));
        assert!(!is_known_unit("php8.4-fpm", &known));
        assert!(!is_known_unit("sshd", &known));
        assert!(!is_known_unit("nginx; reboot", &known));
        assert!(!is_known_unit("nginx.service", &known));
        assert!(!is_known_unit("", &known));
    }

    #[test]
    fn version_commands() {
        assert_eq!(version_command("php8.3-fpm").unwrap().0, "php-fpm8.3");
        assert!(version_command("phpx-fpm").is_none());
        assert!(version_command("cron").is_none());
    }
}
