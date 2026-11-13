//! Listing and downloading backup files under `backup_dir`.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::fmt_ts;

const ENGINES: [&str; 2] = ["mysql", "postgres"];

#[derive(Debug, Clone, Serialize)]
pub struct BackupFile {
    pub engine: String,
    pub database: String,
    pub file: String,
    pub size_bytes: u64,
    pub created_at: String,
    #[serde(skip)]
    sort_key: DateTime<Utc>,
}

/// `shop_prod-20261113-030000.sql.gz` → 2026-11-13T03:00:00Z
pub fn timestamp_from_name(file_name: &str) -> Option<DateTime<Utc>> {
    let stem = file_name.strip_suffix(".sql.gz")?;
    if stem.len() < 15 {
        return None;
    }
    let ts = &stem[stem.len() - 15..];
    NaiveDateTime::parse_from_str(ts, "%Y%m%d-%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}

pub fn scan(backup_dir: &Path) -> Vec<BackupFile> {
    let mut out = Vec::new();
    for engine in ENGINES {
        let Ok(dbs) = std::fs::read_dir(backup_dir.join(engine)) else {
            continue;
        };
        for db in dbs.flatten() {
            let Ok(ft) = db.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let database = db.file_name().to_string_lossy().into_owned();
            let Ok(files) = std::fs::read_dir(db.path()) else {
                continue;
            };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || !name.ends_with(".sql.gz") {
                    continue;
                }
                // Regular files only (symlinks are not followed in the listing).
                let Ok(meta) = std::fs::symlink_metadata(f.path()) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                let created = timestamp_from_name(&name)
                    .or_else(|| meta.modified().ok().map(DateTime::<Utc>::from))
                    .unwrap_or_default();
                out.push(BackupFile {
                    engine: engine.to_string(),
                    file: format!("{engine}/{database}/{name}"),
                    database: database.clone(),
                    size_bytes: meta.len(),
                    created_at: fmt_ts(created),
                    sort_key: created,
                });
            }
        }
    }
    out.sort_by(|a, b| b.sort_key.cmp(&a.sort_key).then(b.file.cmp(&a.file)));
    out
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<BackupFile>> {
    let dir = state.config.backup_dir.clone();
    Json(
        tokio::task::spawn_blocking(move || scan(&dir))
            .await
            .unwrap_or_default(),
    )
}

#[derive(Debug, PartialEq)]
pub enum PathError {
    Invalid,
    NotFound,
}

/// Resolve a client-supplied relative path to a file that is guaranteed (after
/// canonicalisation, i.e. with symlinks resolved) to live inside `base`.
pub fn resolve_inside(base: &Path, rel: &str) -> Result<PathBuf, PathError> {
    if rel.is_empty() || rel.contains('\0') {
        return Err(PathError::Invalid);
    }
    let relp = Path::new(rel);
    if relp
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        // Rejects absolute paths, `..`, `.` and prefixes.
        return Err(PathError::Invalid);
    }
    let base = std::fs::canonicalize(base).map_err(|_| PathError::NotFound)?;
    let full = std::fs::canonicalize(base.join(relp)).map_err(|_| PathError::NotFound)?;
    if !full.starts_with(&base) || full == base {
        return Err(PathError::Invalid);
    }
    if !full.is_file() {
        return Err(PathError::Invalid);
    }
    Ok(full)
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    file: String,
}

fn safe_filename(p: &Path) -> String {
    let name: String = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if name.is_empty() {
        "backup.sql.gz".into()
    } else {
        name
    }
}

pub async fn download(
    State(state): State<AppState>,
    Query(q): Query<DownloadQuery>,
) -> ApiResult<Response> {
    let path = match resolve_inside(&state.config.backup_dir, &q.file) {
        Ok(p) => p,
        Err(PathError::Invalid) => return Err(ApiError::bad_request("invalid backup file path")),
        Err(PathError::NotFound) => return Err(ApiError::not_found("backup file not found")),
    };
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("backup file not found"))?;
    let len = file.metadata().await.map(|m| m.len()).ok();
    let mut resp = Body::from_stream(ReaderStream::new(file)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        safe_filename(&path)
    )) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(len) = len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("nexacloud-bk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn path_validation() {
        let root = tmpdir();
        let base = root.join("backups");
        std::fs::create_dir_all(base.join("mysql/shop")).unwrap();
        std::fs::write(base.join("mysql/shop/shop-20261113-030000.sql.gz"), b"x").unwrap();
        std::fs::write(root.join("secret.txt"), b"s").unwrap();
        std::os::unix::fs::symlink(root.join("secret.txt"), base.join("mysql/shop/link.sql.gz"))
            .unwrap();

        assert!(resolve_inside(&base, "mysql/shop/shop-20261113-030000.sql.gz").is_ok());
        assert_eq!(
            resolve_inside(&base, "../secret.txt"),
            Err(PathError::Invalid)
        );
        assert_eq!(
            resolve_inside(&base, "mysql/../../secret.txt"),
            Err(PathError::Invalid)
        );
        assert_eq!(
            resolve_inside(&base, "/etc/passwd"),
            Err(PathError::Invalid)
        );
        assert_eq!(
            resolve_inside(&base, "./mysql/shop/shop-20261113-030000.sql.gz"),
            Err(PathError::Invalid)
        );
        assert_eq!(resolve_inside(&base, ""), Err(PathError::Invalid));
        assert_eq!(resolve_inside(&base, "a\0b"), Err(PathError::Invalid));
        // Symlink escaping the backup dir.
        assert_eq!(
            resolve_inside(&base, "mysql/shop/link.sql.gz"),
            Err(PathError::Invalid)
        );
        // Directory, not a file.
        assert_eq!(resolve_inside(&base, "mysql/shop"), Err(PathError::Invalid));
        assert_eq!(
            resolve_inside(&base, "mysql/shop/missing.sql.gz"),
            Err(PathError::NotFound)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_newest_first_and_skips_partials() {
        let base = tmpdir();
        std::fs::create_dir_all(base.join("mysql/shop")).unwrap();
        std::fs::create_dir_all(base.join("postgres/app")).unwrap();
        std::fs::write(base.join("mysql/shop/shop-20261112-030000.sql.gz"), b"aa").unwrap();
        std::fs::write(base.join("postgres/app/app-20261113-030000.sql.gz"), b"b").unwrap();
        std::fs::write(
            base.join("mysql/shop/.shop-20261113-040000.sql.gz.partial"),
            b"",
        )
        .unwrap();
        std::fs::write(base.join("mysql/shop/notes.txt"), b"").unwrap();
        let v = scan(&base);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].file, "postgres/app/app-20261113-030000.sql.gz");
        assert_eq!(v[0].created_at, "2026-11-13T03:00:00Z");
        assert_eq!(v[1].engine, "mysql");
        assert_eq!(v[1].database, "shop");
        assert_eq!(v[1].size_bytes, 2);
        assert!(scan(Path::new("/nonexistent/backups")).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn name_timestamps() {
        assert_eq!(
            fmt_ts(timestamp_from_name("my-db-20261113-030000.sql.gz").unwrap()),
            "2026-11-13T03:00:00Z"
        );
        assert!(timestamp_from_name("x.sql.gz").is_none());
    }
}
