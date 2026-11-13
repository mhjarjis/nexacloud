//! Read-only scan of nginx site configs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::nginx::{self, ServerBlock};
use crate::util::cmd::Cmd;
use crate::util::fmt_ts;

const SITES_ENABLED: &str = "/etc/nginx/sites-enabled";
const CONF_D: &str = "/etc/nginx/conf.d";
const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct GitInfo {
    pub branch: String,
    pub commit: String,
    pub message: String,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub id: String,
    pub server_names: Vec<String>,
    pub root: Option<String>,
    pub kind: String,
    pub php_version: Option<String>,
    pub ssl: bool,
    pub ssl_expires: Option<String>,
    pub proxy_pass: Option<String>,
    pub config_path: String,
    pub git: Option<GitInfo>,
}

/// Config files nginx would load: everything in sites-enabled (symlinks
/// followed) and conf.d/*.conf.
fn config_files(sites_enabled: &Path, conf_d: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(sites_enabled) {
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        files.extend(v);
    }
    if let Ok(rd) = std::fs::read_dir(conf_d) {
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
            .collect();
        v.sort();
        files.extend(v);
    }
    // `metadata` follows symlinks; broken links and directories are skipped.
    files
        .into_iter()
        .filter(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.len() <= MAX_CONFIG_BYTES)
                .unwrap_or(false)
        })
        .collect()
}

pub fn read_blocks(sites_enabled: &Path, conf_d: &Path) -> Vec<ServerBlock> {
    let mut blocks = Vec::new();
    for f in config_files(sites_enabled, conf_d) {
        match std::fs::read(&f) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                blocks.extend(nginx::server_blocks(&text, &f));
            }
            Err(e) => tracing::debug!("cannot read {}: {e}", f.display()),
        }
    }
    nginx::merge_blocks(blocks)
}

/// notAfter of the first certificate in a PEM file.
pub fn cert_not_after(pem_bytes: &[u8]) -> Option<DateTime<Utc>> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem_bytes).ok()?;
    let cert = pem.parse_x509().ok()?;
    DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)
}

/// php version from a fastcgi_pass value; resolves a unix socket symlink
/// such as /run/php/php-fpm.sock → php8.3-fpm.sock when needed.
fn php_version(fastcgi_pass: &str) -> Option<String> {
    if let Some(v) = nginx::php_version_from_socket(fastcgi_pass) {
        return Some(v);
    }
    let path = fastcgi_pass.strip_prefix("unix:")?;
    let target = std::fs::canonicalize(path).ok()?;
    nginx::php_version_from_socket(&target.to_string_lossy())
}

/// Remove `user:password@` from URL-style git remotes.
pub fn redact_remote(remote: &str) -> String {
    if let Some((scheme, rest)) = remote.split_once("://") {
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if let Some(at) = authority.rfind('@') {
            let host = &authority[at + 1..];
            let user = &authority[..at];
            let user = user.split(':').next().unwrap_or("");
            // Keep a bare username like `git@`, drop anything with a secret.
            if authority[..at].contains(':') || scheme.starts_with("http") {
                return format!("{scheme}://{host}{path}");
            }
            return format!("{scheme}://{user}@{host}{path}");
        }
    }
    remote.to_string()
}

fn git(dir: &Path) -> Cmd {
    Cmd::new("git")
        .args([
            "-c",
            "safe.directory=*",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "log.showSignature=false",
            "--no-optional-locks",
            "-C",
        ])
        .arg(dir.to_string_lossy())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .timeout(Duration::from_secs(10))
}

async fn git_info(dir: &Path) -> Option<GitInfo> {
    if !dir.join(".git").exists() {
        return None;
    }
    let (branch, log, remote) = tokio::join!(
        git(dir).args(["rev-parse", "--abbrev-ref", "HEAD"]).run(),
        git(dir).args(["log", "-1", "--format=%h%x1f%s"]).run(),
        git(dir).args(["remote", "get-url", "origin"]).run(),
    );
    if !branch.ok {
        return None;
    }
    let (commit, message) = if log.ok {
        let line = log.stdout.trim_end_matches(['\n', '\r']);
        match line.split_once('\x1f') {
            Some((c, m)) => (c.to_string(), m.to_string()),
            None => (line.to_string(), String::new()),
        }
    } else {
        (String::new(), String::new())
    };
    let remote = if remote.ok {
        let r = remote.stdout.trim();
        (!r.is_empty()).then(|| redact_remote(r))
    } else {
        None
    };
    Some(GitInfo {
        branch: branch.stdout.trim().to_string(),
        commit,
        message,
        remote,
    })
}

async fn build_site(b: ServerBlock, with_git: bool) -> Site {
    let root_path = b.root.as_ref().map(PathBuf::from);
    let artisan = root_path
        .as_ref()
        .and_then(|r| r.parent().map(|p| p.join("artisan")))
        .map(|p| p.is_file())
        .unwrap_or(false);
    let kind = nginx::site_kind(
        b.root.is_some(),
        artisan,
        b.fastcgi_pass.is_some(),
        b.proxy_pass.is_some(),
    );
    let php_version = b.fastcgi_pass.as_deref().and_then(php_version);
    let ssl_expires = match &b.ssl_certificate {
        Some(p) => tokio::fs::read(p)
            .await
            .ok()
            .and_then(|bytes| cert_not_after(&bytes))
            .map(fmt_ts),
        None => None,
    };
    let git = if with_git {
        let dir = match (&root_path, kind) {
            (Some(r), "laravel") => r.parent().map(Path::to_path_buf),
            (Some(r), _) => Some(r.clone()),
            _ => None,
        };
        match dir {
            Some(d) => git_info(&d).await,
            None => None,
        }
    } else {
        None
    };
    Site {
        id: b.id().to_string(),
        server_names: b.server_names.clone(),
        root: b.root.clone(),
        kind: kind.to_string(),
        php_version,
        ssl: b.ssl(),
        ssl_expires,
        proxy_pass: b.proxy_pass.clone(),
        config_path: b.config_path.to_string_lossy().into_owned(),
        git,
    }
}

pub async fn scan(with_git: bool) -> Vec<Site> {
    let blocks = tokio::task::spawn_blocking(|| {
        read_blocks(Path::new(SITES_ENABLED), Path::new(CONF_D))
    })
    .await
    .unwrap_or_default();
    let futs = blocks
        .into_iter()
        .map(|b| build_site(b, with_git))
        .collect();
    let mut sites: Vec<Site> = crate::util::join_all(futs).await.into_iter().flatten().collect();
    sites.sort_by(|a, b| a.id.cmp(&b.id));
    sites
}

pub async fn list() -> Json<Vec<Site>> {
    Json(scan(true).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_are_redacted() {
        assert_eq!(redact_remote("git@github.com:org/shop.git"), "git@github.com:org/shop.git");
        assert_eq!(
            redact_remote("https://user:ghp_secret@github.com/org/shop.git"),
            "https://github.com/org/shop.git"
        );
        assert_eq!(
            redact_remote("https://oauth2token@gitlab.com/org/shop.git"),
            "https://gitlab.com/org/shop.git"
        );
        assert_eq!(redact_remote("ssh://git@host:22/repo.git"), "ssh://git@host:22/repo.git");
        assert_eq!(redact_remote("https://github.com/org/shop.git"), "https://github.com/org/shop.git");
    }

    #[test]
    fn scans_directory_with_symlinks() {
        let base = std::env::temp_dir().join(format!("nexacloud-nginx-{}", uuid::Uuid::new_v4()));
        let avail = base.join("sites-available");
        let enabled = base.join("sites-enabled");
        let confd = base.join("conf.d");
        for d in [&avail, &enabled, &confd] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            avail.join("a"),
            "server { listen 80; server_name a.test; root /srv/a; }",
        )
        .unwrap();
        std::os::unix::fs::symlink(avail.join("a"), enabled.join("a")).unwrap();
        std::os::unix::fs::symlink(avail.join("missing"), enabled.join("broken")).unwrap();
        std::fs::write(confd.join("b.conf"), "server { server_name b.test; proxy_pass http://x; }").unwrap();
        std::fs::write(confd.join("ignored.txt"), "server { server_name c.test; }").unwrap();
        let blocks = read_blocks(&enabled, &confd);
        let ids: Vec<&str> = blocks.iter().map(|b| b.id()).collect();
        assert_eq!(ids, vec!["a.test", "b.test"]);
        assert_eq!(blocks[0].config_path, enabled.join("a"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_cert_expiry() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["x.test".to_string()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2030, 5, 17);
        let cert = params.self_signed(&key).unwrap();
        let exp = cert_not_after(cert.pem().as_bytes()).unwrap();
        assert_eq!(fmt_ts(exp), "2030-05-17T00:00:00Z");
        assert!(cert_not_after(b"garbage").is_none());
    }
}
