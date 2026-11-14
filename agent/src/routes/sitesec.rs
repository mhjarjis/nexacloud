//! Site security helpers outside the change pipeline:
//! - users of password-protected folders (htpasswd, hashed with SHA-512 crypt)
//! - the server-wide IP block list (read side; writes are the `ip_block` plan)
//! - importing a `.sql` / `.sql.gz` file into a database (backup first)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::audit::{audited, AuditInfo};
use crate::auth::Identity;
use crate::error::{ApiError, ApiResult};
use crate::jobs::JobSpec;
use crate::plans::sites::{htpasswd_path, valid_site_id, HTPASSWD_DIR};
use crate::routes::databases::{dump_into, mysql_info, postgres_info, validate_backup_params, Engine};
use crate::state::AppState;
use crate::util::cmd::Cmd;

fn forbidden(m: &str) -> ApiError {
    ApiError::new(axum::http::StatusCode::FORBIDDEN, m)
}

// ------------------------------------------------------------------ htpasswd users

fn read_users(id: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(htpasswd_path(id))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(':').map(|(u, h)| (u.to_string(), h.to_string())))
        .collect()
}

/// `GET /api/sites/{id}/auth-users` — names only.
pub async fn auth_users(Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    if !valid_site_id(&id) {
        return Err(ApiError::bad_request("invalid site"));
    }
    let _ = me;
    let names: Vec<String> = read_users(&id).into_iter().map(|(u, _)| u).collect();
    Ok(Json(json!({ "users": names })))
}

#[derive(Deserialize)]
pub struct AuthUser {
    name: String,
    /// New password; omit to keep the current one.
    password: Option<String>,
}
#[derive(Deserialize)]
pub struct AuthUsersBody {
    users: Vec<AuthUser>,
}

fn valid_login(u: &str) -> bool {
    !u.is_empty() && u.len() <= 32 && u.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
}

async fn crypt(password: &str) -> ApiResult<String> {
    let o = Cmd::new("openssl").args(["passwd", "-6", "-stdin"]).stdin(format!("{password}\n")).timeout(Duration::from_secs(10)).run().await;
    let h = o.stdout.trim().to_string();
    if !o.ok || !h.starts_with("$6$") {
        return Err(ApiError::internal(o.error_summary("openssl passwd")));
    }
    Ok(h)
}

/// `PUT /api/sites/{id}/auth-users` — replace the list (passwords kept unless given).
pub async fn auth_users_put(Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>, Json(b): Json<AuthUsersBody>) -> Response {
    let info = AuditInfo::action("site.auth_users", id.clone());
    let res = async {
        if !valid_site_id(&id) {
            return Err(ApiError::bad_request("invalid site"));
        }
        if !me.can_change_project(&id) {
            return Err(forbidden("you can't change that site"));
        }
        if b.users.len() > 100 {
            return Err(ApiError::bad_request("at most 100 users"));
        }
        let old = read_users(&id);
        let mut lines = Vec::new();
        let mut seen = Vec::new();
        for u in &b.users {
            let name = u.name.trim();
            if !valid_login(name) {
                return Err(ApiError::bad_request(format!("username {name:?}: letters, digits, . _ - @ (max 32)")));
            }
            if seen.contains(&name.to_string()) {
                continue;
            }
            seen.push(name.to_string());
            let hash = match u.password.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => {
                    if p.chars().count() < 8 || p.len() > 200 || p.contains('\n') {
                        return Err(ApiError::bad_request("passwords: at least 8 characters"));
                    }
                    crypt(p).await?
                }
                None => old.iter().find(|(n, _)| n == name).map(|(_, h)| h.clone()).ok_or_else(|| ApiError::bad_request(format!("set a password for {name}")))?,
            };
            lines.push(format!("{name}:{hash}"));
        }
        std::fs::create_dir_all(HTPASSWD_DIR).map_err(|e| ApiError::internal(e.to_string()))?;
        let gid = nix_gid("www-data");
        if let Some(g) = gid {
            let _ = std::os::unix::fs::chown(HTPASSWD_DIR, Some(0), Some(g));
            let _ = std::fs::set_permissions(HTPASSWD_DIR, std::os::unix::fs::PermissionsExt::from_mode(0o750));
        }
        let body = if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" };
        crate::util::atomic_write(std::path::Path::new(&htpasswd_path(&id)), body.as_bytes(), 0o640, gid.map(|g| (0, g)))
            .map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(Json(json!({ "users": seen })).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn nix_gid(group: &str) -> Option<u32> {
    std::fs::read_to_string("/etc/group").ok()?.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() > 2 && f[0] == group).then(|| f[2].parse().ok()).flatten()
    })
}

// ------------------------------------------------------------------ block list

pub async fn blocklist(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "ips": crate::plans::sites::read_blocklist(&state.changes_env) }))
}

// ------------------------------------------------------------------ SQL import

#[derive(Deserialize)]
pub struct ImportQ {
    name: String,
    /// Back up the database first (default true).
    backup: Option<bool>,
}

/// `POST /api/dbm/{engine}/{db}/import?name=dump.sql.gz` — raw body. The
/// file is saved, the database is backed up, then the file is imported in a job.
pub async fn sql_import(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath((engine, db)): UrlPath<(String, String)>,
    Query(q): Query<ImportQ>,
    body: Body,
) -> Response {
    let mut info = AuditInfo::action("db.import", format!("{engine}/{db}: {}", q.name));
    let res: ApiResult<String> = async {
        let engine = validate_backup_params(&engine, &db)?;
        let gz = q.name.ends_with(".sql.gz") || q.name.ends_with(".gz");
        if !(gz || q.name.ends_with(".sql")) {
            return Err(ApiError::bad_request("upload a .sql or .sql.gz file"));
        }
        let listing = match engine {
            Engine::Mysql => mysql_info().await,
            Engine::Postgres => postgres_info().await,
        };
        if !listing.databases.iter().any(|d| d.name == db) {
            return Err(ApiError::not_found(format!("database not found: {db}")));
        }
        let dir = state.config.data_dir.join("imports");
        crate::util::create_private_dir(&dir).map_err(|e| ApiError::internal(format!("{e:#}")))?;
        let file: PathBuf = dir.join(format!("{}.sql{}", uuid::Uuid::new_v4().simple(), if gz { ".gz" } else { "" }));
        {
            use std::os::unix::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).map_err(|e| ApiError::internal(e.to_string()))?;
            let mut f = tokio::fs::File::from_std(f);
            let mut stream = body.into_data_stream();
            let mut size = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = std::fs::remove_file(&file);
                        return Err(ApiError::bad_request(format!("upload interrupted: {e}")));
                    }
                };
                size += chunk.len() as u64;
                if f.write_all(&chunk).await.is_err() {
                    let _ = std::fs::remove_file(&file);
                    return Err(ApiError::internal("cannot save the upload (disk full?)"));
                }
            }
            f.flush().await.ok();
            if size == 0 {
                let _ = std::fs::remove_file(&file);
                return Err(ApiError::bad_request("the file is empty"));
            }
        }
        let backup = q.backup.unwrap_or(true);
        let spec = JobSpec::new("db_import", &format!("Import {} into {}/{db}", q.name, engine.as_str()))
            .heavy()
            .lock(format!("db:{}/{db}", engine.as_str()))
            .timeout(Duration::from_secs(4 * 3600))
            .actor(Some(addr.ip().to_canonical().to_string()));
        let st = state.clone();
        let id = state.jobs.submit(spec, move |ctx| async move {
            let _cleanup = Remove(file.clone());
            if backup {
                ctx.log("Backing up the database first…");
                let bdir = st.config.backup_dir.join(engine.as_str()).join(&db);
                for d in [&st.config.backup_dir, &st.config.backup_dir.join(engine.as_str()), &bdir] {
                    crate::util::create_private_dir(d)?;
                }
                let p = dump_into(&ctx, engine, &db, &bdir).await?;
                ctx.log(&format!("Backup saved: {}", p.display()));
            }
            ctx.log("Importing…");
            let reader = if gz { "gunzip -c \"$1\"" } else { "cat \"$1\"" };
            let script = match engine {
                Engine::Mysql => format!("set -o pipefail; {reader} | mysql \"$2\""),
                Engine::Postgres => format!("set -o pipefail; {reader} | runuser -u postgres -- psql -q -v ON_ERROR_STOP=1 -d \"$2\""),
            };
            let o = Cmd::new("bash").args(["-c", &script, "import", &file.to_string_lossy(), &db]).timeout(Duration::from_secs(4 * 3600)).run().await;
            for l in o.combined().lines().take(200) {
                ctx.log(l);
            }
            if !o.ok {
                anyhow::bail!("import failed{}", if backup { " — the backup taken before is under Backups" } else { "" });
            }
            ctx.log("Done — imported.");
            Ok(())
        })?;
        Ok(id)
    }
    .await;
    if let Ok(id) = &res {
        info.job_id = Some(id.clone());
    }
    audited(res.map(|id| Json(json!({ "job_id": id })).into_response()).unwrap_or_else(IntoResponse::into_response), info)
}

struct Remove(PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
