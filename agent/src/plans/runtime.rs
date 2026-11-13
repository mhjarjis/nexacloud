//! FrankenPHP as an alternative PHP runtime per site, and the UFW firewall
//! with a confirm-or-revert guard.
//!
//! FrankenPHP: each switched site gets its own systemd unit running
//! `frankenphp run` (classic mode) on a loopback port, as the project
//! directory's owner; nginx keeps TLS, certificates and static files and
//! proxies PHP requests to it. The PHP-FPM pool stays, so switching back is
//! one click. Every switch probes the runtime and the public site; any
//! failure restores the previous nginx config and stops the unit.

use serde::Deserialize;

use crate::changes::{ChangeEnv, ChangePlan};
use crate::plans::sites::{self, Runtime, SiteModel};

pub const FRANKENPHP_BIN: &str = "/usr/local/bin/frankenphp";
/// Pinned release + checksum (never "latest"): bump both together.
pub const FRANKENPHP_VERSION: &str = "v1.13.5";
const FRANKENPHP_URL: &str =
    "https://github.com/php/frankenphp/releases/download/v1.13.5/frankenphp-linux-x86_64";
const FRANKENPHP_SHA256: &str = "52d262be9b107a1a6f5f1d2945393dd838e89e4735c38a376b940e9fa9e1c881";
pub const GUARD_UNIT: &str = "nexacloud-firewall-guard";

fn unit_name(id: &str) -> String {
    format!("nexacloud-frankenphp-{}", id.replace('.', "-"))
}

fn caddyfile(m: &SiteModel, port: u16) -> String {
    format!(
        "# Managed by NexaCloud — FrankenPHP for {id}\n{{\n\tfrankenphp\n\tadmin off\n\tauto_https off\n\tpersist_config off\n}}\n\n# Any Host header (nginx forwards the site's domain), loopback only.\nhttp://:{port} {{\n\tbind 127.0.0.1\n\troot * {root}\n\tencode zstd gzip\n\tphp_server\n}}\n",
        id = m.id,
        root = m.root.as_deref().unwrap_or("/var/www/html")
    )
}

fn unit(m: &SiteModel, user: &str) -> String {
    let id = &m.id;
    format!(
        "# Managed by NexaCloud\n[Unit]\nDescription=FrankenPHP for {id}\nAfter=network.target\n\n[Service]\nUser={user}\nGroup={user}\nWorkingDirectory={dir}\nEnvironment=HOME=/var/lib/frankenphp/{id}\nEnvironment=XDG_DATA_HOME=/var/lib/frankenphp/{id}\nEnvironment=XDG_CONFIG_HOME=/var/lib/frankenphp/{id}\nExecStart={FRANKENPHP_BIN} run --config /etc/frankenphp/{id}/Caddyfile\nRestart=on-failure\nRestartSec=2\nMemoryMax=1G\nLimitNOFILE=65535\n\n[Install]\nWantedBy=multi-user.target\n",
        dir = m.project_dir.as_deref().or(m.root.as_deref()).unwrap_or("/var/www")
    )
}

/// First free port from 8100 not used by another managed site.
fn pick_port(env: &ChangeEnv, me: &str) -> u16 {
    let used: Vec<u16> = std::fs::read_dir(env.host_path("/etc/nginx/conf.d"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .filter_map(|t| sites::parse_model(&t))
                .filter(|m| m.id != me)
                .filter_map(|m| m.runtime_port)
                .collect()
        })
        .unwrap_or_default();
    (8100..9000).find(|p| !used.contains(p)).unwrap_or(8100)
}

fn owner(env: &ChangeEnv, m: &SiteModel) -> String {
    use std::os::unix::fs::MetadataExt;
    let dir = m
        .project_dir
        .as_deref()
        .or(m.root.as_deref())
        .unwrap_or("/var/www");
    let uid = std::fs::metadata(env.host_path(dir))
        .map(|md| md.uid())
        .unwrap_or(33);
    // Resolve via /etc/passwd (no subprocess at plan time).
    std::fs::read_to_string(env.host_path("/etc/passwd"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() > 2 && f[2] == uid.to_string()).then(|| f[0].to_string())
            })
        })
        .filter(|u| u != "root")
        .unwrap_or_else(|| "www-data".into())
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeParams {
    pub id: String,
    /// fpm | frankenphp
    pub runtime: Runtime,
}

pub fn plan_runtime(env: &ChangeEnv, p: &RuntimeParams) -> Result<ChangePlan, String> {
    let old = sites::load_model(env, &p.id)?;
    if !old.kind.uses_php() {
        return Err("only PHP sites can switch runtime".into());
    }
    if old.runtime == p.runtime {
        return Err(format!("{} already runs on {:?}", p.id, p.runtime));
    }
    let unit_n = unit_name(&p.id);
    let unit_path = format!("/etc/systemd/system/{unit_n}.service");
    let caddy_path = format!("/etc/frankenphp/{}/Caddyfile", p.id);
    let mut new = old.clone();
    new.runtime = p.runtime.clone();
    let title;
    let mut b;
    match p.runtime {
        Runtime::Frankenphp => {
            let port = pick_port(env, &p.id);
            new.runtime_port = Some(port);
            let user = owner(env, &old);
            title = format!("Run {} on FrankenPHP", p.id);
            b = ChangePlan::builder("site_runtime", &title, &format!("site:{}", p.id))
                .lock(&format!("site:{}", p.id))
                .lock("nginx")
                .create_parents();
            if !env.host_path(FRANKENPHP_BIN).exists() {
                b = b
                    .pre_command_timeout(
                        [
                            "curl",
                            "-fsSL",
                            "--retry",
                            "5",
                            "--retry-all-errors",
                            "-C",
                            "-",
                            "-o",
                            "/usr/local/bin/frankenphp.download",
                            FRANKENPHP_URL,
                        ],
                        1800,
                    )
                    .pre_command([
                        "sh",
                        "-c",
                        "echo \"$1  /usr/local/bin/frankenphp.download\" | sha256sum -c - || { rm -f /usr/local/bin/frankenphp.download; echo 'checksum mismatch: refusing to install FrankenPHP'; exit 1; }",
                        "verify",
                        FRANKENPHP_SHA256,
                    ])
                    .pre_command([
                        "install",
                        "-m",
                        "0755",
                        "/usr/local/bin/frankenphp.download",
                        FRANKENPHP_BIN,
                    ])
                    .pre_command(["rm", "-f", "/usr/local/bin/frankenphp.download"]);
            }
            b = b
                .pre_command(["install", "-d", "-m", "0750", "-o", &user, "-g", &user, &format!("/var/lib/frankenphp/{}", p.id)])
                .write_file(&caddy_path, caddyfile(&new, port))
                .write_file(&unit_path, unit(&new, &user))
                .write_file(&sites::conf_path(&p.id), sites::render(&new))
                .command(["systemctl", "daemon-reload"])
                .command(["systemctl", "enable", "--now", &unit_n])
                .command([
                    "sh", "-c",
                    "for i in $(seq 1 20); do code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$1/); [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ] && { echo \"FrankenPHP answers: HTTP $code\"; exit 0; }; sleep 1; done; echo \"FrankenPHP did not answer (last HTTP $code)\"; exit 1",
                    "probe", &port.to_string(),
                ])
                .command(["nginx", "-t"])
                .command(["systemctl", "reload", "nginx"])
                .on_revert(["sh", "-c", "systemctl disable --now \"$1\" 2>/dev/null; systemctl daemon-reload; systemctl reload nginx", "revert", &unit_n])
                .note(format!(
                    "Starts FrankenPHP {FRANKENPHP_VERSION} (classic mode, its own bundled PHP) for {} on 127.0.0.1:{port} as {user}, then points nginx at it. The PHP-FPM pool is kept so you can switch back in one click. Check that your app's PHP extensions are in FrankenPHP's build (frankenphp php-cli -m).",
                    p.id
                ));
        }
        Runtime::Fpm => {
            new.runtime_port = None;
            title = format!("Run {} on PHP-FPM", p.id);
            b = ChangePlan::builder("site_runtime", &title, &format!("site:{}", p.id))
                .lock(&format!("site:{}", p.id))
                .lock("nginx")
                .write_file(&sites::conf_path(&p.id), sites::render(&new))
                .delete_file(&unit_path)
                .delete_file(&caddy_path)
                .command(["nginx", "-t"])
                .command(["systemctl", "reload", "nginx"])
                .command([
                    "sh",
                    "-c",
                    "systemctl stop \"$1\" 2>/dev/null; rm -f \"/etc/systemd/system/multi-user.target.wants/$1.service\"; systemctl daemon-reload; systemctl reset-failed \"$1\" 2>/dev/null; true",
                    "stop",
                    &unit_n,
                ])
                .on_revert(["sh", "-c", "systemctl daemon-reload; systemctl enable --now \"$1\" 2>/dev/null; systemctl reload nginx", "revert", &unit_n]);
        }
    }
    b = b.verify_command(["systemctl", "is-active", "--quiet", "nginx"]);
    let (scheme, port) = if new.ssl {
        ("https", "443")
    } else {
        ("http", "80")
    };
    b.verify_command([
        "sh", "-c",
        "for i in 1 2 3 4 5 6 7 8; do code=$(curl -ks -o /dev/null -w '%{http_code}' --resolve \"$1:$2:127.0.0.1\" \"$3://$1/\"); if [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ]; then echo \"HTTP $code\"; exit 0; fi; sleep 1; done; echo \"HTTP $code\"; exit 1",
        "probe", &new.id, port, scheme,
    ])
    .build(env)
    .map_err(|e| format!("{e:#}"))
}

// ------------------------------------------------------------------ firewall

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallParams {
    /// Extra TCP ports to allow besides SSH, HTTP, HTTPS and the panel.
    #[serde(default)]
    pub extra_ports: Vec<u16>,
}

/// Enable UFW with safe defaults. A systemd timer (independent of the agent)
/// disables UFW again after 3 minutes unless `POST /api/firewall/confirm` is
/// called from a browser that can still reach the panel.
pub fn plan_firewall(
    env: &ChangeEnv,
    panel_port: u16,
    p: &FirewallParams,
) -> Result<ChangePlan, String> {
    if p.extra_ports.len() > 20 || p.extra_ports.contains(&0) {
        return Err("give up to 20 ports".into());
    }
    let ssh_port = sshd_port(env);
    let mut b = ChangePlan::builder("firewall_enable", "Turn on the firewall (UFW)", "firewall")
        .lock("firewall");
    if !env.host_path("/usr/sbin/ufw").exists() {
        b = b
            .lock("apt")
            .pre_command_timeout(["apt-get", "update", "-q"], 600)
            .pre_command_timeout(["apt-get", "install", "-y", "-q", "ufw"], 900);
    }
    // Arm the guard first: if anything below locks us out, it undoes itself.
    b = b
        .pre_command(["sh", "-c", "systemctl stop \"$1.timer\" \"$1.service\" 2>/dev/null; systemctl reset-failed \"$1.service\" \"$1.timer\" 2>/dev/null; true", "guard", GUARD_UNIT])
        .pre_command(["systemd-run", &format!("--unit={GUARD_UNIT}"), "--on-active=180", "--timer-property=AccuracySec=1s", "/usr/sbin/ufw", "--force", "disable"]);
    let mut ports: Vec<String> = vec![
        format!("{ssh_port}/tcp"),
        "80/tcp".into(),
        "443/tcp".into(),
        format!("{panel_port}/tcp"),
    ];
    for x in &p.extra_ports {
        let s = format!("{x}/tcp");
        if !ports.contains(&s) {
            ports.push(s);
        }
    }
    b = b
        .command(["ufw", "default", "deny", "incoming"])
        .command(["ufw", "default", "allow", "outgoing"])
        .command(["ufw", "limit", &format!("{ssh_port}/tcp")]);
    for port in ports.iter().skip(1) {
        b = b.command(["ufw", "allow", port]);
    }
    b.command(["ufw", "--force", "enable"])
        .verify_command(["sh", "-c", "ufw status | grep -q '^Status: active'"])
        .on_revert(["sh", "-c", "ufw --force disable; systemctl stop \"$1.timer\" 2>/dev/null; true", "revert", GUARD_UNIT])
        .note(format!(
            "Allows only {} (SSH is rate-limited). Safety net: unless you click “Keep it on” within 3 minutes, a timer turns the firewall off again — so you can't lock yourself out.",
            ports.join(", ")
        ))
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

fn sshd_port(env: &ChangeEnv) -> u16 {
    // The effective config (includes sshd_config.d/*), when sshd is present.
    if env.root == std::path::Path::new("/") {
        if let Ok(o) = std::process::Command::new("sshd").arg("-T").output() {
            let t = String::from_utf8_lossy(&o.stdout);
            if let Some(p) = t
                .lines()
                .find_map(|l| l.strip_prefix("port ").and_then(|p| p.trim().parse().ok()))
            {
                return p;
            }
        }
    }
    let mut files = vec![env.host_path("/etc/ssh/sshd_config")];
    if let Ok(rd) = std::fs::read_dir(env.host_path("/etc/ssh/sshd_config.d")) {
        files.extend(rd.flatten().map(|e| e.path()));
    }
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .find_map(|t| {
            t.lines().find_map(|l| {
                let l = l.trim();
                l.strip_prefix("Port ").and_then(|p| p.trim().parse().ok())
            })
        })
        .unwrap_or(22)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caddyfile_and_unit() {
        let m = sites::model_from_create(&sites::CreateParams {
            kind: sites::SiteKind::Laravel,
            domain: "shop.example.com".into(),
            aliases: vec![],
            root: None,
            php_version: Some("8.3".into()),
            upstream: None,
            redirect_to: None,
            redirect_code: None,
            options: None,
        });
        let c = caddyfile(&m, 8101);
        assert!(c.contains("http://:8101 {"));
        assert!(c.contains("bind 127.0.0.1"));
        assert!(c.contains("root * /var/www/shop.example.com/public"));
        assert!(c.contains("php_server"));
        let u = unit(&m, "www-data");
        assert!(u.contains("User=www-data"));
        assert!(u.contains("ExecStart=/usr/local/bin/frankenphp run --config /etc/frankenphp/shop.example.com/Caddyfile"));
        let mut f = m.clone();
        f.runtime = Runtime::Frankenphp;
        f.runtime_port = Some(8101);
        let conf = sites::render(&f);
        assert!(conf.contains("proxy_pass http://127.0.0.1:8101;"));
        assert!(conf.contains("try_files $uri @app;"));
        assert!(!conf.contains("fastcgi_pass"));
        assert!(
            conf.contains("location ~ \\.php$ {\n        proxy_pass http://127.0.0.1:8101;"),
            "{conf}"
        );
    }
}
