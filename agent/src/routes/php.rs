//! PHP versions installed under /etc/php, their FPM state, support dates and
//! which sites use them.

use std::path::Path;

use axum::Json;
use chrono::{NaiveDate, Utc};
use serde::Serialize;

use crate::routes::{services, sites};
use crate::util::cmd::Cmd;
use crate::util::join_all;

pub const PHP_ETC: &str = "/etc/php";

/// Security-support end dates.
pub const SUPPORT_TABLE: [(&str, &str); 7] = [
    ("7.4", "2022-11-28"),
    ("8.0", "2023-11-26"),
    ("8.1", "2025-12-31"),
    ("8.2", "2026-12-31"),
    ("8.3", "2027-12-31"),
    ("8.4", "2028-12-31"),
    ("8.5", "2029-12-31"),
];

/// `"8.3"` → `(8, 3)`; only `^\d+\.\d+$`.
pub fn parse_version(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('.')?;
    if a.is_empty()
        || b.is_empty()
        || !a.bytes().all(|c| c.is_ascii_digit())
        || !b.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((a.parse().ok()?, b.parse().ok()?))
}

pub fn security_until(version: &str) -> Option<&'static str> {
    SUPPORT_TABLE
        .iter()
        .find(|(v, _)| *v == version)
        .map(|(_, d)| *d)
}

/// `eol` = today > security_until. Versions older than the table are EOL;
/// unknown newer versions are not.
pub fn is_eol(version: &str, today: NaiveDate) -> bool {
    match security_until(version).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        Some(until) => today > until,
        None => matches!(parse_version(version), Some(v) if v < (7, 4)),
    }
}

/// Version dirs (`^\d+\.\d+$`) under `base`, sorted ascending.
pub fn versions_on_disk(base: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut v: Vec<(u32, u32, String)> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse_version(&name).map(|(a, b)| (a, b, name))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, n)| n).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct PhpVersion {
    pub version: String,
    pub fpm_installed: bool,
    pub fpm_active: bool,
    pub eol: bool,
    pub security_until: Option<String>,
    pub sites: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhpInfo {
    pub cli_default: Option<String>,
    pub versions: Vec<PhpVersion>,
}

async fn cli_default() -> Option<String> {
    let out = Cmd::new("php")
        .args(["-r", "echo PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION;"])
        .timeout(std::time::Duration::from_secs(10))
        .run()
        .await;
    if !out.ok {
        return None;
    }
    let v = out.stdout.trim();
    parse_version(v).map(|_| v.to_string())
}

/// Build PHP info using an already-scanned site list.
pub async fn info(site_list: &[sites::Site]) -> PhpInfo {
    let today = Utc::now().date_naive();
    let versions = versions_on_disk(Path::new(PHP_ETC));
    let futs = versions
        .iter()
        .map(|v| {
            let unit = format!("php{v}-fpm");
            async move { services::unit_state(&unit).await }
        })
        .collect();
    let states = join_all(futs).await;
    let out = versions
        .into_iter()
        .zip(states)
        .map(|(version, st)| {
            let st = st.unwrap_or(services::UnitState {
                installed: false,
                active: false,
                state: "unknown".into(),
            });
            let mut used_by: Vec<String> = site_list
                .iter()
                .filter(|s| s.php_version.as_deref() == Some(version.as_str()))
                .map(|s| s.id.clone())
                .collect();
            used_by.sort();
            PhpVersion {
                eol: is_eol(&version, today),
                security_until: security_until(&version).map(str::to_string),
                fpm_installed: st.installed,
                fpm_active: st.active,
                sites: used_by,
                version,
            }
        })
        .collect();
    PhpInfo {
        cli_default: cli_default().await,
        versions: out,
    }
}

pub async fn get() -> Json<PhpInfo> {
    let site_list = sites::scan(false).await;
    Json(info(&site_list).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn support_table() {
        assert_eq!(security_until("7.4"), Some("2022-11-28"));
        assert_eq!(security_until("8.0"), Some("2023-11-26"));
        assert_eq!(security_until("8.1"), Some("2025-12-31"));
        assert_eq!(security_until("8.2"), Some("2026-12-31"));
        assert_eq!(security_until("8.3"), Some("2027-12-31"));
        assert_eq!(security_until("8.4"), Some("2028-12-31"));
        assert_eq!(security_until("8.5"), Some("2029-12-31"));
        assert_eq!(security_until("5.6"), None);
    }

    #[test]
    fn eol_rules() {
        let today = d("2026-11-13");
        assert!(is_eol("7.4", today));
        assert!(is_eol("8.0", today));
        assert!(is_eol("8.1", today));
        assert!(!is_eol("8.2", today));
        assert!(!is_eol("8.3", today));
        assert!(is_eol("5.6", today));
        assert!(!is_eol("9.0", today));
        // Boundary: the end date itself is still supported.
        assert!(!is_eol("8.2", d("2026-12-31")));
        assert!(is_eol("8.2", d("2027-01-01")));
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("8.3"), Some((8, 3)));
        assert_eq!(parse_version("10.12"), Some((10, 12)));
        assert!(parse_version("8.10") > parse_version("8.9"));
        assert_eq!(parse_version("8"), None);
        assert_eq!(parse_version("8.3.1"), None);
        assert_eq!(parse_version("mods-available"), None);
        assert_eq!(parse_version(".3"), None);
    }

    #[test]
    fn disk_detection() {
        let dir = std::env::temp_dir().join(format!("nexacloud-php-{}", uuid::Uuid::new_v4()));
        for sub in ["8.3", "7.4", "8.10", "mods", "8.x"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("8.2"), "not a dir").unwrap();
        assert_eq!(versions_on_disk(&dir), vec!["7.4", "8.3", "8.10"]);
        assert!(versions_on_disk(Path::new("/nonexistent/php")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
