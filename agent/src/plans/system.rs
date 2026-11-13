//! PHP and service plans: install a PHP version, extensions, php.ini drop-in,
//! CLI default, and the one-click service catalog.

use serde::{Deserialize, Serialize};

use crate::changes::{ChangeEnv, ChangePlan, ChangePlanBuilder};
use crate::plans::sites::valid_php_version;
use crate::routes::php::versions_on_disk;

const APT_TIMEOUT: u64 = 1800;
pub const INI_DROPIN: &str = "99-nexacloud.ini";

/// Extensions installed with every new PHP version (Laravel's needs).
pub const DEFAULT_EXTENSIONS: &[&str] = &[
    "cli", "fpm", "common", "mbstring", "xml", "curl", "zip", "bcmath", "intl", "mysql", "pgsql",
    "sqlite3", "gd", "redis", "opcache", "readline",
];

/// Extensions offered in the UI (apt package suffix).
pub const OPTIONAL_EXTENSIONS: &[&str] = &[
    "mbstring",
    "xml",
    "curl",
    "zip",
    "bcmath",
    "intl",
    "mysql",
    "pgsql",
    "sqlite3",
    "gd",
    "redis",
    "imagick",
    "soap",
    "ldap",
    "memcached",
    "mongodb",
    "xdebug",
    "swoole",
    "igbinary",
    "msgpack",
    "gmp",
    "imap",
    "tidy",
    "xsl",
];

/// php.ini keys the panel may set, with a validator.
pub const INI_KEYS: &[(&str, &str)] = &[
    ("memory_limit", "size"),
    ("upload_max_filesize", "size"),
    ("post_max_size", "size"),
    ("max_execution_time", "int"),
    ("max_input_time", "int"),
    ("max_input_vars", "int"),
    ("display_errors", "onoff"),
    ("date.timezone", "tz"),
    ("opcache.enable", "bool"),
    ("opcache.memory_consumption", "int"),
    ("opcache.interned_strings_buffer", "int"),
    ("opcache.max_accelerated_files", "int"),
    ("opcache.validate_timestamps", "bool"),
    ("opcache.revalidate_freq", "int"),
];

fn valid_ini_value(kind: &str, v: &str) -> bool {
    let v = v.trim();
    match kind {
        "size" => {
            !v.is_empty()
                && v.len() <= 8
                && (v == "-1"
                    || (v[..v.len() - 1].bytes().all(|b| b.is_ascii_digit())
                        && v.as_bytes()[v.len() - 1].is_ascii_alphanumeric()))
        }
        "int" => {
            !v.is_empty() && v.len() <= 9 && (v == "-1" || v.bytes().all(|b| b.is_ascii_digit()))
        }
        "onoff" => matches!(v, "On" | "Off" | "on" | "off" | "0" | "1"),
        "bool" => matches!(v, "0" | "1"),
        "tz" => {
            v.len() < 40
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-+".contains(&b))
        }
        _ => false,
    }
}

fn apt(b: ChangePlanBuilder, packages: &[String]) -> ChangePlanBuilder {
    let mut argv = vec![
        "apt-get".to_string(),
        "install".into(),
        "-y".into(),
        "-q".into(),
        "-o".into(),
        "Dpkg::Options::=--force-confold".into(),
    ];
    argv.extend(packages.iter().cloned());
    b.lock("apt")
        .pre_command_timeout(["apt-get", "update", "-q"], 600)
        .pre_command_timeout(argv, APT_TIMEOUT)
}

pub fn installed_php(env: &ChangeEnv) -> Vec<String> {
    versions_on_disk(&env.host_path("/etc/php"))
}

fn ppa_present(env: &ChangeEnv) -> bool {
    let dir = env.host_path("/etc/apt/sources.list.d");
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.contains("ondrej") && n.contains("php")
            })
        })
        .unwrap_or(false)
}

// ------------------------------------------------------------------ PHP

pub fn plan_php_install(env: &ChangeEnv, version: &str) -> Result<ChangePlan, String> {
    if !valid_php_version(version)
        || !matches!(
            version,
            "7.4" | "8.0" | "8.1" | "8.2" | "8.3" | "8.4" | "8.5"
        )
    {
        return Err("choose a PHP version between 7.4 and 8.5".into());
    }
    if env.host_path(&format!("/etc/php/{version}/fpm")).is_dir() {
        return Err(format!("PHP {version} FPM is already installed"));
    }
    let mut b = ChangePlan::builder(
        "php_install",
        &format!("Install PHP {version}"),
        &format!("php:{version}"),
    );
    if !ppa_present(env) {
        b = b
            .lock("apt")
            .pre_command_timeout(
                [
                    "apt-get",
                    "install",
                    "-y",
                    "-q",
                    "software-properties-common",
                    "ca-certificates",
                ],
                APT_TIMEOUT,
            )
            .pre_command_timeout(["add-apt-repository", "-y", "ppa:ondrej/php"], 600);
    }
    let pkgs: Vec<String> = DEFAULT_EXTENSIONS
        .iter()
        .map(|e| format!("php{version}-{e}"))
        .collect();
    b = apt(b, &pkgs)
        .command(["systemctl", "enable", "--now", &format!("php{version}-fpm")])
        .verify_command(["systemctl", "is-active", "--quiet", &format!("php{version}-fpm")])
        .verify_command([&format!("php{version}"), "-v"])
        .note(format!(
            "Installs PHP {version} with FPM and common extensions ({}) from the ondrej/php PPA. Existing PHP versions and sites are not changed.",
            DEFAULT_EXTENSIONS.join(", ")
        ));
    b.build(env).map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtParams {
    pub version: String,
    pub extensions: Vec<String>,
}

pub fn plan_php_extensions(env: &ChangeEnv, p: &ExtParams) -> Result<ChangePlan, String> {
    if !installed_php(env).contains(&p.version) {
        return Err(format!("PHP {} is not installed", p.version));
    }
    if p.extensions.is_empty() || p.extensions.len() > 20 {
        return Err("choose 1–20 extensions".into());
    }
    for e in &p.extensions {
        if !OPTIONAL_EXTENSIONS.contains(&e.as_str()) {
            return Err(format!("unknown extension {e:?}"));
        }
    }
    let v = &p.version;
    let pkgs: Vec<String> = p.extensions.iter().map(|e| format!("php{v}-{e}")).collect();
    let b = ChangePlan::builder(
        "php_extensions",
        &format!("Install PHP {v} extensions: {}", p.extensions.join(", ")),
        &format!("php:{v}"),
    );
    apt(b, &pkgs)
        .command(["systemctl", "reload-or-restart", &format!("php{v}-fpm")])
        .verify_command(["systemctl", "is-active", "--quiet", &format!("php{v}-fpm")])
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct IniParams {
    pub version: String,
    /// key → value; an empty value removes the key from the drop-in.
    pub settings: std::collections::BTreeMap<String, String>,
}

pub fn ini_path(version: &str, sapi: &str) -> String {
    format!("/etc/php/{version}/{sapi}/conf.d/{INI_DROPIN}")
}

/// Current values in our drop-in.
pub fn read_ini(env: &ChangeEnv, version: &str) -> std::collections::BTreeMap<String, String> {
    let text =
        std::fs::read_to_string(env.host_path(&ini_path(version, "fpm"))).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim_start().starts_with(';'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

pub fn render_ini(settings: &std::collections::BTreeMap<String, String>) -> String {
    let mut s =
        String::from("; Managed by NexaCloud (PHP settings page). Remove this file to undo.\n");
    for (k, v) in settings {
        s.push_str(&format!("{k} = {v}\n"));
    }
    s
}

pub fn plan_php_ini(env: &ChangeEnv, p: &IniParams) -> Result<ChangePlan, String> {
    let v = &p.version;
    if !installed_php(env).contains(v) {
        return Err(format!("PHP {v} is not installed"));
    }
    let mut merged = read_ini(env, v);
    for (k, val) in &p.settings {
        let kind = INI_KEYS
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, t)| *t)
            .ok_or_else(|| format!("{k} cannot be set from the panel"))?;
        if val.trim().is_empty() {
            merged.remove(k);
        } else if !valid_ini_value(kind, val) {
            return Err(format!("invalid value for {k}: {val:?}"));
        } else {
            merged.insert(k.clone(), val.trim().to_string());
        }
    }
    let content = render_ini(&merged);
    let mut b = ChangePlan::builder(
        "php_ini",
        &format!("Update PHP {v} settings"),
        &format!("php:{v}"),
    )
    .lock(&format!("php:{v}"));
    for sapi in ["fpm", "cli"] {
        if env
            .host_path(&format!("/etc/php/{v}/{sapi}/conf.d"))
            .is_dir()
        {
            b = b.write_file(&ini_path(v, sapi), content.clone());
        }
    }
    b.command([&format!("php-fpm{v}"), "-t"])
        .command(["systemctl", "reload", &format!("php{v}-fpm")])
        .verify_command(["systemctl", "is-active", "--quiet", &format!("php{v}-fpm")])
        .on_revert(["systemctl", "reload", &format!("php{v}-fpm")])
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

pub fn plan_php_cli(env: &ChangeEnv, version: &str) -> Result<ChangePlan, String> {
    if !env.host_path(&format!("/usr/bin/php{version}")).exists() {
        return Err(format!("PHP {version} CLI is not installed"));
    }
    ChangePlan::builder(
        "php_cli",
        &format!("Make PHP {version} the command-line default"),
        "php:cli",
    )
    .command([
        "update-alternatives",
        "--set",
        "php",
        &format!("/usr/bin/php{version}"),
    ])
    .verify_command([
        "sh",
        "-c",
        "php -r 'echo PHP_MAJOR_VERSION.\".\".PHP_MINOR_VERSION;' | grep -qx \"$1\"",
        "check",
        version,
    ])
    .build(env)
    .map_err(|e| format!("{e:#}"))
}

// ------------------------------------------------------------------ catalog

#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub packages: &'static [&'static str],
    pub unit: Option<&'static str>,
    /// Binary whose presence means "installed".
    pub probe: &'static str,
    pub installed: bool,
}

/// (id, label, description, apt packages, systemd unit, probe binary)
type CatalogEntry = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<&'static str>,
    &'static str,
);

pub const CATALOG: &[CatalogEntry] = &[
    (
        "redis",
        "Redis",
        "In-memory cache and queue backend for Laravel (CACHE_STORE / QUEUE_CONNECTION=redis).",
        &["redis-server"],
        Some("redis-server"),
        "/usr/bin/redis-server",
    ),
    (
        "supervisor",
        "Supervisor",
        "Keeps Laravel queue workers running.",
        &["supervisor"],
        Some("supervisor"),
        "/usr/bin/supervisord",
    ),
    (
        "fail2ban",
        "Fail2ban",
        "Bans IPs that keep failing SSH logins. Installed with the sshd jail on.",
        &["fail2ban"],
        Some("fail2ban"),
        "/usr/bin/fail2ban-client",
    ),
    (
        "ufw",
        "UFW firewall",
        "Installs the firewall tool only — turning it on is a separate, guarded step.",
        &["ufw"],
        None,
        "/usr/sbin/ufw",
    ),
    (
        "unattended-upgrades",
        "Automatic security updates",
        "Installs Ubuntu security patches automatically (no automatic reboot).",
        &["unattended-upgrades"],
        None,
        "/usr/bin/unattended-upgrade",
    ),
    (
        "certbot",
        "Certbot",
        "Let's Encrypt client used for free SSL certificates.",
        &["certbot"],
        None,
        "/usr/bin/certbot",
    ),
    (
        "composer",
        "Composer",
        "PHP dependency manager (needed for deploys).",
        &["composer"],
        None,
        "/usr/bin/composer",
    ),
    (
        "nodejs",
        "Node.js + npm",
        "Builds front-end assets (npm run build) during deploys.",
        &["nodejs", "npm"],
        None,
        "/usr/bin/node",
    ),
    (
        "git",
        "Git",
        "Needed for deploys from GitHub.",
        &["git"],
        None,
        "/usr/bin/git",
    ),
    (
        "rclone",
        "rclone",
        "Copies backups to Synology, S3, SFTP and more.",
        &["rclone"],
        None,
        "/usr/bin/rclone",
    ),
    (
        "clamav",
        "ClamAV",
        "Malware scanner for uploaded files (nightly scan of /var/www).",
        &["clamav"],
        None,
        "/usr/bin/clamscan",
    ),
    (
        "postgresql",
        "PostgreSQL",
        "PostgreSQL database server.",
        &["postgresql"],
        Some("postgresql"),
        "/usr/bin/psql",
    ),
    (
        "mysql",
        "MySQL",
        "MySQL database server.",
        &["mysql-server"],
        Some("mysql"),
        "/usr/sbin/mysqld",
    ),
];

pub fn catalog(env: &ChangeEnv) -> Vec<CatalogItem> {
    CATALOG
        .iter()
        .map(
            |(id, label, description, packages, unit, probe)| CatalogItem {
                id,
                label,
                description,
                packages,
                unit: *unit,
                probe,
                installed: env.host_path(probe).exists(),
            },
        )
        .collect()
}

pub fn plan_service_install(env: &ChangeEnv, id: &str) -> Result<ChangePlan, String> {
    let item = CATALOG
        .iter()
        .find(|c| c.0 == id)
        .ok_or_else(|| format!("unknown service {id:?}"))?;
    let (id, label, _, packages, unit, probe) = *item;
    if env.host_path(probe).exists() {
        return Err(format!("{label} is already installed"));
    }
    let pkgs: Vec<String> = packages.iter().map(|s| s.to_string()).collect();
    let mut b = apt(
        ChangePlan::builder(
            "service_install",
            &format!("Install {label}"),
            &format!("service:{id}"),
        ),
        &pkgs,
    );
    match id {
        "fail2ban" => {
            b = b.write_file(
                "/etc/fail2ban/jail.d/nexacloud.local",
                "# Managed by NexaCloud\n[sshd]\nenabled = true\nmaxretry = 5\nfindtime = 10m\nbantime = 1h\n",
            );
        }
        "unattended-upgrades" => {
            b = b.write_file(
                "/etc/apt/apt.conf.d/20auto-upgrades",
                "APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n",
            );
        }
        _ => {}
    }
    if let Some(u) = unit {
        b = b
            .command(["systemctl", "enable", "--now", u])
            .verify_command(["systemctl", "is-active", "--quiet", u]);
        if id == "fail2ban" {
            b = b.command(["systemctl", "restart", "fail2ban"]);
        }
    } else {
        b = b.verify_command(["test", "-x", probe]);
    }
    b.create_parents()
        .note(format!("Runs apt-get install {}.", packages.join(" ")))
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_validation() {
        assert!(valid_ini_value("size", "512M"));
        assert!(valid_ini_value("size", "-1"));
        assert!(!valid_ini_value("size", "512M\nextension=evil.so"));
        assert!(valid_ini_value("int", "60"));
        assert!(!valid_ini_value("int", "6 0"));
        assert!(valid_ini_value("tz", "Europe/Amsterdam"));
        assert!(!valid_ini_value("tz", "a\nb"));
    }

    #[test]
    fn ini_render_parse_roundtrip() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("memory_limit".to_string(), "512M".to_string());
        let r = render_ini(&m);
        assert!(r.contains("memory_limit = 512M"));
    }
}
