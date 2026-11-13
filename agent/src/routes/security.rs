//! Firewall, fail2ban, unattended-upgrades and SSH password auth status.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use serde::Serialize;

use crate::routes::services;
use crate::util::cmd::Cmd;

#[derive(Debug, Clone, Serialize)]
pub struct Ufw {
    pub installed: bool,
    pub active: bool,
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fail2ban {
    pub installed: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecurityInfo {
    pub ufw: Ufw,
    pub fail2ban: Fail2ban,
    pub unattended_upgrades: bool,
    pub ssh_password_auth: Option<bool>,
}

/// Parse `ufw status` output → (active, rules with whitespace collapsed).
pub fn parse_ufw_status(out: &str) -> (bool, Vec<String>) {
    let mut active = false;
    let mut rules = Vec::new();
    let mut in_rules = false;
    for line in out.lines() {
        let t = line.trim();
        if let Some(s) = t.strip_prefix("Status:") {
            active = s.trim() == "active";
            continue;
        }
        if t.starts_with("--") {
            in_rules = true;
            continue;
        }
        if in_rules && !t.is_empty() {
            rules.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    (active, rules)
}

async fn ufw() -> Ufw {
    let out = Cmd::new("ufw")
        .arg("status")
        .timeout(Duration::from_secs(15))
        .run()
        .await;
    if out.missing {
        // Binary not on PATH; fall back to the systemd unit.
        let st = services::unit_state("ufw").await;
        return Ufw {
            installed: st.installed,
            active: false,
            rules: Vec::new(),
        };
    }
    if !out.ok {
        return Ufw {
            installed: true,
            active: false,
            rules: Vec::new(),
        };
    }
    let (active, rules) = parse_ufw_status(&out.stdout);
    Ufw {
        installed: true,
        active,
        rules,
    }
}

/// `APT::Periodic::Unattended-Upgrade "1";` — last occurrence wins.
pub fn apt_unattended_setting(content: &str) -> Option<bool> {
    let mut val = None;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("APT::Periodic::Unattended-Upgrade") {
            let v: String = rest
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != ';')
                .collect();
            val = Some(
                matches!(v.as_str(), "1" | "true" | "yes")
                    || v.parse::<u32>().map(|n| n > 0).unwrap_or(false),
            );
        }
    }
    val
}

fn unattended_upgrades() -> bool {
    if !Path::new("/usr/bin/unattended-upgrade").exists() {
        return false;
    }
    let Ok(rd) = std::fs::read_dir("/etc/apt/apt.conf.d") else {
        return false;
    };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    let mut val = None;
    for f in files {
        if let Ok(s) = std::fs::read_to_string(&f) {
            if let Some(v) = apt_unattended_setting(&s) {
                val = Some(v);
            }
        }
    }
    val.unwrap_or(false)
}

/// `sshd -T` output → value of `passwordauthentication`.
pub fn parse_sshd_t(out: &str) -> Option<bool> {
    out.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()? == "passwordauthentication" {
            match it.next()? {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            }
        } else {
            None
        }
    })
}

/// Minimal sshd_config evaluation: first `PasswordAuthentication` outside a
/// `Match` block wins; `Include` is expanded via `load_include` (returns the
/// contents of the files the pattern matches, in order). None = not set.
pub fn sshd_config_password_auth(
    content: &str,
    load_include: &mut dyn FnMut(&str) -> Vec<String>,
    depth: usize,
) -> Option<bool> {
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.splitn(2, |c: char| c.is_whitespace() || c == '=');
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let val = parts
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('=')
            .trim();
        match key.as_str() {
            "match" => return None,
            "passwordauthentication" => {
                return match val.to_ascii_lowercase().as_str() {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                }
            }
            "include" if depth < 8 => {
                for pat in val.split_whitespace() {
                    for inc in load_include(pat) {
                        if let Some(v) = sshd_config_password_auth(&inc, load_include, depth + 1) {
                            return Some(v);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Very small glob: `*` allowed in the last path component only.
fn glob_files(pattern: &str) -> Vec<PathBuf> {
    let p = if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        Path::new("/etc/ssh").join(pattern)
    };
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains('*') {
        return if p.is_file() { vec![p] } else { Vec::new() };
    }
    let dir = p.parent().unwrap_or(Path::new("/"));
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|f| {
            f.file_name()
                .map(|n| wildcard_match(&name, &n.to_string_lossy()))
                .unwrap_or(false)
                && f.is_file()
        })
        .collect();
    v.sort();
    v
}

pub fn wildcard_match(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(idx) => rest = &rest[idx + part.len()..],
                None => return false,
            }
        }
    }
    true
}

async fn ssh_password_auth() -> Option<bool> {
    let mut out = Cmd::new("sshd")
        .arg("-T")
        .timeout(Duration::from_secs(10))
        .run()
        .await;
    if out.missing {
        out = Cmd::new("/usr/sbin/sshd")
            .arg("-T")
            .timeout(Duration::from_secs(10))
            .run()
            .await;
    }
    if out.ok {
        if let Some(v) = parse_sshd_t(&out.stdout) {
            return Some(v);
        }
    }
    // Fallback: evaluate the config files ourselves.
    tokio::task::spawn_blocking(|| {
        let main = std::fs::read_to_string("/etc/ssh/sshd_config").ok()?;
        let mut loader = |pat: &str| -> Vec<String> {
            glob_files(pat)
                .into_iter()
                .filter_map(|f| std::fs::read_to_string(f).ok())
                .collect()
        };
        // OpenSSH default is `yes`.
        Some(sshd_config_password_auth(&main, &mut loader, 0).unwrap_or(true))
    })
    .await
    .ok()
    .flatten()
}

pub async fn info() -> SecurityInfo {
    let (ufw, f2b, ssh) =
        tokio::join!(ufw(), services::unit_state("fail2ban"), ssh_password_auth());
    let unattended = tokio::task::spawn_blocking(unattended_upgrades)
        .await
        .unwrap_or(false);
    SecurityInfo {
        ufw,
        fail2ban: Fail2ban {
            installed: f2b.installed,
            active: f2b.active,
        },
        unattended_upgrades: unattended,
        ssh_password_auth: ssh,
    }
}

pub async fn get() -> Json<SecurityInfo> {
    Json(info().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ufw_parsing() {
        let out = "Status: active\n\nTo                         Action      From\n--                         ------      ----\n22/tcp                     ALLOW       Anywhere\n8443/tcp                   ALLOW       Anywhere\n22/tcp (v6)                ALLOW       Anywhere (v6)\n\n";
        let (active, rules) = parse_ufw_status(out);
        assert!(active);
        assert_eq!(
            rules,
            vec![
                "22/tcp ALLOW Anywhere",
                "8443/tcp ALLOW Anywhere",
                "22/tcp (v6) ALLOW Anywhere (v6)"
            ]
        );
        let (active, rules) = parse_ufw_status("Status: inactive\n");
        assert!(!active);
        assert!(rules.is_empty());
    }

    #[test]
    fn apt_parsing() {
        let s = "APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n";
        assert_eq!(apt_unattended_setting(s), Some(true));
        assert_eq!(
            apt_unattended_setting("APT::Periodic::Unattended-Upgrade \"0\";"),
            Some(false)
        );
        assert_eq!(
            apt_unattended_setting("// APT::Periodic::Unattended-Upgrade \"1\";"),
            None
        );
        assert_eq!(apt_unattended_setting(""), None);
    }

    #[test]
    fn sshd_t_parsing() {
        assert_eq!(
            parse_sshd_t("port 22\npasswordauthentication yes\n"),
            Some(true)
        );
        assert_eq!(parse_sshd_t("passwordauthentication no\n"), Some(false));
        assert_eq!(parse_sshd_t("port 22\n"), None);
    }

    #[test]
    fn sshd_config_first_match_wins_with_includes() {
        let main = "Include /etc/ssh/sshd_config.d/*.conf\n#PasswordAuthentication yes\nPasswordAuthentication yes\n";
        let mut no_includes = |_: &str| Vec::new();
        assert_eq!(
            sshd_config_password_auth(main, &mut no_includes, 0),
            Some(true)
        );
        let mut cloud_init = |_: &str| vec!["PasswordAuthentication no\n".to_string()];
        assert_eq!(
            sshd_config_password_auth(main, &mut cloud_init, 0),
            Some(false)
        );
        let m = "Match User bob\n  PasswordAuthentication yes\n";
        assert_eq!(sshd_config_password_auth(m, &mut no_includes, 0), None);
        assert_eq!(
            sshd_config_password_auth("passwordauthentication=no", &mut no_includes, 0),
            Some(false)
        );
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.conf", "50-cloud-init.conf"));
        assert!(!wildcard_match("*.conf", "x.conf.bak"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
    }
}
