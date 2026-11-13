//! Append-only audit log (`audit_log` table; UPDATE/DELETE are rejected by
//! triggers).
//!
//! Every mutating API request (POST/PUT/PATCH/DELETE under `/api`) is recorded
//! by [`middleware`]: time, client IP, action, target, change/job id and the
//! result. Handlers name the action precisely by attaching an [`AuditInfo`]
//! to their response with [`audited`]; without one, the action is
//! `"<METHOD> <path>"`. Request bodies are never logged.
//!
//! Background work records its own outcome with [`record`] (ip = null), e.g.
//! `change.applied` / `change.reverted` when a change job finishes.

use std::net::SocketAddr;

use anyhow::Result;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::error::ErrorText;
use crate::state::AppState;
use crate::util::fmt_ts;

/// Default and maximum `limit` for `GET /api/audit`.
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

/// One audit row.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: String,
    /// Client IP, `cli` for the command line, null for the agent itself.
    pub ip: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub change_id: Option<String>,
    pub job_id: Option<String>,
    /// SHA-256 of the change plan (what exactly was applied).
    pub plan_hash: Option<String>,
    /// `ok`, or `<http status> <error>` / a failure reason.
    pub result: String,
}

/// Response extension a handler attaches to describe what it did.
#[derive(Debug, Clone, Default)]
pub struct AuditInfo {
    pub action: Option<String>,
    pub target: Option<String>,
    pub change_id: Option<String>,
    pub job_id: Option<String>,
    pub plan_hash: Option<String>,
    /// Don't record this request (read-only POSTs such as previews).
    pub skip: bool,
}

impl AuditInfo {
    pub fn action(action: &str, target: impl Into<String>) -> Self {
        AuditInfo {
            action: Some(action.to_string()),
            target: Some(target.into()),
            ..Default::default()
        }
    }
    pub fn skip() -> Self {
        AuditInfo {
            skip: true,
            ..Default::default()
        }
    }
    pub fn change(mut self, id: &str) -> Self {
        self.change_id = Some(id.to_string());
        self
    }
    pub fn job(mut self, id: &str) -> Self {
        self.job_id = Some(id.to_string());
        self
    }
}

/// Attach audit details to a handler's response.
pub fn audited(resp: impl IntoResponse, info: AuditInfo) -> Response {
    let mut r = resp.into_response();
    r.extensions_mut().insert(info);
    r
}

/// Append one row. Failures are logged, never propagated: auditing must not
/// break the action it describes.
pub fn record(db: &Db, e: &AuditEntry) {
    let res = db.conn().execute(
        "INSERT INTO audit_log (ts, ip, action, target, change_id, job_id, plan_hash, result)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            if e.ts.is_empty() {
                fmt_ts(Utc::now())
            } else {
                e.ts.clone()
            },
            e.ip,
            e.action,
            e.target,
            e.change_id,
            e.job_id,
            e.plan_hash,
            e.result
        ],
    );
    if let Err(err) = res {
        tracing::error!("cannot write audit log: {err}");
    }
}

/// Newest first.
pub fn list(db: &Db, limit: usize) -> Result<Vec<AuditEntry>> {
    let c = db.conn();
    let mut st = c.prepare(
        "SELECT id, ts, ip, action, target, change_id, job_id, plan_hash, result
         FROM audit_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = st.query_map([limit.clamp(1, MAX_LIMIT) as i64], |r| {
        Ok(AuditEntry {
            id: r.get(0)?,
            ts: r.get(1)?,
            ip: r.get(2)?,
            action: r.get(3)?,
            target: r.get(4)?,
            change_id: r.get(5)?,
            job_id: r.get(6)?,
            plan_hash: r.get(7)?,
            result: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn is_mutating(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Records every mutating `/api` request after it has been handled.
pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !is_mutating(req.method()) {
        return next.run(req).await;
    }
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_canonical().to_string());
    // Nested routers see a stripped path; OriginalUri keeps `/api/...`.
    let path = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let default_action = format!("{} {path}", req.method());
    let resp = next.run(req).await;
    let info = resp
        .extensions()
        .get::<AuditInfo>()
        .cloned()
        .unwrap_or_default();
    if info.skip {
        return resp;
    }
    let status = resp.status();
    let result = if status.is_success() {
        "ok".to_string()
    } else {
        let msg = resp
            .extensions()
            .get::<ErrorText>()
            .map(|e| e.0.clone())
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
        format!("{} {}", status.as_u16(), msg)
    };
    record(
        &state.db,
        &AuditEntry {
            ip,
            action: info.action.unwrap_or(default_action),
            target: info.target,
            change_id: info.change_id,
            job_id: info.job_id,
            plan_hash: info.plan_hash,
            result,
            ..Default::default()
        },
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_list_newest_first() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..3 {
            record(
                &db,
                &AuditEntry {
                    ip: Some("203.0.113.5".into()),
                    action: format!("a{i}"),
                    result: "ok".into(),
                    ..Default::default()
                },
            );
        }
        let rows = list(&db, 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, "a2");
        assert_eq!(rows[1].action, "a1");
        assert!(!rows[0].ts.is_empty());
    }
}
