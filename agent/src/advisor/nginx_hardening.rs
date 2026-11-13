//! "nginx global tuning drop-in": the first real change type, and the
//! template later fixes copy.
//!
//! Writes `/etc/nginx/conf.d/zz-nexacloud.conf` with global directives NexaCloud
//! recommends (today: `server_tokens off;` — hide the nginx version), but only
//! those **not already set at `http` level** anywhere else. Detection reads
//! `nginx -T` (the full effective config), so the commented
//! `# server_tokens off;` in Ubuntu's stock nginx.conf does not count, while a
//! real `server_tokens on;` written by the admin is respected (never overridden;
//! it would also be a duplicate-directive error).
//!
//! Apply: write the drop-in → `nginx -t` → `systemctl reload nginx` → verify
//! nginx is active. Revert/rollback: restore the snapshot → reload.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;

use crate::changes::{ChangeEnv, ChangePlan, Content};
use crate::nginx::{self, Directive};
use crate::routes::security::wildcard_match;
use crate::util::cmd::NullSink;

pub const KIND: &str = "nginx_hardening";
pub const DROPIN: &str = "/etc/nginx/conf.d/zz-nexacloud.conf";
const NGINX_CONF: &str = "/etc/nginx/nginx.conf";
const NGINX_PREFIX: &str = "/etc/nginx/";

/// Directives this drop-in manages, in file order.
pub const DIRECTIVES: &[(&str, &str, &str)] = &[(
    "server_tokens",
    "off",
    "hide the nginx version in error pages and the Server header",
)];

/// What `nginx -T` tells us.
#[derive(Debug, Default, PartialEq)]
pub struct Analysis {
    /// The drop-in path is matched by an `include` inside `http { }`.
    pub includes_dropin: bool,
    /// Directive name → file that sets it at http level (our drop-in excluded).
    pub http_level: BTreeMap<String, String>,
}

/// Split `nginx -T` output into (file path, text) sections.
fn sections(out: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# configuration file ") {
            let path = rest.trim_end().trim_end_matches(':').to_string();
            v.push((path, String::new()));
        } else if let Some((_, text)) = v.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    v
}

fn resolve(pattern: &str) -> String {
    if pattern.starts_with('/') {
        pattern.to_string()
    } else {
        format!("{NGINX_PREFIX}{pattern}")
    }
}

/// Analyse the full config dump.
pub fn analyse(nginx_t: &str) -> Analysis {
    let secs = sections(nginx_t);
    let Some(main_idx) = secs
        .iter()
        .position(|(p, _)| p == NGINX_CONF)
        .or(if secs.is_empty() { None } else { Some(0) })
    else {
        return Analysis::default();
    };
    let (main_path, main_text) = &secs[main_idx];
    let tree = nginx::parse(main_text);
    let http: Vec<Directive> = tree
        .into_iter()
        .find(|d| d.name == "http" && d.block.is_some())
        .and_then(|d| d.block)
        .unwrap_or_default();

    let mut a = Analysis::default();
    let mut patterns = Vec::new();
    for d in &http {
        if d.name == "include" {
            if let Some(p) = d.args.first() {
                patterns.push(resolve(p));
            }
        } else if d.block.is_none() {
            a.http_level
                .entry(d.name.clone())
                .or_insert_with(|| main_path.clone());
        }
    }
    a.includes_dropin = patterns.iter().any(|p| wildcard_match(p, DROPIN));
    for (i, (path, text)) in secs.iter().enumerate() {
        if i == main_idx || path == DROPIN {
            continue;
        }
        if !patterns.iter().any(|p| wildcard_match(p, path)) {
            continue;
        }
        for d in nginx::parse(text) {
            if d.block.is_none() && d.name != "include" {
                a.http_level.entry(d.name).or_insert_with(|| path.clone());
            }
        }
    }
    a
}

/// The drop-in file text for the given directives.
pub fn render(directives: &[&(&str, &str, &str)]) -> String {
    let mut s = String::from(
        "# Managed by NexaCloud: global nginx settings.\n\
         # Changes here are made through the panel (Security page) and can be\n\
         # rolled back from the Activity page or with `nexacloud rollback`.\n\n",
    );
    for (name, value, why) in directives {
        s.push_str(&format!("# {why}\n{name} {value};\n"));
    }
    s
}

/// Build the plan (possibly "nothing to do", with a note saying why).
/// Err: nginx is missing or its current config is broken.
pub async fn plan(env: &ChangeEnv) -> Result<ChangePlan, String> {
    let argv = vec!["nginx".to_string(), "-T".to_string()];
    let out = env
        .runner
        .run(&argv, Duration::from_secs(30), &NullSink)
        .await;
    if out.missing {
        return Err("nginx is not installed".into());
    }
    if !out.ok {
        return Err(format!(
            "the current nginx configuration does not pass `nginx -t`, fix that first: {}",
            out.error_summary("nginx")
        ));
    }
    let a = analyse(&out.stdout);
    let b = ChangePlan::builder(KIND, "Hide nginx version", "nginx").lock("nginx");
    let build = |b: crate::changes::ChangePlanBuilder| b.build(env).map_err(|e| format!("{e:#}"));
    if !a.includes_dropin {
        return build(b.note(
            "nginx.conf does not include /etc/nginx/conf.d/*.conf inside http { }, so a drop-in file would have no effect. Nothing to do.",
        ));
    }
    let wanted: Vec<&(&str, &str, &str)> = DIRECTIVES
        .iter()
        .filter(|(n, _, _)| !a.http_level.contains_key(*n))
        .collect();
    if wanted.is_empty() {
        let set: Vec<String> = DIRECTIVES
            .iter()
            .filter_map(|(n, _, _)| {
                a.http_level
                    .get(*n)
                    .map(|f| format!("{n} is already set in {f}"))
            })
            .collect();
        return build(b.note(format!(
            "{}. NexaCloud does not override settings made by hand. Nothing to do.",
            set.join("; ")
        )));
    }
    let mut plan = build(
        b.write_file_with(DROPIN, Content::Text(render(&wanted)), Some(0o644), None)
            .command(["nginx", "-t"])
            .command(["systemctl", "reload", "nginx"])
            .verify_command(["systemctl", "is-active", "--quiet", "nginx"])
            .on_revert(["systemctl", "reload", "nginx"]),
    )?;
    if plan.files.is_empty() {
        plan.commands.clear();
        plan.verify.clear();
        plan.on_revert.clear();
        plan.note = Some("Already applied: nginx does not show its version.".into());
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::runner::fake::FakeRunner;
    use std::sync::Arc;

    const STOCK: &str = "nginx: the configuration file /etc/nginx/nginx.conf syntax is ok
# configuration file /etc/nginx/nginx.conf:
user www-data;
worker_processes auto;
events { worker_connections 768; }
http {
\tsendfile on;
\t# server_tokens off;
\tinclude /etc/nginx/mime.types;
\tinclude /etc/nginx/conf.d/*.conf;
\tinclude /etc/nginx/sites-enabled/*;
}

# configuration file /etc/nginx/mime.types:
types { text/html html; }

# configuration file /etc/nginx/sites-enabled/shop:
server { listen 80; server_tokens on; include snippets/fastcgi-php.conf; }

# configuration file /etc/nginx/snippets/fastcgi-php.conf:
fastcgi_split_path_info ^(.+?\\.php)(/.*)$;
";

    #[test]
    fn stock_ubuntu_needs_server_tokens() {
        let a = analyse(STOCK);
        assert!(a.includes_dropin);
        assert!(a.http_level.contains_key("sendfile"));
        // Commented in nginx.conf, server-level in a site: neither counts.
        assert!(!a.http_level.contains_key("server_tokens"));
        // Snippets are not included at http level.
        assert!(!a.http_level.contains_key("fastcgi_split_path_info"));
    }

    #[test]
    fn respects_existing_settings() {
        let set_main = STOCK.replace("# server_tokens off;", "server_tokens on;");
        assert_eq!(
            analyse(&set_main)
                .http_level
                .get("server_tokens")
                .map(String::as_str),
            Some("/etc/nginx/nginx.conf")
        );
        let in_confd = format!(
            "{STOCK}\n# configuration file /etc/nginx/conf.d/security.conf:\nserver_tokens off;\n"
        );
        assert_eq!(
            analyse(&in_confd)
                .http_level
                .get("server_tokens")
                .map(String::as_str),
            Some("/etc/nginx/conf.d/security.conf")
        );
        // Our own drop-in does not count as "set elsewhere".
        let ours = format!("{STOCK}\n# configuration file {DROPIN}:\nserver_tokens off;\n");
        assert!(!analyse(&ours).http_level.contains_key("server_tokens"));
        let no_confd = STOCK.replace("include /etc/nginx/conf.d/*.conf;", "");
        assert!(!analyse(&no_confd).includes_dropin);
        assert_eq!(analyse(""), Analysis::default());
    }

    fn env_with(fake: Arc<FakeRunner>) -> (ChangeEnv, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("nexacloud-adv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("etc/nginx/conf.d")).unwrap();
        (
            ChangeEnv {
                root: root.clone(),
                snapshots_dir: root.join("snapshots"),
                runner: fake,
            },
            root,
        )
    }

    #[tokio::test]
    async fn plans_the_dropin_then_nothing_to_do() {
        let fake = Arc::new(FakeRunner::default());
        fake.script("nginx -T", 0, STOCK);
        let (env, root) = env_with(fake.clone());
        let p = plan(&env).await.unwrap();
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].path, DROPIN);
        let text = p.files[0]
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .unwrap()
            .to_string();
        assert!(text.contains("server_tokens off;"));
        assert_eq!(p.commands[0].argv, vec!["nginx", "-t"]);
        assert_eq!(p.commands[1].argv, vec!["systemctl", "reload", "nginx"]);
        assert_eq!(p.locks, vec!["nginx"]);

        // Once written (and visible in nginx -T), there is nothing to do.
        std::fs::write(env.host_path(DROPIN), &text).unwrap();
        fake.script(
            "nginx -T",
            0,
            &format!("{STOCK}\n# configuration file {DROPIN}:\n{text}"),
        );
        let p2 = plan(&env).await.unwrap();
        assert!(p2.is_noop());
        assert!(p2.note.unwrap().contains("Already applied"));

        // Set by hand → nothing to do, with the reason.
        fake.script(
            "nginx -T",
            0,
            &STOCK.replace("# server_tokens off;", "server_tokens on;"),
        );
        let p3 = plan(&env).await.unwrap();
        assert!(p3.is_noop());
        assert!(p3
            .note
            .unwrap()
            .contains("already set in /etc/nginx/nginx.conf"));

        // Broken config → refuse.
        fake.script("nginx -T", 1, "");
        assert!(plan(&env).await.unwrap_err().contains("fix that first"));
        let _ = std::fs::remove_dir_all(root);
    }
}
