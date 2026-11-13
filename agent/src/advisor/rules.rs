//! Advisor rules: inspect the server, report findings with evidence and a
//! recommended value, and offer a fix plan through the change pipeline.
//!
//! Budget for the reference box (8 GB / 2 vCPU, see docs/contracts/advisor.md):
//! OS+panel 0.6 GB, MySQL ~2.2 GB (buffer pool 1.5 GB), PHP-FPM ~2.5 GB,
//! Redis 0.25 GB, build headroom 1.5 GB, rest page cache. Values scale with RAM.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::advisor::nginx_hardening;
use crate::changes::{ChangeEnv, ChangePlan};
use crate::db::Db;
use crate::util::cmd::Cmd;

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub id: String,
    /// critical | warning | info
    pub severity: &'static str,
    pub category: &'static str,
    pub title: String,
    pub detail: String,
    pub current: Option<String>,
    pub recommended: Option<String>,
    /// safe (one click) | restart (a service blips) | manual (link only)
    pub tier: &'static str,
    /// When set, `POST /api/plans/preview {"kind":"advisor_fix","params":{"rule":id}}` works.
    pub fixable: bool,
    pub link: Option<String>,
    pub dismissed_until: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub score: u32,
    pub findings: Vec<Finding>,
    pub dismissed: Vec<Finding>,
    pub ram_bytes: u64,
}

const DISMISS_KEY: &str = "advisor.dismissed";

fn mem_total() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|k| k.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn swap_total() -> u64 {
    std::fs::read_to_string("/proc/swaps")
        .map(|s| {
            s.lines()
                .skip(1)
                .filter_map(|l| l.split_whitespace().nth(2)?.parse::<u64>().ok())
                .sum::<u64>()
                * 1024
        })
        .unwrap_or(0)
}

const GB: u64 = 1 << 30;

/// Recommended InnoDB buffer pool in MB (~19% of RAM, min 128M, max 8G).
pub fn buffer_pool_mb(ram: u64) -> u64 {
    let mb = ram / (1 << 20) * 19 / 100;
    let mb = (mb / 128).max(1) * 128;
    mb.clamp(128, 8192)
}

async fn run(argv: &[&str]) -> Option<String> {
    let mut c = Cmd::new(argv[0]).timeout(std::time::Duration::from_secs(20));
    for a in &argv[1..] {
        c = c.arg(a);
    }
    let o = c.run().await;
    o.ok.then_some(o.stdout)
}

fn finding(
    id: &str,
    severity: &'static str,
    category: &'static str,
    title: impl Into<String>,
    detail: impl Into<String>,
) -> Finding {
    Finding {
        id: id.to_string(),
        severity,
        category,
        title: title.into(),
        detail: detail.into(),
        current: None,
        recommended: None,
        tier: "manual",
        fixable: false,
        link: None,
        dismissed_until: None,
    }
}

impl Finding {
    fn cur(mut self, c: impl Into<String>) -> Self {
        self.current = Some(c.into());
        self
    }
    fn rec(mut self, r: impl Into<String>) -> Self {
        self.recommended = Some(r.into());
        self
    }
    fn fix(mut self, tier: &'static str) -> Self {
        self.tier = tier;
        self.fixable = true;
        self
    }
    fn link(mut self, l: &str) -> Self {
        self.link = Some(l.into());
        self
    }
}

/// Laravel project dirs from the nginx scan (root/.. containing artisan).
async fn laravel_dirs() -> Vec<(String, String)> {
    crate::routes::sites::scan(false)
        .await
        .into_iter()
        .filter(|s| s.kind == "laravel")
        .filter_map(|s| {
            let root = s.root?;
            let dir = std::path::Path::new(&root)
                .parent()?
                .to_string_lossy()
                .into_owned();
            Some((s.id, dir))
        })
        .collect()
}

fn env_value(env_text: &str, key: &str) -> Option<String> {
    env_text.lines().find_map(|l| {
        let l = l.trim();
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}

pub async fn evaluate(env: &ChangeEnv, db: &Db) -> Report {
    let mut out: Vec<Finding> = Vec::new();
    let ram = mem_total();

    // nginx: version hiding (Phase 0 change) + gzip.
    if let Ok(p) = nginx_hardening::plan(env).await {
        if !p.is_noop() {
            out.push(
                finding("nginx-hardening", "info", "nginx", "nginx shows its version number", "Error pages and the Server header reveal the exact nginx version, which helps attackers pick exploits.")
                    .cur("server_tokens on").rec("server_tokens off").fix("safe"),
            );
        }
    }
    if let Some(t) = run(&["nginx", "-T"]).await {
        let a = nginx_hardening::analyse(&t);
        if !a.http_level.contains_key("gzip_types") && !std::path::Path::new(GZIP_DROPIN).exists() {
            out.push(
                finding("nginx-gzip", "warning", "nginx", "Compression only covers HTML", "nginx compresses text/html by default; CSS, JavaScript, JSON and SVG are sent uncompressed (2–5× larger).")
                    .cur("gzip_types not set").rec("gzip for css, js, json, svg, xml (level 5)").fix("safe"),
            );
        }
    }

    // Swap.
    if ram > 0 && ram <= 16 * GB && swap_total() == 0 {
        out.push(
            finding("swap", "warning", "memory", "No swap space", "Without swap, a memory spike (a big composer install or npm build) makes the kernel kill MySQL or PHP. A small swapfile absorbs spikes; swappiness 10 keeps it for emergencies.")
                .cur("0 MB").rec("2 GB swapfile, vm.swappiness=10").fix("safe"),
        );
    }

    // PHP versions: EOL + tuning drop-in.
    let today = Utc::now().date_naive();
    let sites = crate::routes::sites::scan(false).await;
    for v in crate::plans::system::installed_php(env) {
        let used: Vec<String> = sites
            .iter()
            .filter(|s| s.php_version.as_deref() == Some(v.as_str()))
            .map(|s| s.id.clone())
            .collect();
        if crate::routes::php::is_eol(&v, today) && !used.is_empty() {
            out.push(
                finding(
                    &format!("php-eol-{v}"),
                    "critical",
                    "php",
                    format!("PHP {v} no longer gets security fixes"),
                    format!(
                        "Used by: {}. Switch these sites to PHP 8.4 (install it first).",
                        used.join(", ")
                    ),
                )
                .cur(format!("PHP {v}"))
                .rec("PHP 8.4")
                .link("/php"),
            );
        }
        if env.host_path(&format!("/etc/php/{v}/fpm/conf.d")).is_dir()
            && !env.host_path(&php_tuning_path(&v)).exists()
        {
            out.push(
                finding(&format!("php-tuning-{v}"), "warning", "php", format!("PHP {v} uses default OPcache and upload limits"), "Default OPcache (128 MB, 10k files) is too small for several Laravel apps, and the 2 MB upload limit breaks image uploads.")
                    .cur("opcache 128M · upload 2M").rec("opcache 256M/20k files · upload 64M · memory 256M").fix("restart"),
            );
        }
    }

    // MySQL.
    if let Some(v) = run(&[
        "mysql",
        "--batch",
        "--skip-column-names",
        "-e",
        "SELECT @@innodb_buffer_pool_size",
    ])
    .await
    {
        let cur: u64 = v.trim().parse().unwrap_or(0);
        let rec = buffer_pool_mb(ram);
        if ram >= 4 * GB
            && cur < rec * (1 << 20) / 2
            && !std::path::Path::new(MYSQL_DROPIN).exists()
        {
            out.push(
                finding("mysql-buffer-pool", "warning", "database", "MySQL cache is too small", format!("InnoDB keeps hot data in its buffer pool. With {} GB RAM the recommended size is {rec} MB.", ram / GB))
                    .cur(format!("{} MB", cur >> 20)).rec(format!("{rec} MB")).fix("restart"),
            );
        }
    }
    if let Some(ss) = run(&["ss", "-ltnH"]).await {
        for (port, name, id) in [
            ("3306", "MySQL", "mysql-bind"),
            ("5432", "PostgreSQL", "postgres-bind"),
            ("6379", "Redis", "redis-bind"),
        ] {
            let public = ss.lines().any(|l| {
                l.split_whitespace().nth(3).is_some_and(|a| {
                    a.ends_with(&format!(":{port}"))
                        && (a.starts_with("0.0.0.0") || a.starts_with("*") || a.starts_with("[::]"))
                })
            });
            if public {
                let mut f = finding(id, "critical", "security", format!("{name} listens on all network interfaces"), format!("Port {port} is reachable from the internet if no firewall blocks it. Apps on this server only need 127.0.0.1."))
                    .cur("0.0.0.0").rec("127.0.0.1");
                if id == "mysql-bind" {
                    f = f.fix("restart");
                } else {
                    f = f.link("/security");
                }
                out.push(f);
            }
        }
    }

    // Security services.
    let sec = crate::routes::security::info().await;
    if !sec.ufw.active {
        out.push(
            finding("ufw", "critical", "security", "Firewall is off", "Every open port is reachable. Turning UFW on (allow SSH, HTTP, HTTPS and the panel) is a guarded step with automatic undo if you lose access.")
                .cur(if sec.ufw.installed { "installed, inactive" } else { "not installed" }).rec("active: 22, 80, 443, 8443").link("/security"),
        );
    }
    if !sec.fail2ban.installed {
        out.push(
            finding(
                "fail2ban",
                "warning",
                "security",
                "No protection against SSH password guessing",
                "Fail2ban bans IPs after 5 failed SSH logins for an hour.",
            )
            .cur("not installed")
            .rec("fail2ban with the sshd jail")
            .fix("safe"),
        );
    }
    if !sec.unattended_upgrades {
        out.push(
            finding("unattended-upgrades", "warning", "security", "Security updates are not automatic", "Ubuntu publishes security fixes daily; unattended-upgrades installs them without rebooting.")
                .cur("off").rec("on (no automatic reboot)").fix("safe"),
        );
    }

    // journald size.
    if !std::path::Path::new(JOURNALD_DROPIN).exists() {
        let size = run(&["du", "-sb", "/var/log/journal"])
            .await
            .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0);
        if size > 400 << 20 {
            out.push(
                finding("journald-size", "info", "disk", "System logs can grow without limit", format!("The systemd journal uses {} MB. Capping it at 500 MB frees disk space automatically.", size >> 20))
                    .cur(format!("{} MB", size >> 20)).rec("SystemMaxUse=500M").fix("safe"),
            );
        }
    }

    // Laravel .env checks.
    let mut redis_wanted = Vec::new();
    for (id, dir) in laravel_dirs().await {
        let Ok(text) = std::fs::read_to_string(format!("{dir}/.env")) else {
            continue;
        };
        let debug = env_value(&text, "APP_DEBUG").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let prod = env_value(&text, "APP_ENV").is_none_or(|v| v == "production");
        if debug && prod {
            out.push(
                finding(&format!("app-debug-{id}"), "critical", "laravel", format!("{id} shows debug pages in production"), "APP_DEBUG=true prints stack traces with environment variables (database passwords, APP_KEY) to any visitor who triggers an error.")
                    .cur("APP_DEBUG=true").rec("APP_DEBUG=false").link("/sites"),
            );
        }
        if env_value(&text, "QUEUE_CONNECTION").is_some_and(|v| v == "sync" || v == "database") {
            redis_wanted.push(id);
        }
    }
    if !redis_wanted.is_empty() && !env.host_path("/usr/bin/redis-server").exists() {
        out.push(
            finding(
                "redis",
                "info",
                "laravel",
                "Queues run without Redis",
                format!(
                    "{} use the sync/database queue. Redis makes queues and cache much faster.",
                    redis_wanted.join(", ")
                ),
            )
            .cur("not installed")
            .rec("Redis (maxmemory 256 MB)")
            .fix("safe"),
        );
    }

    // Certificates.
    let now = Utc::now();
    for s in &sites {
        if let Some(exp) = s
            .ssl_expires
            .as_deref()
            .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
        {
            let days = (exp.with_timezone(&Utc) - now).num_days();
            if days < 21 {
                out.push(
                    finding(
                        &format!("cert-{}", s.id),
                        if days < 7 { "critical" } else { "warning" },
                        "ssl",
                        format!("Certificate for {} expires in {days} days", s.id),
                        "Check that certbot renewal works (systemctl status certbot.timer).",
                    )
                    .link("/sites"),
                );
            }
        }
    }

    // Backups freshness.
    if let Some(f) = crate::backups::freshness_finding(db) {
        out.push(f);
    }

    // Dismissals.
    let dismissed_map = load_dismissed(db);
    let mut findings = Vec::new();
    let mut dismissed = Vec::new();
    for mut f in out {
        if let Some(until) = dismissed_map.get(&f.id) {
            let active = until == "forever"
                || DateTime::parse_from_rfc3339(until).is_ok_and(|t| t.with_timezone(&Utc) > now);
            if active {
                f.dismissed_until = Some(until.clone());
                dismissed.push(f);
                continue;
            }
        }
        findings.push(f);
    }
    let penalty: u32 = findings
        .iter()
        .map(|f| match f.severity {
            "critical" => 15,
            "warning" => 7,
            _ => 2,
        })
        .sum();
    Report {
        score: 100u32.saturating_sub(penalty),
        findings,
        dismissed,
        ram_bytes: ram,
    }
}

pub fn load_dismissed(db: &Db) -> BTreeMap<String, String> {
    db.setting(DISMISS_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
pub struct DismissBody {
    /// Days to snooze; absent/0 = forever; negative = undismiss.
    pub days: Option<i64>,
}

pub fn dismiss(db: &Db, id: &str, days: Option<i64>) -> anyhow::Result<()> {
    let mut m = load_dismissed(db);
    match days {
        Some(d) if d < 0 => {
            m.remove(id);
        }
        Some(d) if d > 0 => {
            m.insert(
                id.to_string(),
                crate::util::fmt_ts(Utc::now() + Duration::days(d)),
            );
        }
        _ => {
            m.insert(id.to_string(), "forever".into());
        }
    }
    db.set_setting(DISMISS_KEY, &serde_json::to_string(&m)?)
}

// ------------------------------------------------------------------ fixes

pub const GZIP_DROPIN: &str = "/etc/nginx/conf.d/zz-nexacloud-gzip.conf";
pub const MYSQL_DROPIN: &str = "/etc/mysql/mysql.conf.d/zz-nexacloud.cnf";
pub const JOURNALD_DROPIN: &str = "/etc/systemd/journald.conf.d/nexacloud.conf";

pub fn php_tuning_path(v: &str) -> String {
    format!("/etc/php/{v}/fpm/conf.d/98-nexacloud-tuning.ini")
}

/// Build the fix plan for a rule id.
pub async fn fix_plan(env: &ChangeEnv, rule: &str) -> Result<ChangePlan, String> {
    let ram = mem_total();
    let build = |b: crate::changes::ChangePlanBuilder| b.build(env).map_err(|e| format!("{e:#}"));
    match rule {
        "nginx-hardening" => nginx_hardening::plan(env).await,
        "nginx-gzip" => {
            let t = run(&["nginx", "-T"])
                .await
                .ok_or("nginx -T failed (nginx missing or config broken)")?;
            let a = nginx_hardening::analyse(&t);
            let mut s = String::from("# Managed by NexaCloud: compression for text assets.\n");
            for (k, v) in [
                ("gzip", "on"),
                ("gzip_vary", "on"),
                ("gzip_proxied", "any"),
                ("gzip_comp_level", "5"),
                ("gzip_min_length", "256"),
                ("gzip_types", "text/plain text/css text/xml text/javascript application/javascript application/json application/xml application/rss+xml image/svg+xml font/woff2"),
            ] {
                if !a.http_level.contains_key(k) {
                    s.push_str(&format!("{k} {v};\n"));
                }
            }
            build(
                ChangePlan::builder("advisor_fix", "Compress CSS, JS, JSON and SVG", "nginx")
                    .lock("nginx")
                    .write_file(GZIP_DROPIN, s)
                    .command(["nginx", "-t"])
                    .command(["systemctl", "reload", "nginx"])
                    .verify_command(["systemctl", "is-active", "--quiet", "nginx"])
                    .on_revert(["systemctl", "reload", "nginx"]),
            )
        }
        "swap" => {
            if swap_total() > 0 {
                return Err("swap is already enabled".into());
            }
            let fstab = std::fs::read_to_string(env.host_path("/etc/fstab")).unwrap_or_default();
            let mut new_fstab = fstab.clone();
            if !fstab.contains("/swapfile") {
                if !new_fstab.ends_with('\n') && !new_fstab.is_empty() {
                    new_fstab.push('\n');
                }
                new_fstab.push_str("/swapfile none swap sw 0 0\n");
            }
            build(
                ChangePlan::builder("advisor_fix", "Add a 2 GB swapfile", "memory")
                    .lock("swap")
                    .pre_command_timeout(["fallocate", "-l", "2G", "/swapfile"], 120)
                    .pre_command(["chmod", "600", "/swapfile"])
                    .pre_command(["mkswap", "/swapfile"])
                    .pre_command(["swapon", "/swapfile"])
                    .write_file("/etc/fstab", new_fstab)
                    .write_file("/etc/sysctl.d/99-nexacloud-swap.conf", "# Managed by NexaCloud\nvm.swappiness = 10\n")
                    .command(["sysctl", "-w", "vm.swappiness=10"])
                    .verify_command(["sh", "-c", "[ \"$(awk 'NR>1' /proc/swaps | wc -l)\" -gt 0 ]"])
                    .on_revert(["sh", "-c", "swapoff /swapfile 2>/dev/null; rm -f /swapfile"])
                    .note("Creates /swapfile (2 GB), enables it now and at boot, and sets vm.swappiness=10."),
            )
        }
        r if r.starts_with("php-tuning-") => {
            let v = &r["php-tuning-".len()..];
            if !crate::plans::sites::valid_php_version(v)
                || !env.host_path(&format!("/etc/php/{v}/fpm")).is_dir()
            {
                return Err(format!("PHP {v} FPM is not installed"));
            }
            build(
                ChangePlan::builder("advisor_fix", &format!("Tune PHP {v} (OPcache, upload limits)"), &format!("php:{v}"))
                    .lock(&format!("php:{v}"))
                    .write_file(&php_tuning_path(v), "; Managed by NexaCloud (advisor)\nopcache.enable = 1\nopcache.memory_consumption = 256\nopcache.interned_strings_buffer = 16\nopcache.max_accelerated_files = 20000\nupload_max_filesize = 64M\npost_max_size = 64M\nmemory_limit = 256M\n")
                    .command([&format!("php-fpm{v}"), "-t"])
                    .command(["systemctl", "reload", &format!("php{v}-fpm")])
                    .verify_command(["systemctl", "is-active", "--quiet", &format!("php{v}-fpm")])
                    .on_revert(["systemctl", "reload", &format!("php{v}-fpm")]),
            )
        }
        "mysql-buffer-pool" | "mysql-bind" => {
            let existing = std::fs::read_to_string(env.host_path(MYSQL_DROPIN)).unwrap_or_default();
            let mut keys: BTreeMap<String, String> = existing
                .lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            if rule == "mysql-buffer-pool" {
                keys.insert(
                    "innodb_buffer_pool_size".into(),
                    format!("{}M", buffer_pool_mb(ram)),
                );
                keys.entry("max_connections".into())
                    .or_insert_with(|| "100".into());
            } else {
                keys.insert("bind-address".into(), "127.0.0.1".into());
                keys.insert("mysqlx-bind-address".into(), "127.0.0.1".into());
            }
            let mut s = String::from(
                "# Managed by NexaCloud (advisor). Remove this file to undo.\n[mysqld]\n",
            );
            for (k, v) in &keys {
                s.push_str(&format!("{k} = {v}\n"));
            }
            build(
                ChangePlan::builder(
                    "advisor_fix",
                    if rule == "mysql-buffer-pool" {
                        "Resize the MySQL buffer pool"
                    } else {
                        "Make MySQL listen on 127.0.0.1 only"
                    },
                    "service:mysql",
                )
                .lock("db:mysql")
                .create_parents()
                .write_file(MYSQL_DROPIN, s)
                .command_timeout(["systemctl", "restart", "mysql"], 180)
                .verify_command(["mysqladmin", "ping"])
                .on_revert(["systemctl", "restart", "mysql"])
                .note("MySQL restarts: sites lose database access for a few seconds."),
            )
        }
        "journald-size" => build(
            ChangePlan::builder("advisor_fix", "Cap system logs at 500 MB", "journald")
                .create_parents()
                .write_file(
                    JOURNALD_DROPIN,
                    "# Managed by NexaCloud\n[Journal]\nSystemMaxUse=500M\n",
                )
                .command(["systemctl", "restart", "systemd-journald"])
                .verify_command(["systemctl", "is-active", "--quiet", "systemd-journald"])
                .on_revert(["systemctl", "restart", "systemd-journald"]),
        ),
        "fail2ban" => crate::plans::system::plan_service_install(env, "fail2ban"),
        "unattended-upgrades" => {
            if env.host_path("/usr/bin/unattended-upgrade").exists() {
                build(
                    ChangePlan::builder("advisor_fix", "Turn on automatic security updates", "apt")
                        .write_file("/etc/apt/apt.conf.d/20auto-upgrades", "APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n"),
                )
            } else {
                crate::plans::system::plan_service_install(env, "unattended-upgrades")
            }
        }
        "redis" => crate::plans::system::plan_service_install(env, "redis"),
        _ => Err(format!("no automatic fix for {rule:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_pool_budget() {
        assert_eq!(buffer_pool_mb(8 * GB), 1536);
        assert_eq!(buffer_pool_mb(2 * GB), 384);
        assert_eq!(buffer_pool_mb(64 * GB), 8192);
    }

    #[test]
    fn env_parsing() {
        let t = "APP_ENV=production\nAPP_DEBUG=\"true\"\n# QUEUE_CONNECTION=redis\nQUEUE_CONNECTION=sync\n";
        assert_eq!(env_value(t, "APP_DEBUG").as_deref(), Some("true"));
        assert_eq!(env_value(t, "QUEUE_CONNECTION").as_deref(), Some("sync"));
        assert_eq!(env_value(t, "NOPE"), None);
    }
}
