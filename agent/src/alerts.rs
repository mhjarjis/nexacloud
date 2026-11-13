//! Overview alert rules — pure functions over already-collected data.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::routes::php::{is_eol, parse_version, PhpVersion};
use crate::routes::services::ServiceStatus;
use crate::routes::system::DiskInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub link: String,
}

fn alert(level: Level, title: impl Into<String>, detail: impl Into<String>, link: &str) -> Alert {
    Alert {
        level,
        title: title.into(),
        detail: detail.into(),
        link: link.into(),
    }
}

/// Minimal view of a site for alerting.
pub struct SiteSsl<'a> {
    pub id: &'a str,
    pub ssl_expires: Option<&'a str>,
}

pub struct Inputs<'a> {
    pub now: DateTime<Utc>,
    pub php: &'a [PhpVersion],
    pub ufw_installed: bool,
    pub ufw_active: bool,
    pub fail2ban_installed: bool,
    pub services: &'a [ServiceStatus],
    pub sites: &'a [SiteSsl<'a>],
    pub disks: &'a [DiskInfo],
}

const SSL_WARN_DAYS: i64 = 14;
const DISK_WARN_PERCENT: f64 = 85.0;

fn fmt_gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1_000_000_000.0)
}

pub fn compute(i: &Inputs) -> Vec<Alert> {
    let mut out = Vec::new();
    let today = i.now.date_naive();

    // PHP
    for p in i.php {
        if p.sites.is_empty() {
            continue;
        }
        let detail = format!("Used by: {}", p.sites.join(", "));
        let old = matches!(parse_version(&p.version), Some(v) if v <= (8, 1));
        if old || is_eol(&p.version, today) {
            out.push(alert(
                Level::Critical,
                format!("PHP {} no longer gets security fixes", p.version),
                detail,
                "/php",
            ));
        } else if p.version == "8.2" {
            out.push(alert(
                Level::Warning,
                "PHP 8.2 security support ends 2026-12-31",
                detail,
                "/php",
            ));
        }
    }

    // Firewall
    if !i.ufw_installed {
        out.push(alert(
            Level::Critical,
            "Firewall (UFW) is not installed",
            "Nothing is filtering incoming traffic to this server.",
            "/security",
        ));
    } else if !i.ufw_active {
        out.push(alert(
            Level::Critical,
            "Firewall (UFW) is inactive",
            "UFW is installed but not enabled; all ports are reachable.",
            "/security",
        ));
    }

    // Services
    for s in i.services.iter().filter(|s| s.installed && !s.active) {
        out.push(alert(
            Level::Critical,
            format!("{} is not running", s.label),
            format!("systemd unit {} is {}", s.unit, s.state),
            "/services",
        ));
    }

    // SSL
    for s in i.sites {
        let Some(exp) = s
            .ssl_expires
            .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
            .map(|d| d.with_timezone(&Utc))
        else {
            continue;
        };
        let date = exp.format("%Y-%m-%d");
        if exp <= i.now {
            out.push(alert(
                Level::Critical,
                format!("SSL certificate for {} has expired", s.id),
                format!("Expired on {date}"),
                "/sites",
            ));
        } else {
            let days = (exp - i.now).num_days();
            if days < SSL_WARN_DAYS {
                let when = match days {
                    0 => "in less than a day".to_string(),
                    1 => "in 1 day".to_string(),
                    n => format!("in {n} days"),
                };
                out.push(alert(
                    Level::Warning,
                    format!("SSL certificate for {} expires {when}", s.id),
                    format!("Expires on {date}"),
                    "/sites",
                ));
            }
        }
    }

    // Disks
    for d in i.disks {
        if d.total == 0 {
            continue;
        }
        let pct = d.used as f64 * 100.0 / d.total as f64;
        if pct > DISK_WARN_PERCENT {
            out.push(alert(
                Level::Warning,
                format!("Disk {} is {:.0}% full", d.mount, pct.floor()),
                format!("{} of {} used", fmt_gb(d.used), fmt_gb(d.total)),
                "/",
            ));
        }
    }

    // fail2ban
    if !i.fail2ban_installed {
        out.push(alert(
            Level::Info,
            "fail2ban is not installed",
            "fail2ban bans IPs that repeatedly fail SSH logins.",
            "/security",
        ));
    }

    out.sort_by_key(|a| a.level);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn php(v: &str, sites: &[&str]) -> PhpVersion {
        PhpVersion {
            version: v.into(),
            fpm_installed: true,
            fpm_active: true,
            eol: false,
            security_until: None,
            sites: sites.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn svc(unit: &str, installed: bool, active: bool) -> ServiceStatus {
        ServiceStatus {
            unit: unit.into(),
            label: unit.to_uppercase(),
            installed,
            active,
            state: if active { "active".into() } else { "failed".into() },
            version: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn healthy<'a>(php: &'a [PhpVersion], services: &'a [ServiceStatus], sites: &'a [SiteSsl<'a>], disks: &'a [DiskInfo]) -> Inputs<'a> {
        Inputs {
            now: now(),
            php,
            ufw_installed: true,
            ufw_active: true,
            fail2ban_installed: true,
            services,
            sites,
            disks,
        }
    }

    #[test]
    fn healthy_server_has_no_alerts() {
        let p = [php("8.3", &["shop"]), php("8.1", &[])];
        let s = [svc("nginx", true, true), svc("mariadb", false, false)];
        let sites = [SiteSsl { id: "shop", ssl_expires: Some("2027-01-12T10:00:00Z") }];
        let d = [DiskInfo { mount: "/".into(), total: 100, used: 85 }];
        assert!(compute(&healthy(&p, &s, &sites, &d)).is_empty());
    }

    #[test]
    fn php_rules() {
        let p = [php("8.1", &["a.com", "b.com"]), php("8.2", &["c.com"]), php("7.4", &["d.com"]), php("8.3", &["e.com"])];
        let a = compute(&healthy(&p, &[], &[], &[]));
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].level, Level::Critical);
        assert_eq!(a[0].title, "PHP 8.1 no longer gets security fixes");
        assert_eq!(a[0].detail, "Used by: a.com, b.com");
        assert_eq!(a[0].link, "/php");
        assert_eq!(a[1].title, "PHP 7.4 no longer gets security fixes");
        assert_eq!(a[2].level, Level::Warning);
        assert_eq!(a[2].title, "PHP 8.2 security support ends 2026-12-31");
    }

    #[test]
    fn firewall_and_fail2ban() {
        let mut i = healthy(&[], &[], &[], &[]);
        i.ufw_installed = false;
        i.fail2ban_installed = false;
        let a = compute(&i);
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].level, a[0].link.as_str()), (Level::Critical, "/security"));
        assert!(a[0].title.contains("not installed"));
        assert_eq!((a[1].level, a[1].link.as_str()), (Level::Info, "/security"));
        i.ufw_installed = true;
        i.ufw_active = false;
        let a = compute(&i);
        assert!(a[0].title.contains("inactive"));
    }

    #[test]
    fn services_down() {
        let s = [svc("nginx", true, false), svc("mysql", false, false), svc("cron", true, true)];
        let a = compute(&healthy(&[], &s, &[], &[]));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].level, Level::Critical);
        assert_eq!(a[0].link, "/services");
        assert!(a[0].title.contains("NGINX"));
    }

    #[test]
    fn ssl_rules() {
        let sites = [
            SiteSsl { id: "old.com", ssl_expires: Some("2026-11-01T00:00:00Z") },
            SiteSsl { id: "soon.com", ssl_expires: Some("2026-11-20T00:00:00Z") },
            SiteSsl { id: "fine.com", ssl_expires: Some("2026-12-30T00:00:00Z") },
            SiteSsl { id: "none.com", ssl_expires: None },
        ];
        let a = compute(&healthy(&[], &[], &sites, &[]));
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].level, Level::Critical);
        assert_eq!(a[0].title, "SSL certificate for old.com has expired");
        assert_eq!(a[1].level, Level::Warning);
        assert_eq!(a[1].title, "SSL certificate for soon.com expires in 6 days");
        assert_eq!(a[1].link, "/sites");
    }

    #[test]
    fn disk_rule() {
        let d = [
            DiskInfo { mount: "/".into(), total: 1000, used: 900 },
            DiskInfo { mount: "/data".into(), total: 1000, used: 850 },
        ];
        let a = compute(&healthy(&[], &[], &[], &d));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].level, Level::Warning);
        assert_eq!(a[0].title, "Disk / is 90% full");
        assert_eq!(a[0].link, "/");
    }

    #[test]
    fn sorted_by_severity() {
        let mut i = healthy(&[], &[], &[], &[]);
        i.fail2ban_installed = false;
        let d = [DiskInfo { mount: "/".into(), total: 100, used: 99 }];
        i.disks = &d;
        i.ufw_active = false;
        let levels: Vec<Level> = compute(&i).iter().map(|a| a.level).collect();
        assert_eq!(levels, vec![Level::Critical, Level::Warning, Level::Info]);
    }
}
