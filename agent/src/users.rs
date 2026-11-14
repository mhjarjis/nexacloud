//! Panel users, roles, two-factor authentication (TOTP) and API tokens.
//!
//! - The built-in `admin` keeps its password in config.toml (so
//!   `nexacloud reset-password` keeps working); its 2FA lives in `users`.
//! - Roles: `admin` (everything), `developer` (read the server, change and
//!   deploy only the projects listed for them), `viewer` (read only).
//! - TOTP: RFC 6238, SHA-1, 6 digits, 30 s, ±1 step, no code reused.
//!   Ten single-use recovery codes (stored as SHA-256).
//! - API tokens: `nc_` + 40 random chars, stored as SHA-256, scope
//!   `full` (acts as its user, counts as sudo), `deploy` (start deploys,
//!   read projects and jobs) or `read` (GET only).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::db::Db;

pub const ADMIN: &str = "admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Developer,
    Viewer,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "developer" => Some(Role::Developer),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Developer => "developer",
            Role::Viewer => "viewer",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub username: String,
    pub role: Role,
    /// Projects a developer may change (site ids).
    pub projects: Vec<String>,
    pub totp_enabled: bool,
    pub created_at: String,
    pub last_login: Option<String>,
    #[serde(skip)]
    pub password_hash: Option<String>,
    #[serde(skip)]
    pub totp_secret: Option<String>,
    #[serde(skip)]
    pub totp_last_step: i64,
    #[serde(skip)]
    pub recovery: Vec<String>,
    pub recovery_left: usize,
}

pub fn valid_username(u: &str) -> bool {
    (2..=32).contains(&u.len())
        && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && u.as_bytes()[0].is_ascii_lowercase()
}

fn now() -> String {
    crate::util::fmt_ts(chrono::Utc::now())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<User> {
    let projects: String = r.get(2)?;
    let recovery: String = r.get(7)?;
    let recovery: Vec<String> = serde_json::from_str(&recovery).unwrap_or_default();
    Ok(User {
        username: r.get(0)?,
        role: Role::parse(&r.get::<_, String>(1)?).unwrap_or(Role::Viewer),
        projects: serde_json::from_str(&projects).unwrap_or_default(),
        password_hash: r.get(3)?,
        totp_secret: r.get(4)?,
        totp_enabled: r.get::<_, i64>(5)? != 0,
        totp_last_step: r.get(6)?,
        recovery_left: recovery.len(),
        recovery,
        created_at: r.get(8)?,
        last_login: r.get(9)?,
    })
}

const COLS: &str = "username, role, projects, password_hash, totp_secret, totp_enabled, totp_last_step, recovery, created_at, last_login";

/// Make sure the built-in admin row exists.
pub fn ensure_admin(db: &Db) -> Result<()> {
    db.conn().execute(
        "INSERT OR IGNORE INTO users (username, role, projects, created_at) VALUES ('admin', 'admin', '[]', ?1)",
        [now()],
    )?;
    Ok(())
}

pub fn list(db: &Db) -> Result<Vec<User>> {
    let c = db.conn();
    let mut st = c.prepare(&format!("SELECT {COLS} FROM users ORDER BY username = 'admin' DESC, username"))?;
    let v = st.query_map([], row)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn get(db: &Db, username: &str) -> Result<Option<User>> {
    Ok(list(db)?.into_iter().find(|u| u.username == username))
}

pub fn create(db: &Db, username: &str, role: Role, projects: &[String], password_hash: &str) -> Result<()> {
    if !valid_username(username) || username == ADMIN {
        bail!("username: 2–32 lowercase letters, digits, . _ - (starting with a letter)");
    }
    let n = db.conn().execute(
        "INSERT OR IGNORE INTO users (username, role, projects, password_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![username, role.as_str(), serde_json::to_string(projects)?, password_hash, now()],
    )?;
    if n == 0 {
        bail!("a user called {username} already exists");
    }
    Ok(())
}

pub fn update(db: &Db, username: &str, role: Option<Role>, projects: Option<&[String]>, password_hash: Option<&str>) -> Result<()> {
    if username == ADMIN && role.is_some_and(|r| r != Role::Admin) {
        bail!("the built-in admin always stays an admin");
    }
    let c = db.conn();
    if let Some(r) = role {
        c.execute("UPDATE users SET role = ?2 WHERE username = ?1", rusqlite::params![username, r.as_str()])?;
    }
    if let Some(p) = projects {
        c.execute("UPDATE users SET projects = ?2 WHERE username = ?1", rusqlite::params![username, serde_json::to_string(p)?])?;
    }
    if let Some(h) = password_hash {
        if username == ADMIN {
            bail!("change the admin password with `sudo nexacloud reset-password`");
        }
        c.execute("UPDATE users SET password_hash = ?2 WHERE username = ?1", rusqlite::params![username, h])?;
    }
    Ok(())
}

pub fn delete(db: &Db, username: &str) -> Result<bool> {
    if username == ADMIN {
        bail!("the built-in admin cannot be removed");
    }
    db.conn().execute("DELETE FROM api_tokens WHERE username = ?1", [username])?;
    Ok(db.conn().execute("DELETE FROM users WHERE username = ?1", [username])? > 0)
}

pub fn touch_login(db: &Db, username: &str) {
    let _ = db.conn().execute("UPDATE users SET last_login = ?2 WHERE username = ?1", [username, &now()]);
}

// ------------------------------------------------------------------ TOTP

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for c in s.trim_end_matches('=').bytes() {
        let v = B32.iter().position(|&x| x == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push(((buf >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

pub fn totp_at(secret: &[u8], step: i64) -> u32 {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret).expect("hmac key");
    mac.update(&(step as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = ((h[off] as u32 & 0x7f) << 24) | ((h[off + 1] as u32) << 16) | ((h[off + 2] as u32) << 8) | h[off + 3] as u32;
    bin % 1_000_000
}

/// The step a code matches (now ±1), if any.
pub fn totp_match(secret_b32: &str, code: &str, now: i64) -> Option<i64> {
    let code = code.trim().replace(' ', "");
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = code.parse().ok()?;
    let secret = base32_decode(secret_b32)?;
    let step = now / 30;
    (step - 1..=step + 1).find(|s| totp_at(&secret, *s) == want)
}

pub fn new_secret() -> String {
    let mut b = [0u8; 20];
    rand::fill(&mut b[..]);
    base32_encode(&b)
}

pub fn otpauth_uri(secret: &str, username: &str, host: &str) -> String {
    let label = format!("NexaCloud ({host}):{username}");
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    format!("otpauth://totp/{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30", enc(&label), enc("NexaCloud"))
}

fn sha(s: &str) -> String {
    crate::util::sha256_hex(s.as_bytes())
}

pub fn set_pending_secret(db: &Db, username: &str, secret: &str) -> Result<()> {
    db.conn().execute("UPDATE users SET totp_secret = ?2, totp_enabled = 0 WHERE username = ?1", [username, secret])?;
    Ok(())
}

/// Turn 2FA on after the first good code; returns 10 recovery codes.
pub fn enable_totp(db: &Db, username: &str, code: &str) -> Result<Vec<String>> {
    let u = get(db, username)?.ok_or_else(|| anyhow::anyhow!("no such user"))?;
    let secret = u.totp_secret.ok_or_else(|| anyhow::anyhow!("start the setup first"))?;
    let Some(step) = totp_match(&secret, code, chrono::Utc::now().timestamp()) else {
        bail!("that code is not right — check the time on your phone and try the next code");
    };
    let codes: Vec<String> = (0..10)
        .map(|_| {
            use rand::distr::{Alphanumeric, SampleString};
            let s = Alphanumeric.sample_string(&mut rand::rng(), 10).to_ascii_lowercase();
            format!("{}-{}", &s[..5], &s[5..])
        })
        .collect();
    let hashes: Vec<String> = codes.iter().map(|c| sha(c)).collect();
    db.conn().execute(
        "UPDATE users SET totp_enabled = 1, totp_last_step = ?2, recovery = ?3 WHERE username = ?1",
        rusqlite::params![username, step, serde_json::to_string(&hashes)?],
    )?;
    Ok(codes)
}

pub fn disable_totp(db: &Db, username: &str) -> Result<()> {
    db.conn().execute("UPDATE users SET totp_enabled = 0, totp_secret = NULL, recovery = '[]' WHERE username = ?1", [username])?;
    Ok(())
}

/// Check a 2FA code or a recovery code (single use) for a user with 2FA on.
pub fn check_second_factor(db: &Db, u: &User, code: &str) -> bool {
    let Some(secret) = &u.totp_secret else { return false };
    if let Some(step) = totp_match(secret, code, chrono::Utc::now().timestamp()) {
        if step <= u.totp_last_step {
            return false; // replay
        }
        let _ = db.conn().execute("UPDATE users SET totp_last_step = ?2 WHERE username = ?1", rusqlite::params![u.username, step]);
        return true;
    }
    let h = sha(&code.trim().to_ascii_lowercase());
    if let Some(i) = u.recovery.iter().position(|x| *x == h) {
        let mut rest = u.recovery.clone();
        rest.remove(i);
        let _ = db.conn().execute(
            "UPDATE users SET recovery = ?2 WHERE username = ?1",
            rusqlite::params![u.username, serde_json::to_string(&rest).unwrap_or_else(|_| "[]".into())],
        );
        return true;
    }
    false
}

// ------------------------------------------------------------------ API tokens

#[derive(Debug, Clone, Serialize)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    pub username: String,
    pub scope: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

pub fn create_token(db: &Db, username: &str, name: &str, scope: &str, expires_days: Option<i64>) -> Result<(ApiToken, String)> {
    if !["full", "deploy", "read"].contains(&scope) {
        bail!("scope must be full, deploy or read");
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 60 {
        bail!("give the token a name (up to 60 characters)");
    }
    let secret = {
        use rand::distr::{Alphanumeric, SampleString};
        format!("nc_{}", Alphanumeric.sample_string(&mut rand::rng(), 40))
    };
    let t = ApiToken {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        username: username.into(),
        scope: scope.into(),
        prefix: secret[..9].to_string(),
        created_at: now(),
        last_used_at: None,
        expires_at: expires_days.filter(|d| *d > 0).map(|d| crate::util::fmt_ts(chrono::Utc::now() + chrono::Duration::days(d))),
    };
    db.conn().execute(
        "INSERT INTO api_tokens (id, name, username, scope, token_hash, prefix, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![t.id, t.name, t.username, t.scope, sha(&secret), t.prefix, t.created_at, t.expires_at],
    )?;
    Ok((t, secret))
}

fn token_row(r: &rusqlite::Row) -> rusqlite::Result<ApiToken> {
    Ok(ApiToken {
        id: r.get(0)?,
        name: r.get(1)?,
        username: r.get(2)?,
        scope: r.get(3)?,
        prefix: r.get(4)?,
        created_at: r.get(5)?,
        last_used_at: r.get(6)?,
        expires_at: r.get(7)?,
    })
}

pub fn list_tokens(db: &Db, username: Option<&str>) -> Result<Vec<ApiToken>> {
    let c = db.conn();
    let mut st = c.prepare("SELECT id, name, username, scope, prefix, created_at, last_used_at, expires_at FROM api_tokens ORDER BY created_at DESC")?;
    let v: Vec<ApiToken> = st.query_map([], token_row)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v.into_iter().filter(|t| username.is_none_or(|u| t.username == u)).collect())
}

pub fn revoke_token(db: &Db, id: &str, username: Option<&str>) -> Result<bool> {
    let n = match username {
        Some(u) => db.conn().execute("DELETE FROM api_tokens WHERE id = ?1 AND username = ?2", [id, u])?,
        None => db.conn().execute("DELETE FROM api_tokens WHERE id = ?1", [id])?,
    };
    Ok(n > 0)
}

/// Resolve `nc_…` to (token, user). Updates last use. None if unknown/expired.
pub fn authenticate_token(db: &Db, secret: &str) -> Option<(ApiToken, User)> {
    if !secret.starts_with("nc_") || secret.len() != 43 {
        return None;
    }
    let h = sha(secret);
    let t = {
        let c = db.conn();
        c.query_row(
            "SELECT id, name, username, scope, prefix, created_at, last_used_at, expires_at FROM api_tokens WHERE token_hash = ?1",
            [&h],
            token_row,
        )
        .ok()?
    };
    if t.expires_at.as_deref().is_some_and(|e| e < now().as_str()) {
        return None;
    }
    let u = get(db, &t.username).ok().flatten()?;
    let _ = db.conn().execute("UPDATE api_tokens SET last_used_at = ?2 WHERE id = ?1", [&t.id, &now()]);
    Some((t, u))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip_and_rfc6238() {
        let k = b"12345678901234567890";
        assert_eq!(base32_decode(&base32_encode(k)).unwrap(), k.to_vec());
        // RFC 6238 SHA-1 test vectors (8-digit values, last 6 digits)
        assert_eq!(totp_at(k, 59 / 30), 287082);
        assert_eq!(totp_at(k, 1111111109 / 30), 81804);
        assert_eq!(totp_at(k, 1234567890 / 30), 5924);
        let s = base32_encode(k);
        assert_eq!(totp_match(&s, "287082", 59), Some(1));
        assert_eq!(totp_match(&s, "287 082", 60), Some(1));
        assert_eq!(totp_match(&s, "000000", 59), None);
        assert!(otpauth_uri(&s, "admin", "panel.example.com").starts_with("otpauth://totp/NexaCloud%20%28panel.example.com%29%3Aadmin?secret="));
    }

    #[test]
    fn users_tokens_and_recovery() {
        let db = Db::open_in_memory().unwrap();
        ensure_admin(&db).unwrap();
        assert!(create(&db, "Bad Name", Role::Viewer, &[], "h").is_err());
        create(&db, "dev1", Role::Developer, &["shop.test".into()], "h").unwrap();
        assert!(create(&db, "dev1", Role::Viewer, &[], "h").is_err());
        assert_eq!(list(&db).unwrap().len(), 2);
        assert!(update(&db, "admin", Some(Role::Viewer), None, None).is_err());
        let (t, secret) = create_token(&db, "dev1", "CI", "deploy", None).unwrap();
        assert!(secret.starts_with("nc_") && secret.len() == 43);
        let (t2, u) = authenticate_token(&db, &secret).unwrap();
        assert_eq!((t2.id.as_str(), u.username.as_str()), (t.id.as_str(), "dev1"));
        assert!(authenticate_token(&db, "nc_wrongwrongwrongwrongwrongwrongwrongwrong").is_none());
        set_pending_secret(&db, "dev1", &base32_encode(b"12345678901234567890")).unwrap();
        // enable needs a current code
        let s = base32_decode(&base32_encode(b"12345678901234567890")).unwrap();
        let code = format!("{:06}", totp_at(&s, chrono::Utc::now().timestamp() / 30));
        let rec = enable_totp(&db, "dev1", &code).unwrap();
        assert_eq!(rec.len(), 10);
        let u = get(&db, "dev1").unwrap().unwrap();
        assert!(u.totp_enabled);
        assert!(!check_second_factor(&db, &u, &code), "same code twice is refused");
        assert!(check_second_factor(&db, &u, &rec[0]));
        let u = get(&db, "dev1").unwrap().unwrap();
        assert!(!check_second_factor(&db, &u, &rec[0]), "recovery codes are single use");
        assert_eq!(u.recovery_left, 9);
        assert!(delete(&db, "dev1").unwrap());
        assert!(list_tokens(&db, None).unwrap().is_empty());
    }
}
