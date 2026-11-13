//! `/api/monitor…` and `/api/notify…`.

use axum::extract::{Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::notify::{self, Settings};
use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct HoursQuery {
    hours: Option<i64>,
}

/// Every probed site with its last-24h numbers.
pub async fn list(State(state): State<AppState>) -> Json<Value> {
    let targets = super::targets().await;
    let v: Vec<Value> = targets
        .iter()
        .map(|t| json!(super::stats(&state.db, &t.id, 24)))
        .collect();
    Json(json!(v))
}

pub async fn site(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<HoursQuery>,
) -> Json<Value> {
    Json(json!(super::stats(&state.db, &id, q.hours.unwrap_or(24))))
}

pub async fn notifications(State(state): State<AppState>) -> Json<Value> {
    Json(json!(notify::recent(&state, 50)))
}

pub async fn settings_get(State(state): State<AppState>) -> Json<Value> {
    Json(Settings::load(&state).redacted())
}

pub async fn settings_put(State(state): State<AppState>, Json(b): Json<Settings>) -> Response {
    let info = AuditInfo::action("notify.settings", "server");
    let res: ApiResult<Response> = (|| {
        let s = Settings::load(&state).merged(b);
        s.validate().map_err(ApiError::bad_request)?;
        s.save(&state)?;
        Ok(Json(s.redacted()).into_response())
    })();
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

/// Send a test message to every enabled channel and report per channel.
pub async fn test(State(state): State<AppState>) -> Response {
    let info = AuditInfo::action("notify.test", "server");
    let s = Settings::load(&state);
    let res: ApiResult<Response> = async {
        if !s.telegram.enabled && !s.email.enabled {
            return Err(ApiError::bad_request(
                "turn on Telegram or email first, then save",
            ));
        }
        let r = notify::deliver(
            &s,
            "Test notification",
            "NexaCloud can reach you. You will get messages like this when a site goes down, a deploy or backup fails, or the server needs attention.",
        )
        .await;
        Ok(Json(json!(r
            .into_iter()
            .map(|(c, e)| json!({"channel": c, "ok": e.is_none(), "error": e}))
            .collect::<Vec<_>>()))
        .into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}
