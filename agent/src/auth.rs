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
use rand::distr::{Alphanumeric, SampleString};
use serde::Deserialize;
use serde_json::json;

use crate::error::ApiError;
use crate::state::AppState;

pub const COOKIE_NAME: &str = "nc_session";
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
pub const THROTTLE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// After this many failures inside the window, further attempts get 429.
pub const MAX_FAILURES: usize = 5;
const FAILED_LOGIN_DELAY: Duration = Duration::from_millis(400);
const MAX_PASSWORD_LEN: usize = 1024;

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

#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Instant>>,
}

impl Sessions {
    pub fn create(&self) -> String {
        let token = generate_token();
        let now = Instant::now();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, exp| *exp > now);
        m.insert(token.clone(), now + SESSION_TTL);
        token
    }

    pub fn is_valid(&self, token: &str) -> bool {
        let now = Instant::now();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(token) {
            Some(exp) if *exp > now => true,
            Some(_) => {
                m.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn remove(&self, token: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
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
    if is_mutating(req.method()) {
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
    match token_from_headers(req.headers()) {
        Some(t) if state.sessions.is_valid(&t) => {
            req.extensions_mut().insert(SessionToken(t));
            next.run(req).await
        }
        _ => ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

// ---------------------------------------------------------------- handlers

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let ip = addr.ip().to_canonical();
    if !state.throttle.begin_attempt(ip, Instant::now()) {
        return ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many attempts, try again later",
        )
        .into_response();
    }
    let password = match serde_json::from_slice::<LoginBody>(&body) {
        Ok(b) => b.password,
        Err(_) => {
            // Malformed body: don't count it as a password guess.
            state.throttle.cancel_attempt(ip);
            return ApiError::bad_request("invalid request body: expected {\"password\": \"...\"}")
                .into_response();
        }
    };
    let hash = state.config.admin_password_hash.clone();
    let ok = password.len() <= MAX_PASSWORD_LEN
        && tokio::task::spawn_blocking(move || verify_password(&password, &hash))
            .await
            .unwrap_or(false);
    if !ok {
        tracing::warn!(%ip, "failed login");
        tokio::time::sleep(FAILED_LOGIN_DELAY).await;
        return ApiError::new(StatusCode::UNAUTHORIZED, "wrong password").into_response();
    }
    state.throttle.succeeded(ip);
    let token = state.sessions.create();
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
    resp
}

pub async fn me() -> Json<serde_json::Value> {
    Json(json!({"ok": true}))
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
        let t = s.create();
        assert!(s.is_valid(&t));
        assert!(!s.is_valid("nope"));
        s.remove(&t);
        assert!(!s.is_valid(&t));
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
