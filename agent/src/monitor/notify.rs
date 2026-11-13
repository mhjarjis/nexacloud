//! Notifications: stored in `notifications`, sent to Telegram and/or email.
//!
//! Secrets (bot token, SMTP password) live in the root-only state database
//! and are handed to curl through a config file on stdin / a 0600 temp file,
//! never on the command line.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;
use crate::util::cmd::Cmd;

const SETTINGS_KEY: &str = "notify";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SiteDown,
    DeployFailed,
    BackupFailed,
    Server,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Telegram {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Email {
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// starttls | tls | none
    pub security: String,
    pub user: String,
    pub password: String,
    pub from: String,
    pub to: String,
}

impl Default for Email {
    fn default() -> Self {
        Email {
            enabled: false,
            smtp_host: String::new(),
            smtp_port: 587,
            security: "starttls".into(),
            user: String::new(),
            password: String::new(),
            from: String::new(),
            to: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Events {
    pub site_down: bool,
    pub deploy_failed: bool,
    pub backup_failed: bool,
    pub server: bool,
}

impl Default for Events {
    fn default() -> Self {
        Events {
            site_down: true,
            deploy_failed: true,
            backup_failed: true,
            server: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub telegram: Telegram,
    pub email: Email,
    pub events: Events,
}

impl Settings {
    pub fn load(state: &AppState) -> Settings {
        state
            .db
            .setting(SETTINGS_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, state: &AppState) -> Result<()> {
        state
            .db
            .set_setting(SETTINGS_KEY, &serde_json::to_string(self)?)
    }

    /// For the browser: secrets replaced by a "set" flag.
    pub fn redacted(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        v["telegram"]["bot_token"] = Value::String(String::new());
        v["telegram"]["has_token"] = Value::Bool(!self.telegram.bot_token.is_empty());
        v["email"]["password"] = Value::String(String::new());
        v["email"]["has_password"] = Value::Bool(!self.email.password.is_empty());
        v
    }

    /// Apply an update from the browser: empty secrets keep the stored ones.
    pub fn merged(&self, mut new: Settings) -> Settings {
        if new.telegram.bot_token.is_empty() {
            new.telegram.bot_token = self.telegram.bot_token.clone();
        }
        if new.email.password.is_empty() {
            new.email.password = self.email.password.clone();
        }
        new
    }

    pub fn validate(&self) -> Result<(), String> {
        let t = &self.telegram;
        if !t.bot_token.is_empty()
            && !t
                .bot_token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":_-".contains(&b))
        {
            return Err("the bot token looks wrong (expected 123456:ABC…)".into());
        }
        if !t.chat_id.is_empty()
            && (t.chat_id.len() > 64
                || !t
                    .chat_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_@".contains(&b)))
        {
            return Err("the chat id looks wrong (a number like -100123… or @channel)".into());
        }
        if t.enabled && (t.bot_token.is_empty() || t.chat_id.is_empty()) {
            return Err("Telegram needs a bot token and a chat id".into());
        }
        let e = &self.email;
        if !e.smtp_host.is_empty()
            && !e
                .smtp_host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            return Err("SMTP host: a hostname like smtp.example.com".into());
        }
        if !["starttls", "tls", "none"].contains(&e.security.as_str()) {
            return Err("security must be starttls, tls or none".into());
        }
        for (what, addr) in [("from", &e.from), ("to", &e.to)] {
            if !addr.is_empty() && !valid_email(addr) {
                return Err(format!("the {what} address is not valid"));
            }
        }
        if e.user.len() > 200 || e.password.len() > 500 {
            return Err("SMTP user or password too long".into());
        }
        if e.enabled && (e.smtp_host.is_empty() || e.from.is_empty() || e.to.is_empty()) {
            return Err("email needs an SMTP host, a from and a to address".into());
        }
        Ok(())
    }

    fn wants(&self, ev: Event) -> bool {
        match ev {
            Event::SiteDown => self.events.site_down,
            Event::DeployFailed => self.events.deploy_failed,
            Event::BackupFailed => self.events.backup_failed,
            Event::Server => self.events.server,
        }
    }
}

pub fn valid_email(a: &str) -> bool {
    let Some((user, host)) = a.split_once('@') else {
        return false;
    };
    a.len() <= 200
        && !user.is_empty()
        && host.contains('.')
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._%+-".contains(&b))
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
}

/// Quote a value for a curl config file.
fn cq(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn telegram_api() -> String {
    std::env::var("NEXACLOUD_TELEGRAM_API").unwrap_or_else(|_| "https://api.telegram.org".into())
}

pub async fn send_telegram(t: &Telegram, text: &str) -> Result<()> {
    let config = format!(
        "url = {}\ndata-urlencode = {}\ndata-urlencode = {}\nsilent\nshow-error\nfail-with-body\nmax-time = 20\n",
        cq(&format!("{}/bot{}/sendMessage", telegram_api(), t.bot_token)),
        cq(&format!("chat_id={}", t.chat_id)),
        cq(&format!("text={text}")),
    );
    let o = Cmd::new("curl")
        .args(["-K", "-"])
        .stdin(config.into_bytes())
        .timeout(Duration::from_secs(30))
        .run()
        .await;
    if o.ok {
        Ok(())
    } else {
        // never echo the URL (it holds the token)
        let body = o.stdout.replace(&t.bot_token, "•••");
        bail!(
            "Telegram refused the message: {}",
            crate::util::cmd::truncate(body.trim(), 200)
        )
    }
}

pub async fn send_email(e: &Email, subject: &str, body: &str) -> Result<()> {
    let scheme = if e.security == "tls" { "smtps" } else { "smtp" };
    let mut config = format!(
        "url = {}\nmail-from = {}\nmail-rcpt = {}\nupload-file = \"-\"\nsilent\nshow-error\nmax-time = 30\n",
        cq(&format!("{scheme}://{}:{}", e.smtp_host, e.smtp_port)),
        cq(&e.from),
        cq(&e.to),
    );
    if e.security == "starttls" {
        config.push_str("ssl-reqd\n");
    }
    if !e.user.is_empty() {
        config.push_str(&format!("user = {}\n", cq(&format!("{}:{}", e.user, e.password))));
    }
    let dir = std::env::temp_dir().join(format!("nexacloud-mail-{}", uuid::Uuid::new_v4()));
    crate::util::create_private_dir(&dir)?;
    let cfg = dir.join("curl.conf");
    crate::util::write_private_file(&cfg, config.as_bytes())?;
    let subject: String = subject.chars().filter(|c| !c.is_control()).collect();
    let msg = format!(
        "From: NexaCloud <{}>\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
        e.from,
        e.to,
        subject,
        chrono::Utc::now().to_rfc2822(),
        body.replace('\n', "\r\n")
    );
    let o = Cmd::new("curl")
        .args(["-K", &cfg.to_string_lossy()])
        .stdin(msg.into_bytes())
        .timeout(Duration::from_secs(45))
        .run()
        .await;
    let _ = std::fs::remove_dir_all(&dir);
    if o.ok {
        Ok(())
    } else {
        Err(anyhow!(
            "{}",
            o.error_summary("sending email").replace(&e.password, "•••")
        ))
    }
}

/// Send to every enabled channel; returns `[(channel, error)]`.
pub async fn deliver(s: &Settings, title: &str, body: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let host = crate::routes::system::hostname();
    if s.telegram.enabled {
        let r = send_telegram(&s.telegram, &format!("{title}\n{body}\n— NexaCloud on {host}")).await;
        out.push(("telegram".into(), r.err().map(|e| format!("{e:#}"))));
    }
    if s.email.enabled {
        let r = send_email(
            &s.email,
            &format!("[NexaCloud] {title}"),
            &format!("{body}\n\n— NexaCloud on {host}"),
        )
        .await;
        out.push(("email".into(), r.err().map(|e| format!("{e:#}"))));
    }
    out
}

fn recently_sent(state: &AppState, key: &str, within: Duration) -> bool {
    let since = crate::util::fmt_ts(
        chrono::Utc::now() - chrono::Duration::seconds(within.as_secs() as i64),
    );
    state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE key = ?1 AND at >= ?2",
            [key, &since],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Record a notification and deliver it in the background. With `dedupe`,
/// the same key is not repeated inside that window.
pub fn send(
    state: &AppState,
    ev: Event,
    key: &str,
    level: &str,
    title: &str,
    body: &str,
    dedupe: Option<Duration>,
) {
    if let Some(w) = dedupe {
        if recently_sent(state, key, w) {
            return;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = state.db.conn().execute(
        "INSERT INTO notifications (id, at, level, key, title, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, crate::util::fmt_ts(chrono::Utc::now()), level, key, title, body],
    );
    let settings = Settings::load(state);
    if !settings.wants(ev) {
        return;
    }
    let (state, title, body) = (state.clone(), title.to_string(), body.to_string());
    tokio::spawn(async move {
        let res = deliver(&settings, &title, &body).await;
        let sent = res
            .iter()
            .map(|(c, e)| match e {
                None => format!("{c}:ok"),
                Some(e) => format!("{c}:{}", crate::util::cmd::truncate(e, 120)),
            })
            .collect::<Vec<_>>()
            .join("; ");
        let _ = state.db.conn().execute(
            "UPDATE notifications SET sent = ?1 WHERE id = ?2",
            [&sent, &id],
        );
    });
}

#[derive(Debug, Serialize)]
pub struct Notification {
    pub id: String,
    pub at: String,
    pub level: String,
    pub key: String,
    pub title: String,
    pub body: String,
    pub sent: String,
}

pub fn recent(state: &AppState, limit: usize) -> Vec<Notification> {
    let c = state.db.conn();
    let Ok(mut st) = c.prepare(&format!(
        "SELECT id, at, level, key, title, body, sent FROM notifications ORDER BY at DESC LIMIT {}",
        limit.min(200)
    )) else {
        return Vec::new();
    };
    st.query_map([], |r| {
        Ok(Notification {
            id: r.get(0)?,
            at: r.get(1)?,
            level: r.get(2)?,
            key: r.get(3)?,
            title: r.get(4)?,
            body: r.get(5)?,
            sent: r.get(6)?,
        })
    })
    .map(|it| it.flatten().collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_config_quoting() {
        assert_eq!(cq("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn settings_validation_and_merge() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.telegram.enabled = true;
        assert!(s.validate().is_err());
        s.telegram.bot_token = "123:abc_DEF-x".into();
        s.telegram.chat_id = "-100123".into();
        assert!(s.validate().is_ok());
        s.telegram.bot_token = "123 abc".into();
        assert!(s.validate().is_err());
        s.telegram.bot_token = "123:abc".into();
        s.email.to = "not an email".into();
        assert!(s.validate().is_err());
        s.email.to = "ops@example.com".into();
        assert!(s.validate().is_ok());
        let mut upd = s.clone();
        upd.telegram.bot_token.clear();
        let m = s.merged(upd);
        assert_eq!(m.telegram.bot_token, "123:abc");
        let r = m.redacted();
        assert_eq!(r["telegram"]["bot_token"], "");
        assert_eq!(r["telegram"]["has_token"], true);
    }
}
