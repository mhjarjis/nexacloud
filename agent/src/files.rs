//! File Manager: browse, edit, upload, download, rename, delete, chmod,
//! compress and extract — inside allowed folders only.
//!
//! Admins: `/var/www`, `/srv`, `/home` (read/write) plus `/var/log`,
//! `/etc/nginx`, `/etc/php` (read only). Developers: their projects' folders.
//! Every path is canonicalized (symlinks resolved) and must stay inside a
//! root. New files and folders get the owner of the folder they are created
//! in.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::auth::Identity;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::users::Role;
use crate::util::cmd::Cmd;

const RW_ROOTS: [&str; 3] = ["/var/www", "/srv", "/home"];
const RO_ROOTS: [&str; 3] = ["/var/log", "/etc/nginx", "/etc/php"];
const MAX_EDIT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Roots {
    rw: Vec<PathBuf>,
    ro: Vec<PathBuf>,
}

fn roots(state: &AppState, me: &Identity) -> Roots {
    if me.role == Role::Admin {
        return Roots { rw: RW_ROOTS.iter().map(PathBuf::from).collect(), ro: RO_ROOTS.iter().map(PathBuf::from).collect() };
    }
    let dirs: Vec<PathBuf> = crate::deploys::list_projects(&state.db)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| me.projects.contains(&p.id))
        .map(|p| PathBuf::from(p.dir))
        .collect();
    if me.role == Role::Viewer {
        // viewers never see project files (.env holds secrets)
        Roots { rw: vec![], ro: vec![] }
    } else {
        Roots { rw: dirs, ro: vec![] }
    }
}

fn forbidden(m: &str) -> ApiError {
    ApiError::new(axum::http::StatusCode::FORBIDDEN, m)
}

/// Resolve `p` (which may not exist yet: then its parent must) and check it
/// is inside a root. Returns the real path.
fn resolve(roots: &Roots, p: &str, write: bool) -> ApiResult<PathBuf> {
    if !p.starts_with('/') || p.contains('\0') {
        return Err(ApiError::bad_request("use an absolute path"));
    }
    let path = Path::new(p);
    let real = match std::fs::canonicalize(path) {
        Ok(r) => r,
        Err(_) => {
            let parent = path.parent().ok_or_else(|| ApiError::bad_request("invalid path"))?;
            let name = path.file_name().ok_or_else(|| ApiError::bad_request("invalid path"))?;
            let name_s = name.to_string_lossy();
            if name_s == "." || name_s == ".." || name_s.contains('/') {
                return Err(ApiError::bad_request("invalid name"));
            }
            std::fs::canonicalize(parent).map_err(|_| ApiError::not_found("folder not found"))?.join(name)
        }
    };
    let inside = |rs: &[PathBuf]| rs.iter().any(|r| real.starts_with(r));
    if inside(&roots.rw) || (!write && inside(&roots.ro)) {
        Ok(real)
    } else {
        Err(forbidden(if write { "you can't change files there" } else { "outside the folders you can open" }))
    }
}

fn owner_of(dir: &Path) -> Option<(u32, u32)> {
    std::fs::metadata(dir).ok().map(|m| (m.uid(), m.gid()))
}

fn chown_like_parent(p: &Path) {
    if let Some((u, g)) = p.parent().and_then(owner_of) {
        let _ = std::os::unix::fs::lchown(p, Some(u), Some(g));
    }
}

#[derive(Serialize)]
struct Entry {
    name: String,
    kind: &'static str,
    size: u64,
    mode: String,
    owner: String,
    modified: i64,
    link: Option<String>,
}

#[derive(Deserialize)]
pub struct PathQ {
    path: Option<String>,
}

/// `GET /api/files?path=` — a folder listing (or the list of roots).
pub async fn list(State(state): State<AppState>, Extension(me): Extension<Identity>, Query(q): Query<PathQ>) -> ApiResult<Json<serde_json::Value>> {
    let r = roots(&state, &me);
    let Some(p) = q.path.filter(|p| !p.is_empty() && p != "/") else {
        let rs: Vec<serde_json::Value> = r.rw.iter().map(|p| json!({"path": p, "writable": true}))
            .chain(r.ro.iter().map(|p| json!({"path": p, "writable": false})))
            .filter(|v| Path::new(v["path"].as_str().unwrap_or("")).exists())
            .collect();
        return Ok(Json(json!({"roots": rs})));
    };
    let dir = resolve(&r, &p, false)?;
    let writable = resolve(&r, &p, true).is_ok();
    let entries = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<Entry>> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&dir)?.flatten().take(5000) {
            let Ok(lm) = e.metadata().or_else(|_| std::fs::symlink_metadata(e.path())) else { continue };
            let sm = std::fs::symlink_metadata(e.path()).ok();
            let is_link = sm.as_ref().is_some_and(|m| m.file_type().is_symlink());
            v.push(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                kind: if lm.is_dir() { "dir" } else { "file" },
                size: if lm.is_dir() { 0 } else { lm.len() },
                mode: format!("{:o}", lm.permissions().mode() & 0o7777),
                owner: crate::util::user_name(lm.uid()).unwrap_or_else(|| lm.uid().to_string()),
                modified: lm.mtime(),
                link: if is_link { std::fs::read_link(e.path()).ok().map(|t| t.to_string_lossy().into_owned()) } else { None },
            });
        }
        v.sort_by(|a, b| (a.kind != "dir").cmp(&(b.kind != "dir")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(v)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::bad_request(format!("cannot open the folder: {e}")))?;
    Ok(Json(json!({"path": p, "writable": writable, "entries": entries})))
}

/// `GET /api/files/read?path=` — a text file for the editor (≤ 2 MB).
pub async fn read(State(state): State<AppState>, Extension(me): Extension<Identity>, Query(q): Query<PathQ>) -> ApiResult<Json<serde_json::Value>> {
    let r = roots(&state, &me);
    let p = q.path.ok_or_else(|| ApiError::bad_request("path is required"))?;
    let real = resolve(&r, &p, false)?;
    let md = std::fs::metadata(&real).map_err(|_| ApiError::not_found("file not found"))?;
    if !md.is_file() {
        return Err(ApiError::bad_request("not a file"));
    }
    if md.len() > MAX_EDIT {
        return Err(ApiError::bad_request("file is larger than 2 MB — download it instead"));
    }
    let bytes = std::fs::read(&real).map_err(|e| ApiError::internal(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|_| ApiError::bad_request("binary file — download it instead"))?;
    let writable = resolve(&r, &p, true).is_ok();
    Ok(Json(json!({"path": p, "content": text, "size": md.len(), "mode": format!("{:o}", md.permissions().mode() & 0o7777), "writable": writable})))
}

/// `GET /api/files/download?path=`
pub async fn download(State(state): State<AppState>, Extension(me): Extension<Identity>, Query(q): Query<PathQ>) -> ApiResult<Response> {
    let r = roots(&state, &me);
    let p = q.path.ok_or_else(|| ApiError::bad_request("path is required"))?;
    let real = resolve(&r, &p, false)?;
    if !real.is_file() {
        return Err(ApiError::bad_request("only files can be downloaded — compress a folder first"));
    }
    let f = tokio::fs::File::open(&real).await.map_err(|_| ApiError::not_found("file not found"))?;
    let len = f.metadata().await.map(|m| m.len()).ok();
    let mut resp = Body::from_stream(tokio_util::io::ReaderStream::new(f)).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    let name: String = real.file_name().map(|n| n.to_string_lossy().replace(['"', '\\'], "_")).unwrap_or_else(|| "file".into());
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(l) = len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(l));
    }
    Ok(resp)
}

fn audit(res: ApiResult<Response>, info: AuditInfo) -> Response {
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

#[derive(Deserialize)]
pub struct WriteBody {
    path: String,
    content: String,
}

/// `POST /api/files/write` — save a text file (keeps owner and mode).
pub async fn write(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<WriteBody>) -> Response {
    let info = AuditInfo::action("files.write", b.path.clone());
    let res = (|| {
        let real = resolve(&roots(&state, &me), &b.path, true)?;
        if b.content.len() as u64 > MAX_EDIT {
            return Err(ApiError::bad_request("too large for the editor"));
        }
        let existed = real.exists();
        let (mode, owner) = match std::fs::metadata(&real) {
            Ok(m) => (m.permissions().mode() & 0o7777, Some((m.uid(), m.gid()))),
            Err(_) => (0o644, real.parent().and_then(owner_of)),
        };
        crate::util::atomic_write(&real, b.content.as_bytes(), mode, owner).map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(Json(json!({"ok": true, "created": !existed})).into_response())
    })();
    audit(res, info)
}

#[derive(Deserialize)]
pub struct UploadQ {
    dir: String,
    name: String,
}

/// `POST /api/files/upload?dir=&name=` (raw body streamed to disk, up to 2 GB).
pub async fn upload(State(state): State<AppState>, Extension(me): Extension<Identity>, Query(q): Query<UploadQ>, body: Body) -> Response {
    let info = AuditInfo::action("files.upload", format!("{}/{}", q.dir, q.name));
    let res = async {
        if q.name.is_empty() || q.name.contains('/') || q.name == "." || q.name == ".." || q.name.len() > 255 {
            return Err(ApiError::bad_request("invalid file name"));
        }
        let dir = resolve(&roots(&state, &me), &q.dir, true)?;
        if !dir.is_dir() {
            return Err(ApiError::bad_request("upload into a folder"));
        }
        let target = dir.join(&q.name);
        let target = resolve(&roots(&state, &me), &target.to_string_lossy(), true)?;
        if target.is_dir() {
            return Err(ApiError::bad_request("a folder with that name exists"));
        }
        let tmp = target.with_file_name(format!(".{}.nc-upload", q.name));
        let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| ApiError::internal(e.to_string()))?;
        let mut size: u64 = 0;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(ApiError::bad_request(format!("upload interrupted: {e}")));
                }
            };
            size += chunk.len() as u64;
            if let Err(e) = f.write_all(&chunk).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(ApiError::internal(e.to_string()));
            }
        }
        f.flush().await.ok();
        drop(f);
        tokio::fs::rename(&tmp, &target).await.map_err(|e| ApiError::internal(e.to_string()))?;
        chown_like_parent(&target);
        Ok(Json(json!({"ok": true, "size": size})).into_response())
    }
    .await;
    audit(res, info)
}

#[derive(Deserialize)]
pub struct PathBody {
    path: String,
}

pub async fn mkdir(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<PathBody>) -> Response {
    let info = AuditInfo::action("files.mkdir", b.path.clone());
    let res = (|| {
        let real = resolve(&roots(&state, &me), &b.path, true)?;
        std::fs::create_dir(&real).map_err(|e| ApiError::bad_request(format!("cannot create folder: {e}")))?;
        let _ = std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755));
        chown_like_parent(&real);
        Ok(Json(json!({"ok": true})).into_response())
    })();
    audit(res, info)
}

#[derive(Deserialize)]
pub struct RenameBody {
    from: String,
    to: String,
}

pub async fn rename(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<RenameBody>) -> Response {
    let info = AuditInfo::action("files.rename", format!("{} → {}", b.from, b.to));
    let res = (|| {
        let r = roots(&state, &me);
        let from = resolve(&r, &b.from, true)?;
        let to = resolve(&r, &b.to, true)?;
        if r.rw.iter().any(|root| *root == from) {
            return Err(forbidden("the top folder can't be moved"));
        }
        if to.exists() {
            return Err(ApiError::bad_request("something with that name already exists"));
        }
        std::fs::rename(&from, &to).map_err(|e| ApiError::bad_request(format!("cannot move: {e}")))?;
        Ok(Json(json!({"ok": true})).into_response())
    })();
    audit(res, info)
}

#[derive(Deserialize)]
pub struct PathsBody {
    paths: Vec<String>,
}

pub async fn delete(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<PathsBody>) -> Response {
    let info = AuditInfo::action("files.delete", b.paths.join(", ").chars().take(200).collect::<String>());
    let res = async {
        let r = roots(&state, &me);
        let mut real = Vec::new();
        for p in &b.paths {
            let x = resolve(&r, p, true)?;
            if r.rw.iter().any(|root| *root == x) || x.matches_root() {
                return Err(forbidden("the top folder can't be deleted"));
            }
            real.push(x);
        }
        for x in real {
            let res = if std::fs::symlink_metadata(&x).map(|m| m.is_dir()).unwrap_or(false) { tokio::fs::remove_dir_all(&x).await } else { tokio::fs::remove_file(&x).await };
            res.map_err(|e| ApiError::bad_request(format!("cannot delete {}: {e}", x.display())))?;
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audit(res, info)
}

trait RootCheck {
    fn matches_root(&self) -> bool;
}
impl RootCheck for PathBuf {
    fn matches_root(&self) -> bool {
        self.components().count() <= 2
    }
}

#[derive(Deserialize)]
pub struct ChmodBody {
    path: String,
    mode: String,
    #[serde(default)]
    recursive: bool,
}

pub async fn chmod(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<ChmodBody>) -> Response {
    let info = AuditInfo::action("files.chmod", format!("{} {}", b.mode, b.path));
    let res = async {
        let real = resolve(&roots(&state, &me), &b.path, true)?;
        let mode = u32::from_str_radix(b.mode.trim(), 8).ok().filter(|m| *m <= 0o777).ok_or_else(|| ApiError::bad_request("mode like 644 or 755"))?;
        if mode & 0o002 != 0 {
            return Err(ApiError::bad_request("world-writable permissions (like 777) are not allowed — use 775 or 755"));
        }
        let mut c = Cmd::new("chmod");
        if b.recursive {
            c = c.arg("-R");
        }
        let o = c.args([&format!("{mode:o}"), &real.to_string_lossy().to_string()]).timeout(Duration::from_secs(300)).run().await;
        if !o.ok {
            return Err(ApiError::bad_request(o.error_summary("chmod")));
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audit(res, info)
}

#[derive(Deserialize)]
pub struct CompressBody {
    paths: Vec<String>,
    /// archive path to create (.zip or .tar.gz)
    dest: String,
}

async fn ensure_zip() {
    if !Path::new("/usr/bin/zip").exists() || !Path::new("/usr/bin/unzip").exists() {
        let _ = Cmd::new("apt-get").args(["install", "-y", "-q", "zip", "unzip"]).env("DEBIAN_FRONTEND", "noninteractive").timeout(Duration::from_secs(600)).run().await;
    }
}

pub async fn compress(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<CompressBody>) -> Response {
    let info = AuditInfo::action("files.compress", b.dest.clone());
    let res = async {
        let r = roots(&state, &me);
        let dest = resolve(&r, &b.dest, true)?;
        if dest.exists() {
            return Err(ApiError::bad_request("the archive already exists"));
        }
        let parent = dest.parent().ok_or_else(|| ApiError::bad_request("invalid destination"))?.to_path_buf();
        let mut names = Vec::new();
        for p in &b.paths {
            let x = resolve(&r, p, false)?;
            if x.parent() != Some(parent.as_path()) {
                return Err(ApiError::bad_request("compress files from the folder the archive goes into"));
            }
            names.push(x.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        if names.is_empty() {
            return Err(ApiError::bad_request("choose something to compress"));
        }
        let d = dest.to_string_lossy().to_string();
        let o = if d.ends_with(".zip") {
            ensure_zip().await;
            Cmd::new("zip").args(["-rq", &d, "--"]).args(&names).current_dir(&parent).timeout(Duration::from_secs(3600)).run().await
        } else if d.ends_with(".tar.gz") || d.ends_with(".tgz") {
            Cmd::new("tar").args(["-czf", &d, "--"]).args(&names).current_dir(&parent).timeout(Duration::from_secs(3600)).run().await
        } else {
            return Err(ApiError::bad_request("archive name must end in .zip or .tar.gz"));
        };
        if !o.ok {
            let _ = std::fs::remove_file(&dest);
            return Err(ApiError::bad_request(o.error_summary("compress")));
        }
        chown_like_parent(&dest);
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audit(res, info)
}

#[derive(Deserialize)]
pub struct ExtractBody {
    path: String,
    /// folder to extract into (created if missing)
    dest: String,
}

pub async fn extract(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<ExtractBody>) -> Response {
    let info = AuditInfo::action("files.extract", b.path.clone());
    let res = async {
        let r = roots(&state, &me);
        let src = resolve(&r, &b.path, false)?;
        let dest = resolve(&r, &b.dest, true)?;
        let owner = dest.parent().and_then(owner_of);
        if !dest.exists() {
            std::fs::create_dir_all(&dest).map_err(|e| ApiError::bad_request(e.to_string()))?;
        }
        let s = src.to_string_lossy().to_string();
        let d = dest.to_string_lossy().to_string();
        // Extract as the folder's owner so nothing ends up owned by root and
        // archive paths can't escape through files they are not allowed to write.
        let user = owner.and_then(|(u, _)| crate::util::user_name(u)).filter(|u| u != "root");
        let script = if s.ends_with(".zip") {
            ensure_zip().await;
            "unzip -q -o \"$1\" -d \"$2\""
        } else if s.ends_with(".tar.gz") || s.ends_with(".tgz") || s.ends_with(".tar") || s.ends_with(".tar.bz2") || s.ends_with(".tar.xz") {
            "tar --no-same-owner -xf \"$1\" -C \"$2\""
        } else {
            return Err(ApiError::bad_request("supported: .zip, .tar, .tar.gz, .tgz, .tar.bz2, .tar.xz"));
        };
        if let Some(u) = &user {
            if let Some((uid, gid)) = owner {
                let _ = std::os::unix::fs::chown(&dest, Some(uid), Some(gid));
            }
            let o = Cmd::new("runuser").args(["-u", u, "--", "sh", "-c", script, "extract", &s, &d]).timeout(Duration::from_secs(3600)).run().await;
            if !o.ok {
                return Err(ApiError::bad_request(o.error_summary("extract")));
            }
        } else {
            let o = Cmd::new("sh").args(["-c", script, "extract", &s, &d]).timeout(Duration::from_secs(3600)).run().await;
            if !o.ok {
                return Err(ApiError::bad_request(o.error_summary("extract")));
            }
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audit(res, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stays_inside_roots() {
        let tmp = std::env::temp_dir().join(format!("ncfiles-{}", uuid::Uuid::new_v4()));
        let root = tmp.join("site");
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::create_dir_all(tmp.join("secret")).unwrap();
        std::os::unix::fs::symlink(tmp.join("secret"), root.join("escape")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let r = Roots { rw: vec![root.clone()], ro: vec![] };
        let s = |p: &Path| p.to_string_lossy().to_string();
        assert!(resolve(&r, &s(&root.join("public")), true).is_ok());
        assert!(resolve(&r, &s(&root.join("public/new.txt")), true).is_ok(), "new file in an allowed folder");
        assert!(resolve(&r, &s(&root.join("escape")), false).is_err(), "symlink out of the root");
        assert!(resolve(&r, &s(&root.join("escape/x.txt")), true).is_err());
        assert!(resolve(&r, &s(&root.join("../secret")), false).is_err());
        assert!(resolve(&r, "relative/path", false).is_err());
        let ro = Roots { rw: vec![], ro: vec![root.clone()] };
        assert!(resolve(&ro, &s(&root.join("public")), false).is_ok());
        assert!(resolve(&ro, &s(&root.join("public")), true).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
