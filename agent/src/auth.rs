//! Password hashing, sessions, login throttling, and the auth/origin middleware.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use rand::distr::{Alphanumeric, SampleString};
use serde::Deserialize;
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::error::ApiError;
use crate::state::AppState;
use crate::util::fmt_ts;

pub const COOKIE_NAME: &str = "nc_session";
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
pub const THROTTLE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// After this many failures inside the window, further attempts get 429.
pub const MAX_FAILURES: usize = 5;
const FAILED_LOGIN_DELAY: Duration = Duration::from_millis(400);
const MAX_PASSWORD_LEN: usize = 1024;
/// How long `POST /api/auth/sudo` elevates a session.
pub const SUDO_TTL: Duration = Duration::from_secs(15 * 60);

// ---------------------------------------------------------------- passwords

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn is_valid_hash(hash: &str) -> bool {
    PasswordHash::new(hash).is_ok() && hash.starts_with("$argon2")
}

/// Random 20-character alphanumeric password.
pub fn generate_password() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 20)
}

/// 32 random bytes, hex encoded.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- sessions

struct Session {
    expires: Instant,
    /// Sudo mode: dangerous endpoints are allowed until this time.
    sudo_until: Option<DateTime<Utc>>,
    username: String,
}

/// Who is making the request (cookie session or API token). The role and
/// project list are read fresh from the database on every request.
#[derive(Clone, Debug)]
pub struct Identity {
    pub username: String,
    pub role: crate::users::Role,
    pub projects: Vec<String>,
    /// `Some(scope)` when authenticated with an API token.
    pub token_scope: Option<String>,
}

impl Identity {
    /// May this identity change project `id`?
    pub fn can_change_project(&self, id: &str) -> bool {
        match self.role {
            crate::users::Role::Admin => true,
            crate::users::Role::Developer => self.projects.iter().any(|p| p == id),
            crate::users::Role::Viewer => false,
        }
    }
}

/// In-memory sessions (a restart signs everybody out).
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Session>>,
}

impl Sessions {
    pub fn create(&self, username: &str) -> String {
        let token = generate_token();
        let now = Instant::now();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, s| s.expires > now);
        m.insert(
            token.clone(),
            Session {
                expires: now + SESSION_TTL,
                sudo_until: None,
                username: username.to_string(),
            },
        );
        token
    }

    /// The user of a live session.
    pub fn username(&self, token: &str) -> Option<String> {
        let now = Instant::now();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(token) {
            Some(s) if s.expires > now => Some(s.username.clone()),
            Some(_) => {
                m.remove(token);
                None
            }
            None => None,
        }
    }

    /// Sign out every session of a user (password change, user removed).
    pub fn remove_user(&self, username: &str) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| s.username != username);
    }

    pub fn remove(&self, token: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }

    /// Enter sudo mode for `SUDO_TTL` from `now`. None if the session is gone.
    pub fn elevate(&self, token: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let s = m.get_mut(token)?;
        let until = now + chrono::Duration::from_std(SUDO_TTL).unwrap_or_default();
        s.sudo_until = Some(until);
        Some(until)
    }

    /// When sudo mode ends, if it is active at `now`.
    pub fn sudo_until(&self, token: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.get(token)
            .and_then(|s| s.sudo_until)
            .filter(|until| *until > now)
    }
}

// ---------------------------------------------------------------- throttle

/// Per-IP failed-login tracking. An attempt is counted as a failure *before*
/// the password is checked (and forgiven on success) so that parallel
/// requests cannot race past the limit.
#[derive(Default)]
pub struct Throttle {
    map: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl Throttle {
    /// Returns false (locked out) if the IP already has MAX_FAILURES failures
    /// in the window; otherwise records a provisional failure and returns true.
    pub fn begin_attempt(&self, ip: IpAddr, now: Instant) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, v| {
            v.retain(|t| now.saturating_duration_since(*t) < THROTTLE_WINDOW);
            !v.is_empty()
        });
        let v = m.entry(ip).or_default();
        if v.len() >= MAX_FAILURES {
            return false;
        }
        v.push(now);
        true
    }

    /// Undo the provisional failure recorded by `begin_attempt` (used when the
    /// request was malformed and no password was actually tried).
    pub fn cancel_attempt(&self, ip: IpAddr) {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = m.get_mut(&ip) {
            v.pop();
            if v.is_empty() {
                m.remove(&ip);
            }
        }
    }

    /// Successful login: forget the provisional failure (and earlier ones).
    pub fn succeeded(&self, ip: IpAddr) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
    }
}

// ---------------------------------------------------------------- cookies

pub fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { " Secure;" } else { "" };
    format!(
        "{COOKIE_NAME}={token}; HttpOnly;{secure} SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

pub fn clear_cookie(secure: bool) -> String {
    let secure = if secure { " Secure;" } else { "" };
    format!("{COOKIE_NAME}=; HttpOnly;{secure} SameSite=Strict; Path=/; Max-Age=0")
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all(header::COOKIE) {
        let Ok(s) = v.to_str() else { continue };
        for part in s.split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                if k.trim() == COOKIE_NAME {
                    let v = v.trim();
                    if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------- origin

/// Authority (`host[:port]`) from an Origin header value, with the default
/// port for the scheme removed. None for `null`/garbage.
fn origin_authority(origin: &str) -> Option<(String, &'static str)> {
    let (scheme, rest) = origin.split_once("://")?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "https" => "https",
        "http" => "http",
        _ => return None,
    };
    if rest.is_empty() || rest.contains('/') || rest.contains('@') {
        return None;
    }
    Some((
        strip_default_port(&rest.to_ascii_lowercase(), scheme),
        scheme,
    ))
}

fn strip_default_port(authority: &str, scheme: &str) -> String {
    let default = if scheme == "https" { ":443" } else { ":80" };
    authority
        .strip_suffix(default)
        .unwrap_or(authority)
        .to_string()
}

/// True if the Origin's host:port equals the request's Host.
pub fn origin_matches(origin: Option<&str>, host: Option<&str>) -> bool {
    let (Some(origin), Some(host)) = (origin, host) else {
        return false;
    };
    let Some((o, scheme)) = origin_authority(origin.trim()) else {
        return false;
    };
    let h = strip_default_port(&host.trim().to_ascii_lowercase(), scheme);
    !h.is_empty() && o == h
}

fn is_mutating(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// CSRF defence: mutating requests must carry an Origin matching Host.
/// (HTTP/2 has no Host header; the authority comes from the URI then.)
pub async fn origin_check(req: Request, next: Next) -> Response {
    let bearer = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("Bearer "));
    if is_mutating(req.method()) && !bearer {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| req.uri().authority().map(|a| a.as_str().to_string()));
        if !origin_matches(origin, host.as_deref()) {
            return ApiError::new(StatusCode::FORBIDDEN, "origin check failed").into_response();
        }
    }
    next.run(req).await
}

/// Session token of the authenticated request (inserted by `require_session`).
#[derive(Clone)]
pub struct SessionToken(pub String);

pub async fn require_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string());
    let identity = if let Some(secret) = bearer {
        match crate::users::authenticate_token(&state.db, &secret) {
            Some((t, u)) => {
                req.extensions_mut().insert(SessionToken(format!("token:{}", t.id)));
                Identity { username: u.username, role: u.role, projects: u.projects, token_scope: Some(t.scope) }
            }
            None => return ApiError::new(StatusCode::UNAUTHORIZED, "invalid or expired API token").into_response(),
        }
    } else {
        let Some(t) = token_from_headers(req.headers()) else {
            return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        };
        let Some(username) = state.sessions.username(&t) else {
            return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        };
        let Some(u) = crate::users::get(&state.db, &username).ok().flatten() else {
            state.sessions.remove(&t);
            return ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        };
        req.extensions_mut().insert(SessionToken(t));
        Identity { username: u.username, role: u.role, projects: u.projects, token_scope: None }
    };
    if let Err(msg) = allowed(&identity, req.method(), req.uri().path()) {
        return ApiError::new(StatusCode::FORBIDDEN, msg).into_response();
    }
    req.extensions_mut().insert(identity);
    next.run(req).await
}

/// Role / token-scope rules that only need the method and path. Handlers that
/// act on a project from the request body (plans, files, terminal, cron)
/// check `Identity::can_change_project` themselves.
pub fn allowed(id: &Identity, method: &Method, path: &str) -> Result<(), &'static str> {
    use crate::users::Role;
    let read = *method == Method::GET || *method == Method::HEAD;
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let own_account = seg.first() == Some(&"auth") || seg.first() == Some(&"account");
    match id.token_scope.as_deref() {
        Some("read") if !read => return Err("this API token is read-only"),
        Some("deploy") => {
            let deploy = *method == Method::POST && seg.len() == 3 && seg[0] == "projects" && seg[2] == "deploy";
            let readable = read && matches!(seg.first(), Some(&"projects") | Some(&"jobs") | Some(&"deploys") | Some(&"health"));
            if !(deploy || readable) {
                return Err("this API token can only start deploys and read projects");
            }
            if deploy && !id.can_change_project(seg[1]) {
                return Err("this API token's user cannot deploy that project");
            }
            return Ok(());
        }
        _ => {}
    }
    if read || own_account {
        // Admin-only reads: user management and other users' tokens.
        if seg.first() == Some(&"users") && id.role != Role::Admin {
            return Err("only admins can manage users");
        }
        return Ok(());
    }
    match id.role {
        Role::Admin => Ok(()),
        Role::Viewer => Err("your account is read-only"),
        Role::Developer => {
            let project_path = seg.first() == Some(&"projects") && seg.len() >= 2 && id.can_change_project(seg[1]);
            // body-scoped endpoints check the project in the handler
            let scoped = matches!(seg.first(), Some(&"plans") | Some(&"files") | Some(&"terminal") | Some(&"cron"));
            if project_path || scoped {
                Ok(())
            } else {
                Err("developers can only change their own projects")
            }
        }
    }
}

/// Gate for dangerous endpoints (apply/rollback a change, restart a service,
/// cancel a job…): `403 {"error":"sudo_required"}` unless the session entered
/// sudo mode in the last 15 minutes. Mount with `route_layer` on the
/// "dangerous" router; `require_session` must run first.
pub async fn require_sudo(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let full_token = req
        .extensions()
        .get::<Identity>()
        .is_some_and(|i| i.token_scope.as_deref() == Some("full") || i.token_scope.as_deref() == Some("deploy"));
    let elevated = full_token
        || req
            .extensions()
            .get::<SessionToken>()
            .and_then(|t| state.sessions.sudo_until(&t.0, Utc::now()))
            .is_some();
    if !elevated {
        return ApiError::sudo_required().into_response();
    }
    next.run(req).await
}

// ---------------------------------------------------------------- handlers

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    username: Option<String>,
    password: String,
    /// 2FA code or recovery code.
    #[serde(default)]
    code: Option<String>,
}

/// The client IP for throttling: the socket peer, or — only when the peer is
/// loopback (nginx in front of the panel on its own domain) — `X-Real-IP`.
pub fn throttle_ip(addr: &SocketAddr, headers: &axum::http::HeaderMap) -> IpAddr {
    let peer = addr.ip().to_canonical();
    if peer.is_loopback() {
        if let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
        {
            return ip.to_canonical();
        }
    }
    peer
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let ip = throttle_ip(&addr, &headers);
    let who = serde_json::from_slice::<LoginBody>(&body)
        .ok()
        .and_then(|b| b.username)
        .unwrap_or_else(|| crate::users::ADMIN.into());
    audited(
        login_inner(state, ip, body).await,
        AuditInfo::action("auth.login", crate::util::cmd::truncate(&who, 40)),
    )
}

/// Check a password with the login throttle (shared by login and sudo).
/// `user`: the session's user for sudo; None at login (username from the
/// body, default admin). Returns the user. Err is the response to send.
async fn check_password(state: &AppState, ip: IpAddr, body: &[u8], user: Option<&str>, need_2fa: bool) -> Result<crate::users::User, ApiError> {
    if !state.throttle.begin_attempt(ip, Instant::now()) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts, try again later",
        ));
    }
    let b = match serde_json::from_slice::<LoginBody>(body) {
        Ok(b) => b,
        Err(_) => {
            state.throttle.cancel_attempt(ip);
            return Err(ApiError::bad_request(
                "invalid request body: expected {\"password\": \"...\"}",
            ));
        }
    };
    let username = user
        .map(String::from)
        .or(b.username.clone())
        .map(|u| u.trim().to_ascii_lowercase())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| crate::users::ADMIN.into());
    let u = crate::users::get(&state.db, &username).ok().flatten();
    let hash = match &u {
        Some(u) if u.username == crate::users::ADMIN => Some(state.config.admin_password_hash.clone()),
        Some(u) => u.password_hash.clone(),
        None => None,
    };
    let password = b.password;
    let ok = password.len() <= MAX_PASSWORD_LEN
        && match hash {
            Some(h) => tokio::task::spawn_blocking(move || verify_password(&password, &h))
                .await
                .unwrap_or(false),
            None => false,
        };
    if !ok {
        tokio::time::sleep(FAILED_LOGIN_DELAY).await;
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong username or password"));
    }
    let u = u.expect("checked above");
    if need_2fa && u.totp_enabled {
        match b.code.as_deref().filter(|c| !c.trim().is_empty()) {
            None => {
                // Not a failed attempt: the password was right.
                state.throttle.cancel_attempt(ip);
                return Err(ApiError::new(StatusCode::UNAUTHORIZED, "totp_required"));
            }
            Some(code) => {
                if !crate::users::check_second_factor(&state.db, &u, code) {
                    tokio::time::sleep(FAILED_LOGIN_DELAY).await;
                    return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong two-factor code"));
                }
            }
        }
    }
    state.throttle.succeeded(ip);
    Ok(u)
}

/// `POST /api/auth/sudo {"password"}`: re-enter the password to unlock
/// dangerous actions for 15 minutes. Wrong attempts count towards the login
/// throttle.
pub async fn sudo(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::Extension(SessionToken(token)): axum::Extension<SessionToken>,
    axum::Extension(me): axum::Extension<Identity>,
    body: Bytes,
) -> Response {
    let ip = throttle_ip(&addr, &headers);
    let resp = match check_password(&state, ip, &body, Some(&me.username), false).await {
        Err(e) => {
            tracing::warn!(%ip, "failed sudo attempt");
            e.into_response()
        }
        Ok(_) => match state.sessions.elevate(&token, Utc::now()) {
            Some(until) => {
                tracing::info!(%ip, "sudo mode on");
                Json(json!({"ok": true, "sudo_until": fmt_ts(until)})).into_response()
            }
            None => ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
        },
    };
    audited(resp, AuditInfo::action("auth.sudo", me.username.clone()))
}

async fn login_inner(state: AppState, ip: IpAddr, body: Bytes) -> Response {
    let u = match check_password(&state, ip, &body, None, true).await {
        Ok(u) => u,
        Err(e) => {
            if e.status == StatusCode::UNAUTHORIZED && e.message != "totp_required" {
                tracing::warn!(%ip, "failed login");
            }
            return e.into_response();
        }
    };
    crate::users::touch_login(&state.db, &u.username);
    let token = state.sessions.create(&u.username);
    tracing::info!(%ip, "login");
    let mut resp = Json(json!({"ok": true})).into_response();
    if let Ok(v) = HeaderValue::from_str(&session_cookie(&token, state.config.tls)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

pub async fn logout(
    State(state): State<AppState>,
    axum::Extension(SessionToken(token)): axum::Extension<SessionToken>,
) -> Response {
    state.sessions.remove(&token);
    let mut resp = Json(json!({"ok": true})).into_response();
    if let Ok(v) = HeaderValue::from_str(&clear_cookie(state.config.tls)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    audited(resp, AuditInfo::action("auth.logout", "admin"))
}

/// `{"ok":true,"sudo_until":"<rfc3339>"|null}`
pub async fn me(
    State(state): State<AppState>,
    axum::Extension(SessionToken(token)): axum::Extension<SessionToken>,
    axum::Extension(me): axum::Extension<Identity>,
) -> Json<serde_json::Value> {
    let until = state.sessions.sudo_until(&token, Utc::now()).map(fmt_ts);
    let u = crate::users::get(&state.db, &me.username).ok().flatten();
    Json(json!({
        "ok": true, "sudo_until": until, "username": me.username, "role": me.role,
        "projects": me.projects, "totp_enabled": u.map(|u| u.totp_enabled).unwrap_or(false),
        "via_token": me.token_scope.is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("s3cret").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(is_valid_hash(&h));
        assert!(verify_password("s3cret", &h));
        assert!(!verify_password("s3cret ", &h));
        assert!(!verify_password("x", "not-a-hash"));
    }

    #[test]
    fn tokens_and_passwords() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(t, generate_token());
        let p = generate_password();
        assert_eq!(p.len(), 20);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn cookie_format() {
        assert_eq!(
            session_cookie("abc", true),
            "nc_session=abc; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=43200"
        );
        assert_eq!(
            session_cookie("abc", false),
            "nc_session=abc; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200"
        );
        assert!(clear_cookie(true).contains("Max-Age=0"));
    }

    #[test]
    fn cookie_parsing() {
        let tok = "a".repeat(64);
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("foo=bar; nc_session={tok}; x=y")).unwrap(),
        );
        assert_eq!(token_from_headers(&h), Some(tok));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("nc_session=short"));
        assert_eq!(token_from_headers(&h), None);
    }

    #[test]
    fn sessions() {
        let s = Sessions::default();
        let t = s.create("admin");
        assert_eq!(s.username(&t).as_deref(), Some("admin"));
        assert!(s.username("nope").is_none());
        let t2 = s.create("dev1");
        s.remove_user("dev1");
        assert!(s.username(&t2).is_none(), "remove_user signs that user out");
        s.remove(&t);
        assert!(s.username(&t).is_none());
    }

    #[test]
    fn role_and_token_rules() {
        use crate::users::Role;
        let id = |role, projects: &[&str], scope: Option<&str>| Identity { username: "u".into(), role, projects: projects.iter().map(|s| s.to_string()).collect(), token_scope: scope.map(String::from) };
        let dev = id(Role::Developer, &["shop.test"], None);
        assert!(allowed(&dev, &Method::GET, "/sites").is_ok());
        assert!(allowed(&dev, &Method::POST, "/projects/shop.test/deploy").is_ok());
        assert!(allowed(&dev, &Method::POST, "/projects/other.test/deploy").is_err());
        assert!(allowed(&dev, &Method::POST, "/services/nginx/restart").is_err());
        assert!(allowed(&dev, &Method::POST, "/plans/apply").is_ok(), "checked in the handler");
        assert!(allowed(&dev, &Method::GET, "/users").is_err());
        assert!(allowed(&dev, &Method::POST, "/account/tokens").is_ok());
        let viewer = id(Role::Viewer, &[], None);
        assert!(allowed(&viewer, &Method::POST, "/projects/shop.test/deploy").is_err());
        assert!(allowed(&viewer, &Method::POST, "/auth/sudo").is_ok());
        let admin = id(Role::Admin, &[], None);
        assert!(allowed(&admin, &Method::POST, "/services/nginx/restart").is_ok());
        let ro = id(Role::Admin, &[], Some("read"));
        assert!(allowed(&ro, &Method::POST, "/projects/x/deploy").is_err());
        assert!(allowed(&ro, &Method::GET, "/sites").is_ok());
        let dep = id(Role::Admin, &[], Some("deploy"));
        assert!(allowed(&dep, &Method::POST, "/projects/x/deploy").is_ok());
        assert!(allowed(&dep, &Method::GET, "/projects/x").is_ok());
        assert!(allowed(&dep, &Method::GET, "/sites").is_err());
        assert!(allowed(&dep, &Method::PUT, "/projects/x").is_err());
    }

    #[test]
    fn sudo_mode_expires_after_15_minutes() {
        let s = Sessions::default();
        let t = s.create("admin");
        let now = Utc::now();
        assert_eq!(s.sudo_until(&t, now), None);
        let until = s.elevate(&t, now).unwrap();
        assert_eq!(until - now, chrono::Duration::minutes(15));
        assert_eq!(
            s.sudo_until(&t, now + chrono::Duration::minutes(14)),
            Some(until)
        );
        assert_eq!(s.sudo_until(&t, now + chrono::Duration::minutes(15)), None);
        assert_eq!(s.sudo_until(&t, now + chrono::Duration::minutes(16)), None);
        // Unknown / signed-out sessions can't be elevated.
        assert!(s.elevate("nope", now).is_none());
        s.remove(&t);
        assert!(s.elevate(&t, now).is_none());
        assert_eq!(s.sudo_until(&t, now), None);
    }

    #[test]
    fn throttle_locks_after_five_failures() {
        let th = Throttle::default();
        let ip: IpAddr = "203.0.113.5".parse().unwrap();
        let other: IpAddr = "203.0.113.6".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            assert!(th.begin_attempt(ip, t0));
        }
        assert!(!th.begin_attempt(ip, t0 + Duration::from_secs(60)));
        assert!(th.begin_attempt(other, t0));
        // Window expires.
        assert!(th.begin_attempt(ip, t0 + THROTTLE_WINDOW + Duration::from_secs(1)));
        // Success clears.
        th.succeeded(ip);
        for _ in 0..MAX_FAILURES {
            assert!(th.begin_attempt(ip, t0 + THROTTLE_WINDOW + Duration::from_secs(2)));
        }
    }

    #[test]
    fn origin() {
        assert!(origin_matches(
            Some("https://1.2.3.4:8443"),
            Some("1.2.3.4:8443")
        ));
        assert!(origin_matches(
            Some("http://127.0.0.1:8443"),
            Some("127.0.0.1:8443")
        ));
        assert!(origin_matches(
            Some("https://Panel.Example.com"),
            Some("panel.example.com")
        ));
        assert!(origin_matches(
            Some("https://panel.example.com:443"),
            Some("panel.example.com")
        ));
        assert!(origin_matches(
            Some("https://panel.example.com"),
            Some("panel.example.com:443")
        ));
        assert!(!origin_matches(
            Some("https://evil.com"),
            Some("panel.example.com")
        ));
        assert!(!origin_matches(
            Some("https://1.2.3.4:9999"),
            Some("1.2.3.4:8443")
        ));
        assert!(!origin_matches(
            Some("http://panel.example.com:443"),
            Some("panel.example.com")
        ));
        assert!(!origin_matches(Some("null"), Some("panel.example.com")));
        assert!(!origin_matches(None, Some("panel.example.com")));
        assert!(!origin_matches(Some("https://a.com"), None));
        assert!(!origin_matches(Some("https://a.com@b.com"), Some("b.com")));
    }
}
