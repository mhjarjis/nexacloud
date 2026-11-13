//! Site management plans: create / delete managed sites, switch PHP version,
//! Let's Encrypt SSL, site options and the raw nginx editor.
//!
//! **Managed** sites live in `/etc/nginx/conf.d/nexacloud-<id>.conf`, rendered
//! from a [`SiteModel`] that is stored as JSON in the file's second line
//! (`# nexacloud: {...}`). Because the model lives in the file itself, a
//! snapshot rollback restores the model too — no separate state to drift.
//! Free-form extra rules go in `/etc/nginx/nexacloud/<id>.custom.conf`
//! (included, never touched by NexaCloud).
//!
//! **Unmanaged** sites (existing hand-written configs) can have their PHP
//! version switched and be edited in the raw editor; both rewrite the real
//! file (symlinks in `sites-enabled` are resolved first).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::changes::{ChangeEnv, ChangePlan, Content};
use crate::routes::php::versions_on_disk;

pub const MANAGED_PREFIX: &str = "/etc/nginx/conf.d/nexacloud-";
pub const ACME_ROOT: &str = "/var/www/_acme";
pub const CUSTOM_DIR: &str = "/etc/nginx/nexacloud";
const MODEL_MARK: &str = "# nexacloud: ";
const RENEW_HOOK: &str = "/etc/letsencrypt/renewal-hooks/deploy/nexacloud-reload-nginx.sh";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteKind {
    Laravel,
    Php,
    Static,
    Proxy,
    Redirect,
}

impl SiteKind {
    pub fn uses_php(&self) -> bool {
        matches!(self, SiteKind::Laravel | SiteKind::Php)
    }
}

/// Runtime serving PHP for a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    #[default]
    Fpm,
    Frankenphp,
}

fn default_body() -> String {
    "64M".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteOptions {
    #[serde(default = "default_body")]
    pub client_max_body_size: String,
    #[serde(default = "yes")]
    pub security_headers: bool,
    #[serde(default)]
    pub hsts: bool,
    #[serde(default = "yes")]
    pub cache_static: bool,
    #[serde(default = "yes")]
    pub block_hidden: bool,
}

impl Default for SiteOptions {
    fn default() -> Self {
        SiteOptions {
            client_max_body_size: default_body(),
            security_headers: true,
            hsts: false,
            cache_static: true,
            block_hidden: true,
        }
    }
}

/// Everything a managed nginx site is rendered from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteModel {
    pub id: String,
    pub kind: SiteKind,
    /// First entry is the primary domain.
    pub domains: Vec<String>,
    /// Project directory (laravel: contains `artisan`).
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Document root nginx serves.
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub php_version: Option<String>,
    #[serde(default)]
    pub runtime: Runtime,
    /// FrankenPHP loopback port (runtime = frankenphp).
    #[serde(default)]
    pub runtime_port: Option<u16>,
    /// proxy: upstream URL (`http://127.0.0.1:3000`).
    #[serde(default)]
    pub upstream: Option<String>,
    /// redirect: target URL and code.
    #[serde(default)]
    pub redirect_to: Option<String>,
    #[serde(default)]
    pub redirect_code: Option<u16>,
    #[serde(default)]
    pub ssl: bool,
    #[serde(default)]
    pub options: SiteOptions,
    /// Adopted sites: keep their own PHP-FPM socket (e.g. a per-site pool).
    #[serde(default)]
    pub fpm_socket: Option<String>,
    /// Adopted sites: keep their existing certificate paths.
    #[serde(default)]
    pub ssl_cert: Option<String>,
    #[serde(default)]
    pub ssl_key: Option<String>,
}

// ------------------------------------------------------------------ validation

pub fn valid_domain(d: &str) -> bool {
    let d = d.trim();
    if d.is_empty() || d.len() > 253 || d.starts_with('.') || d.ends_with('.') {
        return false;
    }
    d.split('.').count() >= 2
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

pub fn valid_site_id(id: &str) -> bool {
    valid_domain(id)
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

fn valid_upstream(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://"))
        && u.len() < 200
        && u.bytes().all(|b| {
            b.is_ascii_graphic() && !matches!(b, b';' | b'{' | b'}' | b'"' | b'\'' | b'$' | b'\\')
        })
}

fn valid_body_size(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.len() <= 6
        && s[..s.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && matches!(
            s.as_bytes()[s.len() - 1],
            b'M' | b'm' | b'G' | b'g' | b'K' | b'k' | b'0'..=b'9'
        )
}

fn valid_path(p: &str) -> bool {
    (p.starts_with("/var/www/") || p.starts_with("/home/") || p.starts_with("/srv/"))
        && !p.contains("..")
        && p.len() < 200
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

pub fn valid_php_version(v: &str) -> bool {
    let mut it = v.split('.');
    matches!((it.next(), it.next(), it.next()), (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty() && a.len() <= 2 && b.len() <= 2 && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()))
}

pub fn validate(m: &SiteModel) -> Result<(), String> {
    if !valid_site_id(&m.id) {
        return Err(format!(
            "invalid site id {:?} (use the primary domain, lower case)",
            m.id
        ));
    }
    if m.domains.is_empty() || m.domains.len() > 20 {
        return Err("give 1–20 domains".into());
    }
    for d in &m.domains {
        if !valid_domain(d) {
            return Err(format!("invalid domain {d:?}"));
        }
    }
    if m.domains[0] != m.id {
        return Err("the first domain must equal the site id".into());
    }
    if !valid_body_size(&m.options.client_max_body_size) {
        return Err("client_max_body_size must look like 64M".into());
    }
    match m.kind {
        SiteKind::Laravel | SiteKind::Php | SiteKind::Static => {
            let root = m.root.as_deref().ok_or("document root is required")?;
            if !valid_path(root) {
                return Err(
                    "document root must be under /var/www/ (letters, digits, / . _ -)".into(),
                );
            }
            if let Some(p) = &m.project_dir {
                if !valid_path(p) {
                    return Err("project directory must be under /var/www/".into());
                }
            }
            if m.kind.uses_php() {
                let v = m.php_version.as_deref().ok_or("PHP version is required")?;
                if !valid_php_version(v) {
                    return Err(format!("invalid PHP version {v:?}"));
                }
            }
        }
        SiteKind::Proxy => {
            let u = m.upstream.as_deref().ok_or("upstream URL is required")?;
            if !valid_upstream(u) {
                return Err("upstream must be http(s)://host:port".into());
            }
        }
        SiteKind::Redirect => {
            let u = m
                .redirect_to
                .as_deref()
                .ok_or("redirect target is required")?;
            if !valid_upstream(u) {
                return Err("redirect target must be an http(s) URL".into());
            }
            if !matches!(m.redirect_code.unwrap_or(301), 301 | 302 | 307 | 308) {
                return Err("redirect code must be 301, 302, 307 or 308".into());
            }
        }
    }
    let safe_file = |p: &str| {
        p.starts_with('/')
            && !p.contains("..")
            && p.len() < 200
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    };
    for p in [&m.fpm_socket, &m.ssl_cert, &m.ssl_key]
        .into_iter()
        .flatten()
    {
        if !safe_file(p) {
            return Err(format!("unsupported path {p:?}"));
        }
    }
    if m.runtime == Runtime::Frankenphp && m.runtime_port.is_none() {
        return Err("FrankenPHP runtime needs a port".into());
    }
    Ok(())
}

// ------------------------------------------------------------------ render

pub fn conf_path(id: &str) -> String {
    format!("{MANAGED_PREFIX}{id}.conf")
}

pub fn cert_dir(id: &str) -> String {
    format!("/etc/letsencrypt/live/{id}")
}

/// Render the full managed config for a site.
pub fn render(m: &SiteModel) -> String {
    let names = m.domains.join(" ");
    let model_json = serde_json::to_string(m).unwrap_or_default();
    let mut out = String::new();
    out.push_str("# Managed by NexaCloud — changes here are overwritten. Put extra rules in\n");
    out.push_str(&format!("# {CUSTOM_DIR}/{}.custom.conf\n", m.id));
    out.push_str(&format!("{MODEL_MARK}{model_json}\n\n"));

    let acme = format!(
        "    location ^~ /.well-known/acme-challenge/ {{\n        root {ACME_ROOT};\n        default_type text/plain;\n    }}\n"
    );

    if m.ssl {
        out.push_str(&format!(
            "server {{\n    listen 80;\n    listen [::]:80;\n    server_name {names};\n{acme}    location / {{\n        return 301 https://$host$request_uri;\n    }}\n}}\n\n"
        ));
        out.push_str(&format!(
            "server {{\n    listen 443 ssl http2;\n    listen [::]:443 ssl http2;\n    server_name {names};\n    ssl_certificate {cert};\n    ssl_certificate_key {key};\n    ssl_protocols TLSv1.2 TLSv1.3;\n    ssl_session_cache shared:NexaSSL:10m;\n",
            cert = m.ssl_cert.clone().unwrap_or_else(|| format!("{}/fullchain.pem", cert_dir(&m.id))),
            key = m.ssl_key.clone().unwrap_or_else(|| format!("{}/privkey.pem", cert_dir(&m.id))),
        ));
        if m.options.hsts {
            out.push_str("    add_header Strict-Transport-Security \"max-age=31536000\" always;\n");
        }
    } else {
        out.push_str(&format!(
            "server {{\n    listen 80;\n    listen [::]:80;\n    server_name {names};\n"
        ));
    }
    out.push_str(&body(m, if m.ssl { "" } else { &acme }));
    out.push_str(&format!(
        "    include {CUSTOM_DIR}/{}.custom*.conf;\n}}\n",
        m.id
    ));
    out
}

fn body(m: &SiteModel, acme: &str) -> String {
    let o = &m.options;
    let mut b = String::new();
    b.push_str(&format!(
        "    client_max_body_size {};\n",
        o.client_max_body_size
    ));
    if o.security_headers {
        b.push_str("    add_header X-Frame-Options \"SAMEORIGIN\" always;\n");
        b.push_str("    add_header X-Content-Type-Options \"nosniff\" always;\n");
        b.push_str("    add_header Referrer-Policy \"strict-origin-when-cross-origin\" always;\n");
    }
    b.push_str(acme);
    match m.kind {
        SiteKind::Redirect => {
            b.push_str(&format!(
                "    location / {{\n        return {} {}$request_uri;\n    }}\n",
                m.redirect_code.unwrap_or(301),
                m.redirect_to
                    .as_deref()
                    .unwrap_or("https://example.com")
                    .trim_end_matches('/')
            ));
        }
        SiteKind::Proxy => {
            b.push_str(&proxy_location(
                "/",
                m.upstream.as_deref().unwrap_or("http://127.0.0.1:3000"),
            ));
        }
        SiteKind::Static | SiteKind::Laravel | SiteKind::Php => {
            b.push_str(&format!(
                "    root {};\n",
                m.root.as_deref().unwrap_or("/var/www/html")
            ));
            let php = m.kind.uses_php();
            b.push_str(if php {
                "    index index.php index.html;\n    charset utf-8;\n"
            } else {
                "    index index.html index.htm;\n    charset utf-8;\n"
            });
            if o.cache_static {
                b.push_str("    location ~* \\.(?:css|js|mjs|jpg|jpeg|png|gif|webp|avif|svg|ico|woff2?)$ {\n        expires 30d;\n        access_log off;\n        try_files $uri =404;\n    }\n");
            }
            if php && m.runtime == Runtime::Frankenphp {
                let up = format!("http://127.0.0.1:{}", m.runtime_port.unwrap_or(8100));
                // PHP files must never be served as static files: send them to the app.
                b.push_str(&proxy_location("~ \\.php$", &up));
                b.push_str("    location / {\n        try_files $uri @app;\n    }\n");
                b.push_str(&proxy_location("@app", &up));
            } else if php {
                let v = m.php_version.as_deref().unwrap_or("8.3");
                let sock = m
                    .fpm_socket
                    .clone()
                    .unwrap_or_else(|| format!("/run/php/php{v}-fpm.sock"));
                b.push_str("    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n");
                b.push_str("    location = /favicon.ico { access_log off; log_not_found off; }\n");
                b.push_str("    location = /robots.txt  { access_log off; log_not_found off; }\n");
                b.push_str(&format!(
                    "    location ~ \\.php$ {{\n        fastcgi_pass unix:{sock};\n        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;\n        include fastcgi_params;\n        fastcgi_hide_header X-Powered-By;\n        fastcgi_read_timeout 120;\n    }}\n"
                ));
            } else {
                b.push_str("    location / {\n        try_files $uri $uri/ =404;\n    }\n");
            }
            if o.block_hidden {
                b.push_str("    location ~ /\\.(?!well-known).* {\n        deny all;\n    }\n");
            }
        }
    }
    b
}

fn proxy_location(loc: &str, upstream: &str) -> String {
    format!(
        "    location {loc} {{\n        proxy_pass {upstream};\n        proxy_http_version 1.1;\n        proxy_set_header Host $host;\n        proxy_set_header X-Real-IP $remote_addr;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n        proxy_set_header X-Forwarded-Proto $scheme;\n        proxy_set_header Upgrade $http_upgrade;\n        proxy_set_header Connection \"upgrade\";\n        proxy_read_timeout 120;\n    }}\n"
    )
}

/// Parse the model stored in a managed config.
pub fn parse_model(conf: &str) -> Option<SiteModel> {
    conf.lines()
        .take(5)
        .find_map(|l| l.strip_prefix(MODEL_MARK))
        .and_then(|j| serde_json::from_str(j).ok())
}

pub fn load_model(env: &ChangeEnv, id: &str) -> Result<SiteModel, String> {
    if !valid_site_id(id) {
        return Err("invalid site id".into());
    }
    let text = std::fs::read_to_string(env.host_path(&conf_path(id)))
        .map_err(|_| format!("{id} is not a NexaCloud-managed site"))?;
    parse_model(&text).ok_or_else(|| format!("{} has no readable NexaCloud model", conf_path(id)))
}

// ------------------------------------------------------------------ helpers

fn host_exists(env: &ChangeEnv, p: &str) -> bool {
    env.host_path(p).exists()
}

fn nginx_apply(b: crate::changes::ChangePlanBuilder) -> crate::changes::ChangePlanBuilder {
    b.lock("nginx")
        .command(["nginx", "-t"])
        .command(["systemctl", "reload", "nginx"])
        .verify_command(["systemctl", "is-active", "--quiet", "nginx"])
        .on_revert(["systemctl", "reload", "nginx"])
}

/// Loopback probe that the site answers (any status below 500).
fn probe(b: crate::changes::ChangePlanBuilder, m: &SiteModel) -> crate::changes::ChangePlanBuilder {
    if !m.kind.uses_php() && m.kind != SiteKind::Static {
        return b;
    }
    let (scheme, port) = if m.ssl {
        ("https", "443")
    } else {
        ("http", "80")
    };
    b.verify_command([
        "sh",
        "-c",
        "for i in 1 2 3 4 5 6 7 8; do code=$(curl -ks -o /dev/null -w '%{http_code}' --resolve \"$1:$2:127.0.0.1\" \"$3://$1/\"); if [ \"$code\" -gt 0 ] && [ \"$code\" -lt 500 ]; then echo \"HTTP $code\"; exit 0; fi; sleep 1; done; echo \"HTTP $code\"; exit 1",
        "probe",
        &m.id,
        port,
        scheme,
    ])
}

fn php_installed(env: &ChangeEnv, v: &str) -> bool {
    versions_on_disk(&env.host_path("/etc/php"))
        .iter()
        .any(|x| x == v)
        && env.host_path(&format!("/etc/php/{v}/fpm")).is_dir()
}

// ------------------------------------------------------------------ plans

#[derive(Debug, Clone, Deserialize)]
pub struct CreateParams {
    pub kind: SiteKind,
    pub domain: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub php_version: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub redirect_to: Option<String>,
    #[serde(default)]
    pub redirect_code: Option<u16>,
    #[serde(default)]
    pub options: Option<SiteOptions>,
}

pub fn model_from_create(p: &CreateParams) -> SiteModel {
    let id = p.domain.trim().to_ascii_lowercase();
    let mut domains = vec![id.clone()];
    for a in &p.aliases {
        let a = a.trim().to_ascii_lowercase();
        if !a.is_empty() && !domains.contains(&a) {
            domains.push(a);
        }
    }
    let project = format!("/var/www/{id}");
    let (project_dir, root) = match p.kind {
        SiteKind::Laravel => (
            Some(project.clone()),
            Some(
                p.root
                    .clone()
                    .unwrap_or_else(|| format!("{project}/public")),
            ),
        ),
        SiteKind::Php | SiteKind::Static => (
            Some(project.clone()),
            Some(
                p.root
                    .clone()
                    .unwrap_or_else(|| format!("{project}/public")),
            ),
        ),
        _ => (None, None),
    };
    SiteModel {
        id,
        kind: p.kind.clone(),
        domains,
        project_dir,
        root,
        php_version: if p.kind.uses_php() {
            p.php_version.clone()
        } else {
            None
        },
        runtime: Runtime::Fpm,
        runtime_port: None,
        upstream: p.upstream.clone(),
        redirect_to: p.redirect_to.clone(),
        redirect_code: p.redirect_code,
        ssl: false,
        options: p.options.clone().unwrap_or_default(),
        fpm_socket: None,
        ssl_cert: None,
        ssl_key: None,
    }
}

/// Every server_name already served by nginx (managed or not).
fn taken_names(skip_id: Option<&str>) -> Vec<String> {
    let blocks = crate::routes::sites::read_blocks(
        Path::new("/etc/nginx/sites-enabled"),
        Path::new("/etc/nginx/conf.d"),
    );
    blocks
        .into_iter()
        .filter(|b| skip_id.is_none_or(|id| b.config_path != Path::new(&conf_path(id))))
        .flat_map(|b| b.server_names)
        .collect()
}

pub fn plan_create(env: &ChangeEnv, p: &CreateParams) -> Result<ChangePlan, String> {
    let m = model_from_create(p);
    validate(&m)?;
    if host_exists(env, &conf_path(&m.id)) {
        return Err(format!("{} already exists", m.id));
    }
    if env.root == Path::new("/") {
        let taken = taken_names(None);
        if let Some(d) = m.domains.iter().find(|d| taken.contains(d)) {
            return Err(format!("{d} is already served by another nginx site"));
        }
    }
    if m.kind.uses_php() {
        let v = m.php_version.as_deref().unwrap_or_default();
        if !php_installed(env, v) {
            return Err(format!(
                "PHP {v} (FPM) is not installed — install it on the PHP page first"
            ));
        }
    }
    let mut b = ChangePlan::builder(
        "site_create",
        &format!("Create site {}", m.id),
        &format!("site:{}", m.id),
    )
    .lock(&format!("site:{}", m.id))
    .create_parents()
    .pre_command(["install", "-d", "-m", "0755", ACME_ROOT]);
    if let Some(root) = &m.root {
        if !host_exists(env, root) {
            if let Some(pd) = &m.project_dir {
                if !host_exists(env, pd) {
                    b = b.pre_command([
                        "install", "-d", "-m", "0755", "-o", "www-data", "-g", "www-data", pd,
                    ]);
                }
            }
            b = b.pre_command([
                "install", "-d", "-m", "0755", "-o", "www-data", "-g", "www-data", root,
            ]);
            let (file, content) = if m.kind.uses_php() {
                (
                    format!("{root}/index.php"),
                    format!("<?php\n// Placeholder created by NexaCloud. Deploy your code to {}.\necho 'NexaCloud: {} is ready';\n", m.project_dir.as_deref().unwrap_or(root), m.id),
                )
            } else {
                (
                    format!("{root}/index.html"),
                    format!(
                        "<!doctype html><title>{0}</title><p>NexaCloud: {0} is ready.</p>\n",
                        m.id
                    ),
                )
            };
            b = b.write_file_with(&file, Content::Text(content), Some(0o644), Some((33, 33)));
        }
    }
    b = b.write_file(&conf_path(&m.id), render(&m));
    b = probe(nginx_apply(b), &m).note(format!(
        "Creates the nginx site {} ({:?}). Files under {} are never deleted by NexaCloud.",
        m.domains.join(", "),
        m.kind,
        m.project_dir.as_deref().unwrap_or("-")
    ));
    b.build(env).map_err(|e| format!("{e:#}"))
}

pub fn plan_delete(env: &ChangeEnv, id: &str) -> Result<ChangePlan, String> {
    let m = load_model(env, id)?;
    let b = ChangePlan::builder(
        "site_delete",
        &format!("Remove site {id}"),
        &format!("site:{id}"),
    )
    .lock(&format!("site:{id}"))
    .delete_file(&conf_path(id))
    .note(format!(
        "Removes the nginx config only. Files in {} and certificates stay on disk.",
        m.project_dir.as_deref().unwrap_or("the web root")
    ));
    nginx_apply(b).build(env).map_err(|e| format!("{e:#}"))
}

fn plan_model_update(
    env: &ChangeEnv,
    old: &SiteModel,
    new: SiteModel,
    kind: &str,
    title: String,
) -> Result<ChangePlan, String> {
    validate(&new)?;
    let b = ChangePlan::builder(kind, &title, &format!("site:{}", old.id))
        .lock(&format!("site:{}", old.id))
        .write_file(&conf_path(&old.id), render(&new));
    probe(nginx_apply(b), &new)
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionsParams {
    pub id: String,
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    pub options: SiteOptions,
}

pub fn plan_options(env: &ChangeEnv, p: &OptionsParams) -> Result<ChangePlan, String> {
    let old = load_model(env, &p.id)?;
    let mut new = old.clone();
    new.options = p.options.clone();
    if let Some(aliases) = &p.aliases {
        let mut d = vec![old.id.clone()];
        for a in aliases {
            let a = a.trim().to_ascii_lowercase();
            if !a.is_empty() && !d.contains(&a) {
                d.push(a);
            }
        }
        new.domains = d;
    }
    plan_model_update(
        env,
        &old,
        new,
        "site_options",
        format!("Update settings of {}", p.id),
    )
}

/// Switch PHP version: managed → re-render; unmanaged → rewrite the socket
/// path in the real config file.
pub fn plan_php_switch(env: &ChangeEnv, id: &str, version: &str) -> Result<ChangePlan, String> {
    if !valid_php_version(version) {
        return Err("invalid PHP version".into());
    }
    if !php_installed(env, version) {
        return Err(format!("PHP {version} (FPM) is not installed"));
    }
    if let Ok(old) = load_model(env, id) {
        if !old.kind.uses_php() {
            return Err("this site does not run PHP".into());
        }
        let mut new = old.clone();
        new.php_version = Some(version.to_string());
        if let Some(sock) = &old.fpm_socket {
            new.fpm_socket = Some(replace_php_sockets(sock, version).0);
        }
        return plan_model_update(
            env,
            &old,
            new,
            "site_php",
            format!("Switch {id} to PHP {version}"),
        );
    }
    // Unmanaged: find the file that serves this id.
    let blocks = crate::nginx::merge_blocks(crate::routes::sites::read_blocks(
        &env.host_path("/etc/nginx/sites-enabled"),
        &env.host_path("/etc/nginx/conf.d"),
    ));
    let block = blocks
        .iter()
        .find(|b| b.id() == id)
        .ok_or_else(|| format!("site {id} not found"))?;
    let real = std::fs::canonicalize(&block.config_path)
        .map_err(|e| format!("cannot resolve {}: {e}", block.config_path.display()))?;
    let plan_path = unhost(env, &real)?;
    let text = std::fs::read_to_string(&real).map_err(|e| format!("reading config: {e}"))?;
    let (new_text, n) = replace_php_sockets(&text, version);
    if n == 0 {
        return Err("no php-fpm socket (php8.x-fpm.sock) found in this site's config".into());
    }
    let b = ChangePlan::builder(
        "site_php",
        &format!("Switch {id} to PHP {version}"),
        &format!("site:{id}"),
    )
    .lock(&format!("site:{id}"))
    .write_file(&plan_path, new_text)
    .note(format!(
        "Rewrites {n} PHP-FPM socket reference(s) in {plan_path}."
    ));
    nginx_apply(b).build(env).map_err(|e| format!("{e:#}"))
}

/// Replace `phpX.Y-fpm` occurrences with the new version. Returns count.
pub fn replace_php_sockets(text: &str, version: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0;
    let mut rest = text;
    while let Some(i) = rest.find("php") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 3..];
        let vlen = after
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        let ver = &after[..vlen];
        if vlen >= 3 && valid_php_version(ver) && after[vlen..].starts_with("-fpm") {
            out.push_str("php");
            out.push_str(version);
            n += 1;
            rest = &after[vlen..];
        } else {
            out.push_str("php");
            rest = after;
        }
    }
    out.push_str(rest);
    (out, n)
}

fn unhost(env: &ChangeEnv, real: &Path) -> Result<String, String> {
    let rel = real
        .strip_prefix(&env.root)
        .map_err(|_| "config is outside the managed root".to_string())?;
    let p = format!("/{}", rel.to_string_lossy().trim_start_matches('/'));
    if !p.starts_with("/etc/nginx/") {
        return Err("only files under /etc/nginx/ can be edited".into());
    }
    Ok(p)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SslParams {
    pub id: String,
    pub email: String,
}

fn valid_email(e: &str) -> bool {
    let e = e.trim();
    e.len() < 120
        && e.split_once('@').is_some_and(|(u, d)| {
            !u.is_empty()
                && valid_domain(d)
                && u.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        })
}

/// Let's Encrypt (certbot webroot) for a managed site, then switch to HTTPS.
pub fn plan_ssl(env: &ChangeEnv, p: &SslParams) -> Result<ChangePlan, String> {
    let old = load_model(env, &p.id)?;
    if !valid_email(&p.email) {
        return Err(
            "enter a valid email address (Let's Encrypt sends expiry notices there)".into(),
        );
    }
    if old.ssl {
        return Err("SSL is already on for this site".into());
    }
    let mut new = old.clone();
    new.ssl = true;
    validate(&new)?;
    let mut b = ChangePlan::builder(
        "site_ssl",
        &format!("Enable HTTPS for {}", p.id),
        &format!("site:{}", p.id),
    )
    .lock(&format!("site:{}", p.id))
    .lock("certbot")
    .create_parents();
    if !host_exists(env, "/usr/bin/certbot") {
        b = b.pre_command_timeout(["apt-get", "install", "-y", "-q", "certbot"], 600);
    }
    let mut certbot: Vec<String> = [
        "certbot",
        "certonly",
        "--webroot",
        "-w",
        ACME_ROOT,
        "--cert-name",
        &p.id,
        "--non-interactive",
        "--agree-tos",
        "--keep-until-expiring",
        "-m",
        p.email.trim(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Ok(server) = std::env::var("NEXACLOUD_ACME_SERVER") {
        certbot.push("--server".into());
        certbot.push(server);
        certbot.push("--no-verify-ssl".into());
    }
    for d in &old.domains {
        certbot.push("-d".into());
        certbot.push(d.clone());
    }
    b = b
        .pre_command_timeout(certbot, 300)
        .write_file_with(
            RENEW_HOOK,
            Content::Text("#!/bin/sh\n# Reload nginx after certbot renews a certificate (NexaCloud)\nnginx -t && systemctl reload nginx\n".into()),
            Some(0o755),
            None,
        )
        .write_file(&conf_path(&p.id), render(&new))
        .note("Requests a Let's Encrypt certificate (the domain must already point to this server), then serves the site over HTTPS and redirects HTTP. Certbot renews it automatically.");
    probe(nginx_apply(b), &new)
        .build(env)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawParams {
    pub path: String,
    pub content: String,
}

/// Raw editor: any file under /etc/nginx/ (symlinks resolved).
pub fn plan_raw(env: &ChangeEnv, p: &RawParams) -> Result<ChangePlan, String> {
    let real = resolve_nginx_file(env, &p.path)?;
    if p.content.len() > 512 * 1024 {
        return Err("file too large".into());
    }
    let mut content = p.content.replace("\r\n", "\n");
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let b = ChangePlan::builder("nginx_raw", &format!("Edit {real}"), "nginx")
        .write_file(&real, content)
        .note("Saved only if `nginx -t` passes; otherwise the previous file is restored automatically.");
    nginx_apply(b).build(env).map_err(|e| format!("{e:#}"))
}

/// Validate a path is under /etc/nginx/ and return the real (non-symlink) path.
pub fn resolve_nginx_file(env: &ChangeEnv, path: &str) -> Result<String, String> {
    if !path.starts_with("/etc/nginx/") || path.contains("..") || path.contains('\0') {
        return Err("only files under /etc/nginx/ can be edited".into());
    }
    let hp = env.host_path(path);
    let real = std::fs::canonicalize(&hp).map_err(|e| format!("{path}: {e}"))?;
    if !real.is_file() {
        return Err(format!("{path} is not a file"));
    }
    unhost(env, &real)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laravel() -> SiteModel {
        model_from_create(&CreateParams {
            kind: SiteKind::Laravel,
            domain: "shop.example.com".into(),
            aliases: vec!["www.shop.example.com".into()],
            root: None,
            php_version: Some("8.3".into()),
            upstream: None,
            redirect_to: None,
            redirect_code: None,
            options: None,
        })
    }

    #[test]
    fn domain_validation() {
        assert!(valid_domain("shop.example.com"));
        assert!(valid_domain("a-b.co"));
        assert!(!valid_domain("localhost"));
        assert!(!valid_domain("bad_domain.com"));
        assert!(!valid_domain("x.com; rm -rf /"));
        assert!(!valid_domain("-a.com"));
        assert!(valid_php_version("8.3"));
        assert!(!valid_php_version("8.3.1"));
        assert!(!valid_php_version("8;3"));
    }

    #[test]
    fn renders_laravel_and_roundtrips_model() {
        let m = laravel();
        validate(&m).unwrap();
        let c = render(&m);
        assert!(c.contains("server_name shop.example.com www.shop.example.com;"));
        assert!(c.contains("root /var/www/shop.example.com/public;"));
        assert!(c.contains("fastcgi_pass unix:/run/php/php8.3-fpm.sock;"));
        assert!(c.contains("acme-challenge"));
        assert!(!c.contains("listen 443"));
        assert_eq!(parse_model(&c).unwrap(), m);
        let mut s = m.clone();
        s.ssl = true;
        let c = render(&s);
        assert!(c.contains("listen 443 ssl http2;"));
        assert!(c.contains("return 301 https://$host$request_uri;"));
        assert!(c.contains("/etc/letsencrypt/live/shop.example.com/fullchain.pem"));
    }

    #[test]
    fn renders_proxy_redirect_static() {
        let mut m = laravel();
        m.kind = SiteKind::Proxy;
        m.upstream = Some("http://127.0.0.1:3000".into());
        assert!(render(&m).contains("proxy_pass http://127.0.0.1:3000;"));
        m.kind = SiteKind::Redirect;
        m.redirect_to = Some("https://nexadev.io/".into());
        assert!(render(&m).contains("return 301 https://nexadev.io$request_uri;"));
        m.kind = SiteKind::Static;
        assert!(render(&m).contains("try_files $uri $uri/ =404;"));
    }

    #[test]
    fn rejects_injection() {
        let mut m = laravel();
        m.kind = SiteKind::Proxy;
        m.upstream = Some("http://x;}include /etc/shadow;".into());
        assert!(validate(&m).is_err());
        let mut m = laravel();
        m.root = Some("/etc/nginx".into());
        assert!(validate(&m).is_err());
        let mut m = laravel();
        m.options.client_max_body_size = "64M; evil".into();
        assert!(validate(&m).is_err());
    }

    #[test]
    fn php_socket_replacement() {
        let src = "fastcgi_pass unix:/run/php/php8.1-fpm.sock;\n# php is fun\nfastcgi_pass unix:/var/run/php/php7.4-fpm-portal.sock;\n";
        let (out, n) = replace_php_sockets(src, "8.3");
        assert_eq!(n, 2);
        assert!(out.contains("php8.3-fpm.sock"));
        assert!(out.contains("php8.3-fpm-portal.sock"));
        assert!(out.contains("# php is fun"));
    }
}

// ------------------------------------------------------------------ adopt

/// Directives the managed template reproduces; anything else in an adopted
/// config is reported so nothing is lost silently.
const KNOWN_DIRECTIVES: &[&str] = &[
    "listen",
    "server_name",
    "root",
    "index",
    "charset",
    "location",
    "ssl_certificate",
    "ssl_certificate_key",
    "ssl_protocols",
    "ssl_ciphers",
    "ssl_prefer_server_ciphers",
    "ssl_session_cache",
    "ssl_session_timeout",
    "ssl_session_tickets",
    "ssl_dhparam",
    "include",
    "add_header",
    "client_max_body_size",
    "try_files",
    "fastcgi_pass",
    "fastcgi_param",
    "fastcgi_index",
    "fastcgi_split_path_info",
    "fastcgi_hide_header",
    "fastcgi_read_timeout",
    "fastcgi_buffers",
    "fastcgi_buffer_size",
    "return",
    "access_log",
    "error_log",
    "log_not_found",
    "expires",
    "deny",
    "allow",
    "error_page",
    "http2",
    "server_tokens",
    "sendfile",
    "gzip",
    "gzip_types",
    "gzip_vary",
    "proxy_pass",
    "proxy_http_version",
    "proxy_set_header",
    "proxy_read_timeout",
    "proxy_connect_timeout",
    "proxy_send_timeout",
    "proxy_buffering",
    "proxy_redirect",
    "default_type",
    "autoindex",
];

fn known_location(args: &[String]) -> bool {
    let a = args.join(" ");
    matches!(
        a.as_str(),
        "/" | "~ \\.php$"
            | "~ [^/]\\.php(/|$)"
            | "= /favicon.ico"
            | "= /robots.txt"
            | "~ /\\.(?!well-known).*"
            | "~ /\\.ht"
            | "~ /\\."
            | "^~ /.well-known/acme-challenge/"
            | "/.well-known/acme-challenge/"
            | "~ /.well-known/acme-challenge"
    ) || a.starts_with("~* \\.(")
}

fn collect_custom(ds: &[crate::nginx::Directive], out: &mut Vec<String>) {
    for d in ds {
        if d.name == "location" {
            if !known_location(&d.args) {
                out.push(format!("location {}", d.args.join(" ")));
            }
            if let Some(b) = &d.block {
                collect_custom(b, out);
            }
        } else if d.name == "if" || !KNOWN_DIRECTIVES.contains(&d.name.as_str()) {
            out.push(
                format!("{} {}", d.name, d.args.join(" "))
                    .trim()
                    .to_string(),
            );
        } else if d.name == "listen" && d.args.iter().any(|a| a == "default_server") {
            out.push(format!("listen {} (default_server)", d.args.join(" ")));
        }
    }
}

fn find_servers(ds: &[crate::nginx::Directive], out: &mut Vec<crate::nginx::Directive>) {
    for d in ds {
        if d.name == "server" && d.block.is_some() {
            out.push(d.clone());
        } else if let Some(b) = &d.block {
            find_servers(b, out);
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdoptParams {
    pub id: String,
    /// Required when the existing config has directives the template doesn't cover.
    #[serde(default)]
    pub accept_custom: bool,
}

pub fn plan_adopt(env: &ChangeEnv, p: &AdoptParams) -> Result<ChangePlan, String> {
    if load_model(env, &p.id).is_ok() {
        return Err(format!("{} is already managed by NexaCloud", p.id));
    }
    let raw = crate::routes::sites::read_blocks(
        &env.host_path("/etc/nginx/sites-enabled"),
        &env.host_path("/etc/nginx/conf.d"),
    );
    let merged = crate::nginx::merge_blocks(raw.clone());
    let site = merged
        .iter()
        .find(|b| b.id() == p.id)
        .ok_or_else(|| format!("site {} not found", p.id))?;
    let names: Vec<String> = raw
        .iter()
        .filter(|b| b.server_names.iter().any(|n| site.server_names.contains(n)))
        .flat_map(|b| b.server_names.clone())
        .fold(Vec::new(), |mut v, n| {
            if !v.contains(&n) {
                v.push(n);
            }
            v
        });
    let mut domains = vec![p.id.clone()];
    for n in &names {
        if n != "_" && !domains.contains(n) {
            domains.push(n.clone());
        }
    }
    for d in &domains {
        if !valid_domain(d) {
            return Err(format!(
                "{d:?} can't be managed (wildcard/regex server names are not supported yet)"
            ));
        }
    }
    // Files involved must only serve this site.
    let files: Vec<std::path::PathBuf> = raw
        .iter()
        .filter(|b| b.server_names.iter().any(|n| names.contains(n)))
        .map(|b| b.config_path.clone())
        .fold(Vec::new(), |mut v, f| {
            if !v.contains(&f) {
                v.push(f);
            }
            v
        });
    for f in &files {
        for b in raw.iter().filter(|b| &b.config_path == f) {
            if let Some(other) = b
                .server_names
                .iter()
                .find(|n| !names.contains(n) && *n != "_")
            {
                return Err(format!(
                    "{} also serves {other}; split that file first",
                    f.display()
                ));
            }
        }
    }
    // Detect kind and build the model.
    let root = site.root.clone();
    let kind = if site.proxy_pass.is_some() && site.fastcgi_pass.is_none() {
        SiteKind::Proxy
    } else if site.fastcgi_pass.is_some() {
        let is_laravel = root
            .as_deref()
            .and_then(|r| {
                std::path::Path::new(r)
                    .parent()
                    .map(|d| env.host_path(&d.join("artisan").to_string_lossy()).exists())
            })
            .unwrap_or(false);
        if is_laravel {
            SiteKind::Laravel
        } else {
            SiteKind::Php
        }
    } else if root.is_some() {
        SiteKind::Static
    } else {
        return Err("this site has neither a web root nor a proxy; adopt is not supported".into());
    };
    let php_version = site
        .fastcgi_pass
        .as_deref()
        .and_then(crate::nginx::php_version_from_socket);
    let fpm_socket = site
        .fastcgi_pass
        .as_deref()
        .and_then(|s| s.strip_prefix("unix:"))
        .filter(|s| {
            php_version
                .as_deref()
                .is_none_or(|v| *s != format!("/run/php/php{v}-fpm.sock"))
        })
        .map(String::from);
    if site
        .fastcgi_pass
        .as_deref()
        .is_some_and(|s| !s.starts_with("unix:"))
    {
        return Err("PHP over TCP (fastcgi_pass host:port) is not supported by adopt yet".into());
    }
    let mut custom = Vec::new();
    let mut ssl_key = None;
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut servers = Vec::new();
        find_servers(&crate::nginx::parse(&text), &mut servers);
        for srv in servers {
            if let Some(body) = &srv.block {
                if let Some(k) = body.iter().find(|d| d.name == "ssl_certificate_key") {
                    ssl_key = k.args.first().cloned();
                }
                collect_custom(body, &mut custom);
            }
        }
    }
    custom.sort();
    custom.dedup();
    custom.retain(|c| {
        !c.starts_with("include snippets/fastcgi-php.conf")
            && !c.starts_with("include fastcgi_params")
    });
    if !custom.is_empty() && !p.accept_custom {
        return Err(format!(
            "custom_directives: The current config has rules the NexaCloud template doesn't reproduce: {}. After adopting, put them in /etc/nginx/nexacloud/{}.custom.conf, or keep the site unmanaged.",
            custom.join("; "),
            p.id
        ));
    }
    let m = SiteModel {
        id: p.id.clone(),
        kind,
        domains,
        project_dir: root
            .as_deref()
            .and_then(|r| std::path::Path::new(r).parent())
            .map(|d| d.to_string_lossy().into_owned()),
        root,
        php_version,
        runtime: Runtime::Fpm,
        runtime_port: None,
        upstream: site.proxy_pass.clone(),
        redirect_to: None,
        redirect_code: None,
        ssl: site.ssl() && site.ssl_certificate.is_some() && ssl_key.is_some(),
        options: SiteOptions {
            client_max_body_size: "64M".into(),
            security_headers: false,
            hsts: false,
            cache_static: false,
            block_hidden: true,
        },
        fpm_socket,
        ssl_cert: site.ssl_certificate.clone(),
        ssl_key,
    };
    validate(&m)?;
    let mut b = ChangePlan::builder(
        "site_adopt",
        &format!("Manage {} with NexaCloud", p.id),
        &format!("site:{}", p.id),
    )
    .lock(&format!("site:{}", p.id))
    .lock("nginx")
    .create_parents();
    for f in &files {
        let fs = f.to_string_lossy().into_owned();
        let plan_path = fs
            .strip_prefix(&env.root.to_string_lossy().trim_end_matches('/').to_string())
            .map(String::from)
            .unwrap_or(fs.clone());
        let plan_path = if plan_path.starts_with('/') {
            plan_path
        } else {
            format!("/{plan_path}")
        };
        let meta = std::fs::symlink_metadata(f).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            // sites-enabled symlink: disable it (the target in sites-available stays).
            let target = std::fs::read_link(f).map_err(|e| e.to_string())?;
            b = b.command(["rm", "-f", &plan_path]).on_revert([
                "ln",
                "-sfn",
                &target.to_string_lossy(),
                &plan_path,
            ]);
        } else {
            // A real file: keep a disabled copy, then remove it (snapshotted).
            let text = std::fs::read_to_string(f).map_err(|e| e.to_string())?;
            let name = std::path::Path::new(&plan_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            b = b
                .write_file(&format!("{CUSTOM_DIR}/adopted/{name}.disabled"), text)
                .delete_file(&plan_path);
        }
    }
    b = b.write_file(&conf_path(&p.id), render(&m));
    // Commands: the symlink removal happens before nginx -t (commands run in order).
    let mut b2 = b
        .command(["nginx", "-t"])
        .command(["systemctl", "reload", "nginx"])
        .verify_command(["systemctl", "is-active", "--quiet", "nginx"])
        .on_revert(["systemctl", "reload", "nginx"]);
    b2 = probe(b2, &m);
    let note = if custom.is_empty() {
        "Replaces the hand-written config with a NexaCloud-managed one serving the same domains, web root, PHP version and certificate. The original stays on disk (disabled). If the site doesn't answer afterwards, everything is put back automatically.".to_string()
    } else {
        format!("Custom rules that are NOT carried over: {}. Re-add them in /etc/nginx/nexacloud/{}.custom.conf. The original config stays on disk (disabled).", custom.join("; "), p.id)
    };
    b2.note(note).build(env).map_err(|e| format!("{e:#}"))
}
