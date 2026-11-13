//! `/api/changes`, `/api/advisor/*`, `/api/audit`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::advisor::nginx_hardening;
use crate::audit::{self, audited, AuditEntry, AuditInfo};
use crate::changes::{self, ChangeDetail, ChangeSummary, RollbackError};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn client_ip(addr: &SocketAddr) -> String {
    addr.ip().to_canonical().to_string()
}

#[derive(Deserialize)]
pub struct LimitQuery {
    limit: Option<usize>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<ChangeSummary>>> {
    let limit = q.limit.unwrap_or(changes::LIST_LIMIT).clamp(1, 500);
    let rows = changes::list_changes(&state.db, limit)?;
    Ok(Json(
        rows.iter()
            .map(|r| changes::summary(&state.changes_env, r))
            .collect(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<ChangeDetail>> {
    let rec = changes::get_change(&state.db, &id)?
        .ok_or_else(|| ApiError::not_found("change not found"))?;
    Ok(Json(changes::detail(&state.changes_env, &rec)))
}

#[derive(Deserialize, Default)]
struct RollbackBody {
    #[serde(default)]
    force: bool,
}

/// `POST /api/changes/{id}/rollback` `{"force": false}` (body optional; sudo).
pub async fn rollback(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Response {
    let mut info = AuditInfo::action("change.rollback", id.clone()).change(&id);
    let res: ApiResult<Response> = (|| {
        let b: RollbackBody = if body.iter().all(u8::is_ascii_whitespace) {
            RollbackBody::default()
        } else {
            serde_json::from_slice(&body)
                .map_err(|_| ApiError::bad_request("invalid body: expected {\"force\": bool}"))?
        };
        let plan = changes::plan_rollback(&state.changes_env, &state.db, &id, b.force).map_err(
            |e| match e {
                RollbackError::NotFound => ApiError::not_found("change not found"),
                RollbackError::NotRollbackable(_) | RollbackError::Drift(_) => {
                    ApiError::conflict(e.to_string())
                }
                RollbackError::Other(e) => ApiError::from(e),
            },
        )?;
        info.plan_hash = Some(plan.hash());
        let (change_id, job_id) = changes::submit_apply(
            &state.jobs,
            &state.db,
            &state.changes_env,
            plan,
            Some(client_ip(&addr)),
        )?;
        info.job_id = Some(job_id.clone());
        Ok(
            Json(json!({"change_id": change_id, "job_id": job_id, "rollback_of": id}))
                .into_response(),
        )
    })();
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ advisor

/// `POST /api/advisor/nginx-hardening/preview` (read-only; not audited).
pub async fn nginx_hardening_preview(State(state): State<AppState>) -> Response {
    let res = match nginx_hardening::plan(&state.changes_env).await {
        Ok(plan) => Json(changes::preview(&plan)).into_response(),
        Err(e) => ApiError::conflict(e).into_response(),
    };
    audited(res, AuditInfo::skip())
}

#[derive(Deserialize, Default)]
struct ApplyBody {
    /// `plan_hash` from the preview: refuse if the server changed since.
    plan_hash: Option<String>,
}

/// `POST /api/advisor/nginx-hardening/apply` `{"plan_hash": "..."}` (sudo).
pub async fn nginx_hardening_apply(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let mut info = AuditInfo::action("change.apply", "nginx");
    let res: ApiResult<Response> = async {
        let b: ApplyBody = if body.iter().all(u8::is_ascii_whitespace) {
            ApplyBody::default()
        } else {
            serde_json::from_slice(&body).map_err(|_| {
                ApiError::bad_request("invalid body: expected {\"plan_hash\": \"...\"}")
            })?
        };
        // Never trust a plan from the client: rebuild it here.
        let plan = nginx_hardening::plan(&state.changes_env)
            .await
            .map_err(ApiError::conflict)?;
        if plan.is_noop() {
            return Err(ApiError::conflict(
                plan.note.unwrap_or_else(|| "nothing to do".into()),
            ));
        }
        let hash = plan.hash();
        info.plan_hash = Some(hash.clone());
        if let Some(expected) = b.plan_hash {
            if expected != hash {
                return Err(ApiError::conflict(
                    "the server changed since the preview; preview again",
                ));
            }
        }
        let (change_id, job_id) = changes::submit_apply(
            &state.jobs,
            &state.db,
            &state.changes_env,
            plan,
            Some(client_ip(&addr)),
        )?;
        info.change_id = Some(change_id.clone());
        info.job_id = Some(job_id.clone());
        Ok(
            Json(json!({"change_id": change_id, "job_id": job_id, "plan_hash": hash}))
                .into_response(),
        )
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ audit

pub async fn audit_list(
    State(state): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    let limit = q.limit.unwrap_or(audit::DEFAULT_LIMIT);
    Ok(Json(audit::list(&state.db, limit)?))
}
