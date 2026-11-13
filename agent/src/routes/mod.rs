pub mod backups;
pub mod changes;
pub mod databases;
pub mod domains;
pub mod jobs;
pub mod php;
pub mod security;
pub mod services;
pub mod sites;
pub mod system;
pub mod test_hooks;

use std::path::Path;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::alerts::{self, Alert, SiteSsl};
use crate::audit;
use crate::auth;
use crate::backups::routes as bk;
use crate::deploys::routes as dp;
use crate::error::{json_errors, ApiError};
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(auth::login));
    // GitHub webhook: public, authenticated by its HMAC signature (no Origin).
    let hooks = Router::new().route("/hooks/github/{id}", post(dp::github_hook));

    // Dangerous actions: need sudo mode (password re-entered < 15 min ago).
    // Later phases add their mutating endpoints here.
    let mut dangerous = Router::new()
        .route("/services/{unit}/restart", post(services::restart))
        .route("/jobs/{id}/cancel", post(jobs::cancel))
        .route("/changes/{id}/rollback", post(changes::rollback))
        .route(
            "/advisor/nginx-hardening/apply",
            post(changes::nginx_hardening_apply),
        )
        .route("/plans/apply", post(crate::plans::apply))
        .route("/backup/destinations", post(bk::create_destination))
        .route(
            "/backup/destinations/{id}",
            delete(bk::delete_destination_route),
        )
        .route("/backup/plans", post(bk::create_plan))
        .route(
            "/backup/plans/{id}",
            put(bk::update_plan).delete(bk::delete_plan_route),
        )
        .route("/backup/runs/{id}/restore", post(bk::restore_run))
        .route("/backup/runs/{id}/staging", post(bk::staging_run))
        .route(
            "/dbm/{engine}/{db}/query/write",
            post(crate::dbm::query_write),
        )
        .route(
            "/dbm/{engine}/{db}/tables/{table}/rows/{action}",
            post(crate::dbm::row_action),
        )
        .route("/dbm/{engine}/databases", post(crate::dbm::create_database))
        .route(
            "/dbm/{engine}/users/password",
            post(crate::dbm::reset_password),
        )
        .route("/projects", post(dp::create))
        .route("/projects/{id}", put(dp::update).delete(dp::remove))
        .route("/projects/{id}/deploy", post(dp::deploy))
        .route("/projects/{id}/artisan", post(dp::artisan))
        .route("/firewall/confirm", post(crate::plans::firewall_confirm))
        .route("/projects/{id}/env/reveal", post(dp::env_reveal))
        .route("/projects/{id}/env/raw", post(dp::env_raw))
        .route("/projects/{id}/workers/restart", post(dp::workers_restart))
        .route("/notify/settings", put(crate::monitor::routes::settings_put))
        .route("/notify/test", post(crate::monitor::routes::test))
        .route("/apps", post(crate::apps::create))
        .route("/projects/{id}/releases/{name}/activate", post(dp::release_activate));
    if state.test_hooks {
        tracing::warn!("NEXACLOUD_TEST_HOOKS=1: test-only endpoints are enabled");
        dangerous = dangerous
            .route("/test-hooks/broken-change", post(test_hooks::broken_change))
            .route("/test-hooks/sleep-job", post(test_hooks::sleep_job));
    }
    let dangerous = dangerous.route_layer(from_fn_with_state(state.clone(), auth::require_sudo));

    let protected = Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/sudo", post(auth::sudo))
        .route("/overview", get(overview))
        .route("/system", get(system::get))
        .route("/services", get(services::list))
        .route("/php", get(php::get))
        .route("/sites", get(sites::list))
        .route("/databases", get(databases::list))
        .route("/databases/{engine}/{name}/backup", post(databases::backup))
        .route("/backups", get(backups::list))
        .route("/backups/download", get(backups::download))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::get))
        .route("/jobs/{id}/stream", get(jobs::stream))
        .route("/changes", get(changes::list))
        .route("/changes/{id}", get(changes::get))
        .route(
            "/advisor/nginx-hardening/preview",
            post(changes::nginx_hardening_preview),
        )
        .route("/audit", get(changes::audit_list))
        .route("/plans/preview", post(crate::plans::preview))
        .route("/sites/{id}/model", get(crate::plans::site_model))
        .route("/nginx/file", get(crate::plans::nginx_file))
        .route("/php/{version}/config", get(crate::plans::php_config))
        .route("/catalog", get(crate::plans::catalog))
        .route("/advisor", get(crate::plans::advisor))
        .route("/advisor/{id}/dismiss", post(crate::plans::advisor_dismiss))
        .route("/backup/destinations", get(bk::destinations))
        .route(
            "/backup/destinations/{id}/test",
            post(bk::test_destination_route),
        )
        .route("/backup/ssh-key", get(bk::ssh_key))
        .route("/backup/plans", get(bk::plans))
        .route("/backup/plans/{id}/run", post(bk::run_plan))
        .route("/backup/runs", get(bk::runs))
        .route("/disk", get(bk::disk))
        .route("/dbm/{engine}/users", get(crate::dbm::users))
        .route("/dbm/{engine}/stats", get(crate::dbm::stats))
        .route("/dbm/{engine}/{db}/tables", get(crate::dbm::tables))
        .route(
            "/dbm/{engine}/{db}/tables/{table}/structure",
            get(crate::dbm::structure),
        )
        .route(
            "/dbm/{engine}/{db}/tables/{table}/rows",
            get(crate::dbm::rows),
        )
        .route("/dbm/{engine}/{db}/query", post(crate::dbm::query_read))
        .route("/projects", get(dp::list))
        .route("/projects/{id}", get(dp::get))
        .route("/projects/{id}/deploys", get(dp::deploys))
        .route("/projects/{id}/logs", get(dp::logs))
        .route("/projects/{id}/env", get(dp::env_get))
        .route("/projects/{id}/status", get(dp::status))
        .route("/projects/{id}/overview", get(dp::overview))
        .route("/projects/{id}/releases", get(dp::releases))
        .route("/apps/defaults", get(crate::apps::defaults))
        .route("/domains", get(domains::domains))
        .route("/system/history", get(crate::monitor::routes::server_history))
        .route("/workspace", get(domains::workspace))
        .route("/monitor", get(crate::monitor::routes::list))
        .route("/monitor/{site}", get(crate::monitor::routes::site))
        .route("/notifications", get(crate::monitor::routes::notifications))
        .route("/notify/settings", get(crate::monitor::routes::settings_get))
        .route("/deploys", get(dp::all_deploys))
        .route("/firewall/guard", get(crate::plans::firewall_guard))
        .route("/security", get(security::get))
        .merge(dangerous)
        .route_layer(from_fn_with_state(state.clone(), auth::require_session));

    let api = public
        .merge(protected)
        .fallback(api_not_found)
        .layer(from_fn(auth::origin_check))
        .layer(from_fn(json_errors))
        .layer(from_fn(no_store))
        .layer(from_fn_with_state(state.clone(), audit::middleware));

    let web_dir = state.config.web_dir.clone();
    let hooks = hooks
        .layer(from_fn(json_errors))
        .layer(from_fn(no_store))
        .layer(from_fn_with_state(state.clone(), audit::middleware));
    let app = Router::new().nest("/api", api).merge(hooks);
    let app = if web_ui_installed(&web_dir) {
        let spa = ServeDir::new(&web_dir)
            // No directory redirects (/databases -> /databases/): every
            // unknown path falls through to the SPA shell with a 200.
            .append_index_html_on_directories(false)
            .fallback(ServeFile::new(web_dir.join("index.html")));
        app.fallback_service(spa)
    } else {
        tracing::warn!(
            "web UI not found at {} (no index.html); serving placeholder",
            web_dir.display()
        );
        app.fallback(no_ui)
    };
    app.layer(from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn web_ui_installed(web_dir: &Path) -> bool {
    web_dir.join("index.html").is_file()
}

async fn no_ui() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "NexaCloud agent is running; web UI not installed\n",
    )
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("not found")
}

async fn no_store(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    resp
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

// -------------------------------------------------------------- overview

#[derive(Serialize)]
struct Counts {
    sites: usize,
    databases: usize,
    services_down: usize,
}

#[derive(Serialize)]
struct Overview {
    hostname: String,
    alerts: Vec<Alert>,
    counts: Counts,
}

/// The dashboard's warnings, computed now.
pub async fn current_alerts() -> Vec<alerts::Alert> {
    let (svc, sec, site_list, disks) = tokio::join!(
        services::statuses(false),
        security::info(),
        sites::scan(false),
        async {
            tokio::task::spawn_blocking(system::disks)
                .await
                .unwrap_or_default()
        },
    );
    let php_info = php::info(&site_list).await;
    let ssl: Vec<SiteSsl> = site_list
        .iter()
        .map(|s| SiteSsl {
            id: &s.id,
            ssl_expires: s.ssl_expires.as_deref(),
        })
        .collect();
    alerts::compute(&alerts::Inputs {
        now: chrono::Utc::now(),
        php: &php_info.versions,
        ufw_installed: sec.ufw.installed,
        ufw_active: sec.ufw.active,
        fail2ban_installed: sec.fail2ban.installed,
        services: &svc,
        sites: &ssl,
        disks: &disks,
    })
}

async fn overview() -> Json<Overview> {
    let (svc, sec, dbs, site_list, disks) = tokio::join!(
        services::statuses(false),
        security::info(),
        databases::info(),
        sites::scan(false),
        async {
            tokio::task::spawn_blocking(system::disks)
                .await
                .unwrap_or_default()
        },
    );
    let php_info = php::info(&site_list).await;
    let ssl: Vec<SiteSsl> = site_list
        .iter()
        .map(|s| SiteSsl {
            id: &s.id,
            ssl_expires: s.ssl_expires.as_deref(),
        })
        .collect();
    let alerts = alerts::compute(&alerts::Inputs {
        now: chrono::Utc::now(),
        php: &php_info.versions,
        ufw_installed: sec.ufw.installed,
        ufw_active: sec.ufw.active,
        fail2ban_installed: sec.fail2ban.installed,
        services: &svc,
        sites: &ssl,
        disks: &disks,
    });
    Json(Overview {
        hostname: system::hostname(),
        alerts,
        counts: Counts {
            sites: site_list.len(),
            databases: dbs.mysql.databases.len() + dbs.postgres.databases.len(),
            services_down: svc.iter().filter(|s| s.installed && !s.active).count(),
        },
    })
}
