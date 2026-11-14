//! Web Terminal: a real shell (PTY) over a WebSocket.
//!
//! `GET /api/terminal?project=<id>&cols=&rows=` (WebSocket upgrade).
//! - with `project`: a shell as that project's PHP user, in its app folder
//!   (admins and that project's developers);
//! - without: a root shell (admins only).
//! Always needs sudo mode and a same-origin browser (no API tokens).
//!
//! Protocol: binary frames = raw terminal bytes both ways; text frames from
//! the browser = JSON control (`{"resize":[cols,rows]}`).

use std::io::{Read, Write};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use chrono::Utc;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;

use crate::auth::{Identity, SessionToken};
use crate::error::ApiError;
use crate::state::AppState;
use crate::users::Role;

const IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Deserialize)]
pub struct TermQ {
    project: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

pub async fn open(
    State(state): State<AppState>,
    Extension(me): Extension<Identity>,
    Extension(tok): Extension<SessionToken>,
    headers: HeaderMap,
    Query(q): Query<TermQ>,
    ws: WebSocketUpgrade,
) -> Response {
    if me.token_scope.is_some() {
        return ApiError::new(axum::http::StatusCode::FORBIDDEN, "the terminal needs a panel sign-in, not an API token").into_response();
    }
    // WebSockets skip CORS: check the Origin ourselves (cross-site hijacking).
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !crate::auth::origin_matches(origin, host) {
        return ApiError::new(axum::http::StatusCode::FORBIDDEN, "origin check failed").into_response();
    }
    if state.sessions.sudo_until(&tok.0, Utc::now()).is_none() {
        return ApiError::sudo_required().into_response();
    }
    let (user, cwd, label) = match q.project.as_deref().filter(|p| !p.is_empty()) {
        Some(pid) => {
            if !me.can_change_project(pid) || me.role == Role::Viewer {
                return ApiError::new(axum::http::StatusCode::FORBIDDEN, "you can't open a terminal for that project").into_response();
            }
            let Some(p) = crate::deploys::get_project(&state.db, pid).ok().flatten() else {
                return ApiError::not_found("project not found").into_response();
            };
            let user = crate::deploys::permissions::web_user(&p);
            let dir = p.app_dir();
            (user, dir, format!("project {pid}"))
        }
        None => {
            if me.role != Role::Admin {
                return ApiError::new(axum::http::StatusCode::FORBIDDEN, "only admins can open a server terminal").into_response();
            }
            ("root".to_string(), "/root".to_string(), "server".to_string())
        }
    };
    crate::audit::record(
        &state.db,
        &crate::audit::AuditEntry {
            action: "terminal.open".into(),
            target: Some(format!("{label} as {user} (by {})", me.username)),
            result: "ok".into(),
            ..Default::default()
        },
    );
    let size = PtySize { cols: q.cols.unwrap_or(120).clamp(20, 500), rows: q.rows.unwrap_or(32).clamp(5, 200), pixel_width: 0, pixel_height: 0 };
    ws.on_upgrade(move |socket| session(socket, user, cwd, size))
}

async fn session(mut socket: WebSocket, user: String, cwd: String, size: PtySize) {
    let pty = native_pty_system();
    let pair = match pty.openpty(size) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket.send(Message::Text(format!("\r\ncannot open a terminal: {e}\r\n").into())).await;
            return;
        }
    };
    let mut cmd = if user == "root" {
        let mut c = CommandBuilder::new("/bin/bash");
        c.arg("-l");
        c
    } else {
        let home = crate::util::user_home(&user).unwrap_or_else(|| cwd.clone());
        let mut c = CommandBuilder::new("runuser");
        c.args(["-u", &user, "--", "env", &format!("HOME={home}"), &format!("USER={user}"), "/bin/bash", "-i"]);
        c
    };
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "C.UTF-8");
    cmd.cwd(if std::path::Path::new(&cwd).is_dir() { cwd.as_str() } else { "/" });
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket.send(Message::Text(format!("\r\ncannot start the shell: {e}\r\n").into())).await;
            return;
        }
    };
    drop(pair.slave);
    let master = pair.master;
    let (Ok(mut reader), Ok(mut writer)) = (master.try_clone_reader(), master.take_writer()) else {
        let _ = child.kill();
        return;
    };

    // PTY → browser
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    // browser → PTY
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(b) = in_rx.recv() {
            if writer.write_all(&b).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(b) => {
                    if socket.send(Message::Binary(b.into())).await.is_err() { break; }
                }
                None => {
                    let _ = socket.send(Message::Text("\r\n[session ended]\r\n".into())).await;
                    break;
                }
            },
            msg = tokio::time::timeout(IDLE, socket.recv()) => match msg {
                Err(_) => {
                    let _ = socket.send(Message::Text("\r\n[closed after 30 minutes without input]\r\n".into())).await;
                    break;
                }
                Ok(Some(Ok(Message::Binary(b)))) => { if in_tx.send(b.to_vec()).is_err() { break; } }
                Ok(Some(Ok(Message::Text(t)))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if let Some([c, r]) = v.get("resize").and_then(|x| x.as_array()).map(|a| [a.first().and_then(|x| x.as_u64()), a.get(1).and_then(|x| x.as_u64())]) {
                            if let (Some(c), Some(r)) = (c, r) {
                                let _ = master.resize(PtySize { cols: (c as u16).clamp(20, 500), rows: (r as u16).clamp(5, 200), pixel_width: 0, pixel_height: 0 });
                            }
                        } else if let Some(d) = v.get("data").and_then(|x| x.as_str()) {
                            if in_tx.send(d.as_bytes().to_vec()).is_err() { break; }
                        }
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => break,
                Ok(Some(Ok(_))) => {}
            },
        }
    }
    let _ = child.kill();
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
}
