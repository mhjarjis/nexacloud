//! SFTP-only accounts, one site folder each.
//!
//! An account shares the uid/gid of the site folder's owner (so uploads
//! belong to the site), is in group `nc-sftp`, has no shell, and sshd jails
//! it in `/srv/sftp/<user>` where the site folder is bind-mounted at `/site`.
//! Keys live in `/etc/ssh/nexacloud-keys/<user>` (root-owned).

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::cmd::Cmd;

const GROUP: &str = "nc-sftp";
const JAIL: &str = "/srv/sftp";
const KEYS: &str = "/etc/ssh/nexacloud-keys";
const SSHD_CONF: &str = "/etc/ssh/sshd_config.d/60-nexacloud-sftp.conf";
const FSTAB: &str = "/etc/fstab";

const SSHD_BLOCK: &str = "# Managed by NexaCloud — SFTP-only accounts, each jailed in one site folder.\nMatch Group nc-sftp\n    ChrootDirectory /srv/sftp/%u\n    ForceCommand internal-sftp -d /site\n    AuthorizedKeysFile /etc/ssh/nexacloud-keys/%u\n    PasswordAuthentication yes\n    AllowTcpForwarding no\n    AllowAgentForwarding no\n    X11Forwarding no\n    PermitTunnel no\n";

#[derive(Serialize)]
pub struct SftpUser {
    username: String,
    site_id: String,
    dir: String,
    created_at: String,
    has_key: bool,
    mounted: bool,
}

fn err(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e.to_string())
}

fn mounted(target: &str) -> bool {
    std::fs::read_to_string("/proc/self/mounts").unwrap_or_default().lines().any(|l| l.split_whitespace().nth(1) == Some(target))
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<SftpUser>>> {
    let c = state.db.conn();
    let mut st = c.prepare("SELECT username, site_id, dir, created_at FROM sftp_users ORDER BY username").map_err(err)?;
    let v = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
        .map_err(err)?
        .flatten()
        .map(|(u, s, d, c)| SftpUser {
            has_key: std::fs::metadata(format!("{KEYS}/{u}")).is_ok_and(|m| m.len() > 0),
            mounted: mounted(&format!("{JAIL}/{u}/site")),
            username: u,
            site_id: s,
            dir: d,
            created_at: c,
        })
        .collect();
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct NewSftp {
    username: String,
    site_id: String,
    password: Option<String>,
    ssh_key: Option<String>,
}

fn valid_name(u: &str) -> bool {
    (3..=31).contains(&u.len()) && u.as_bytes()[0].is_ascii_lowercase() && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn valid_key(k: &str) -> bool {
    let k = k.trim();
    k.len() < 16 * 1024
        && !k.is_empty()
        && k.lines().all(|l| {
            let l = l.trim();
            l.is_empty() || ["ssh-ed25519 ", "ssh-rsa ", "ecdsa-sha2-", "sk-ssh-ed25519@", "sk-ecdsa-"].iter().any(|p| l.starts_with(p))
        })
}

async fn run(c: Cmd, what: &str) -> ApiResult<()> {
    let o = c.timeout(Duration::from_secs(120)).run().await;
    if o.ok {
        Ok(())
    } else {
        Err(ApiError::bad_request(o.error_summary(what)))
    }
}

/// Folder an account for this site gets: the project folder, else the
/// managed site's project dir / web root.
fn site_dir(state: &AppState, site: &str) -> ApiResult<String> {
    if let Some(p) = crate::deploys::get_project(&state.db, site).ok().flatten() {
        return Ok(p.dir);
    }
    let m = crate::plans::sites::load_model(&state.changes_env, site).map_err(ApiError::bad_request)?;
    m.project_dir.or(m.root).ok_or_else(|| ApiError::bad_request("this site has no folder"))
}

/// Make sure the group, jail root and sshd rules exist (sshd checked before reload).
async fn ensure_base() -> ApiResult<()> {
    run(Cmd::new("sh").args(["-c", "getent group \"$1\" >/dev/null || groupadd --system \"$1\"", "g", GROUP]), "groupadd").await?;
    for d in [JAIL, KEYS] {
        std::fs::create_dir_all(d).map_err(err)?;
        let _ = std::os::unix::fs::chown(d, Some(0), Some(0));
        let _ = std::fs::set_permissions(d, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    }
    let current = std::fs::read_to_string(SSHD_CONF).unwrap_or_default();
    if current != SSHD_BLOCK {
        if !Path::new("/usr/sbin/sshd").exists() {
            return Err(ApiError::bad_request("the SSH server is not installed (apt install openssh-server)"));
        }
        std::fs::create_dir_all("/etc/ssh/sshd_config.d").map_err(err)?;
        crate::util::atomic_write(Path::new(SSHD_CONF), SSHD_BLOCK.as_bytes(), 0o644, Some((0, 0))).map_err(err)?;
        let t = Cmd::new("sshd").arg("-t").timeout(Duration::from_secs(20)).run().await;
        if !t.ok {
            if current.is_empty() {
                let _ = std::fs::remove_file(SSHD_CONF);
            } else {
                let _ = crate::util::atomic_write(Path::new(SSHD_CONF), current.as_bytes(), 0o644, Some((0, 0)));
            }
            return Err(ApiError::bad_request(format!("sshd refused the SFTP rules, nothing changed: {}", t.error_summary("sshd -t"))));
        }
        let _ = Cmd::new("sh").args(["-c", "systemctl reload ssh 2>/dev/null || systemctl reload sshd 2>/dev/null || true"]).run().await;
    }
    Ok(())
}

fn fstab_line(dir: &str, user: &str) -> String {
    format!("{dir} {JAIL}/{user}/site none bind,nofail 0 0 # nexacloud-sftp:{user}")
}

async fn set_password(user: &str, password: &str) -> ApiResult<()> {
    if password.chars().count() < 12 || password.len() > 200 || password.contains(['\n', ':']) {
        return Err(ApiError::bad_request("password: at least 12 characters (no : or line breaks)"));
    }
    run(Cmd::new("chpasswd").stdin(format!("{user}:{password}\n")), "chpasswd").await
}

fn write_key(user: &str, key: &str) -> ApiResult<()> {
    let body = key.trim().to_string() + "\n";
    crate::util::atomic_write(Path::new(&format!("{KEYS}/{user}")), body.as_bytes(), 0o644, Some((0, 0))).map_err(err)
}

pub async fn create(State(state): State<AppState>, Json(b): Json<NewSftp>) -> Response {
    let info = AuditInfo::action("sftp.create", format!("{} → {}", b.username, b.site_id));
    let res = async {
        let user = b.username.trim().to_string();
        if !valid_name(&user) {
            return Err(ApiError::bad_request("username: 3–31 lowercase letters, digits, _ or -, starting with a letter"));
        }
        if crate::util::user_home(&user).is_some() {
            return Err(ApiError::bad_request("that username already exists on the server"));
        }
        let pw = b.password.as_deref().filter(|p| !p.is_empty());
        let key = b.ssh_key.as_deref().filter(|k| !k.trim().is_empty());
        if pw.is_none() && key.is_none() {
            return Err(ApiError::bad_request("give a password, an SSH key, or both"));
        }
        if let Some(k) = key {
            if !valid_key(k) {
                return Err(ApiError::bad_request("SSH key: paste a public key line (ssh-ed25519 AAAA… or ssh-rsa AAAA…)"));
            }
        }
        let dir = site_dir(&state, &b.site_id)?;
        let md = std::fs::metadata(&dir).map_err(|_| ApiError::bad_request(format!("{dir} does not exist")))?;
        if md.uid() == 0 {
            return Err(ApiError::bad_request(format!("{dir} is owned by root — give the site its own user (Settings → Isolate) or fix permissions first")));
        }
        ensure_base().await?;
        let jail = format!("{JAIL}/{user}");
        let mount = format!("{jail}/site");
        run(
            Cmd::new("useradd").args(["-o", "-u", &md.uid().to_string(), "-g", &md.gid().to_string(), "-G", GROUP, "-M", "-d", "/site", "-s", "/usr/sbin/nologin", "-c", &format!("NexaCloud SFTP for {}", b.site_id), &user]),
            "useradd",
        )
        .await?;
        let undo = |user: String| async move {
            let _ = Cmd::new("userdel").args(["-f", &user]).run().await;
        };
        let setup = async {
            std::fs::create_dir_all(&mount).map_err(err)?;
            let _ = std::os::unix::fs::chown(&jail, Some(0), Some(0));
            let _ = std::fs::set_permissions(&jail, std::os::unix::fs::PermissionsExt::from_mode(0o755));
            if !mounted(&mount) {
                run(Cmd::new("mount").args(["--bind", &dir, &mount]), "mount --bind").await?;
            }
            let fstab = std::fs::read_to_string(FSTAB).unwrap_or_default();
            let line = fstab_line(&dir, &user);
            if !fstab.lines().any(|l| l == line) {
                let mut f = fstab.clone();
                if !f.is_empty() && !f.ends_with('\n') {
                    f.push('\n');
                }
                f.push_str(&line);
                f.push('\n');
                crate::util::atomic_write(Path::new(FSTAB), f.as_bytes(), 0o644, Some((0, 0))).map_err(err)?;
            }
            if let Some(p) = pw {
                set_password(&user, p).await?;
            } else {
                let _ = Cmd::new("passwd").args(["-l", &user]).run().await;
            }
            write_key(&user, key.unwrap_or(""))?;
            state
                .db
                .conn()
                .execute("INSERT INTO sftp_users (username, site_id, dir, created_at) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![user, b.site_id, dir, crate::util::fmt_ts(chrono::Utc::now())])
                .map_err(err)?;
            Ok::<(), ApiError>(())
        }
        .await;
        if let Err(e) = setup {
            remove_account(&user, &dir).await;
            undo(user.clone()).await;
            return Err(e);
        }
        Ok(Json(json!({"ok": true, "username": user, "folder": "/site"})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

async fn remove_account(user: &str, dir: &str) {
    let mount = format!("{JAIL}/{user}/site");
    if mounted(&mount) {
        let _ = Cmd::new("umount").arg(&mount).run().await;
    }
    if let Ok(f) = std::fs::read_to_string(FSTAB) {
        let line = fstab_line(dir, user);
        let marker = format!("# nexacloud-sftp:{user}");
        let kept: Vec<&str> = f.lines().filter(|l| *l != line && !l.ends_with(&marker)).collect();
        let mut out = kept.join("\n");
        out.push('\n');
        let _ = crate::util::atomic_write(Path::new(FSTAB), out.as_bytes(), 0o644, Some((0, 0)));
    }
    // only empty folders: never delete anything inside the site
    let _ = std::fs::remove_dir(&mount);
    let _ = std::fs::remove_dir(format!("{JAIL}/{user}"));
    let _ = std::fs::remove_file(format!("{KEYS}/{user}"));
}

fn find(state: &AppState, user: &str) -> ApiResult<String> {
    state
        .db
        .conn()
        .query_row("SELECT dir FROM sftp_users WHERE username = ?1", [user], |r| r.get::<_, String>(0))
        .map_err(|_| ApiError::not_found("no such SFTP account"))
}

pub async fn remove(State(state): State<AppState>, UrlPath(user): UrlPath<String>) -> Response {
    let info = AuditInfo::action("sftp.delete", user.clone());
    let res = async {
        let dir = find(&state, &user)?;
        // The account shares the site's uid, so never kill by uid (that would
        // hit the site's PHP workers): only this account's sshd session.
        let _ = Cmd::new("pkill").args(["-KILL", "-f", &format!("^sshd: {user}[@ ]")]).run().await;
        remove_account(&user, &dir).await;
        if mounted(&format!("{JAIL}/{user}/site")) {
            return Err(ApiError::bad_request("the folder is still in use — try again in a moment"));
        }
        // -f: the shared uid is "in use" by the site's own processes.
        let o = Cmd::new("userdel").args(["-f", &user]).run().await;
        if !o.ok && crate::util::user_home(&user).is_some() {
            return Err(ApiError::bad_request(o.error_summary("userdel")));
        }
        state.db.conn().execute("DELETE FROM sftp_users WHERE username = ?1", [&user]).map_err(err)?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

#[derive(Deserialize)]
pub struct EditSftp {
    password: Option<String>,
    ssh_key: Option<String>,
}

pub async fn update(State(state): State<AppState>, UrlPath(user): UrlPath<String>, Json(b): Json<EditSftp>) -> Response {
    let info = AuditInfo::action("sftp.update", user.clone());
    let res = async {
        find(&state, &user)?;
        if let Some(p) = b.password.as_deref().filter(|p| !p.is_empty()) {
            set_password(&user, p).await?;
            let _ = Cmd::new("passwd").args(["-u", &user]).run().await;
        }
        if let Some(k) = &b.ssh_key {
            if !k.trim().is_empty() && !valid_key(k) {
                return Err(ApiError::bad_request("SSH key: paste a public key line (ssh-ed25519 AAAA…)"));
            }
            write_key(&user, k)?;
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

/// Startup: re-create bind mounts that are missing (fstab has `nofail`).
pub async fn startup(state: &AppState) {
    let rows: Vec<(String, String)> = {
        let c = state.db.conn();
        let Ok(mut st) = c.prepare("SELECT username, dir FROM sftp_users") else { return };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|it| it.flatten().collect()).unwrap_or_default()
    };
    for (u, dir) in rows {
        let m = format!("{JAIL}/{u}/site");
        if Path::new(&dir).is_dir() && Path::new(&m).is_dir() && !mounted(&m) {
            let _ = Cmd::new("mount").args(["--bind", &dir, &m]).run().await;
        }
    }
}
