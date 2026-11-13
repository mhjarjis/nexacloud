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

/// CPU / memory history (48 points over 24 h) and network traffic.
pub async fn server_history(State(state): State<AppState>) -> Json<Value> {
    let now = chrono::Utc::now().timestamp();
    let rows: Vec<(i64, f64, f64, i64, i64)> = {
        let c = state.db.conn();
        let mut st = match c.prepare("SELECT ts, cpu, mem, rx, tx FROM server_samples WHERE ts >= ?1 ORDER BY ts") {
            Ok(s) => s,
            Err(_) => return Json(json!({})),
        };
        st.query_map([now - 31 * 86400], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    };
    let day: Vec<&(i64, f64, f64, i64, i64)> = rows.iter().filter(|r| r.0 >= now - 86400).collect();
    let step = 1800;
    let first = (now / step - 47) * step;
    let points: Vec<Value> = (0..48)
        .map(|i| {
            let from = first + i * step;
            let inb: Vec<_> = day.iter().filter(|r| r.0 >= from && r.0 < from + step).collect();
            if inb.is_empty() {
                json!({"ts": from, "cpu": null, "mem": null})
            } else {
                let n = inb.len() as f64;
                json!({"ts": from, "cpu": inb.iter().map(|r| r.1).sum::<f64>() / n, "mem": inb.iter().map(|r| r.2).sum::<f64>() / n})
            }
        })
        .collect();
    let net_day: Vec<(i64, i64, i64)> = day.iter().map(|r| (r.0, r.3, r.4)).collect();
    let month_start = {
        use chrono::Datelike;
        let t = chrono::Utc::now();
        chrono::NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp())
            .unwrap_or(now - 30 * 86400)
    };
    let net_month: Vec<(i64, i64, i64)> = rows.iter().filter(|r| r.0 >= month_start).map(|r| (r.0, r.3, r.4)).collect();
    let (rx24, tx24) = super::traffic(&net_day);
    let (rxm, txm) = super::traffic(&net_month);
    // per-30-min traffic for the sparkline
    let net_points: Vec<i64> = (0..48)
        .map(|i| {
            let from = first + i * step;
            let w: Vec<(i64, i64, i64)> = net_day.iter().filter(|r| r.0 >= from && r.0 < from + step + 60).cloned().collect();
            let (a, b) = super::traffic(&w);
            a + b
        })
        .collect();
    Json(json!({
        "points": points,
        "net": {"rx_24h": rx24, "tx_24h": tx24, "rx_month": rxm, "tx_month": txm, "points": net_points, "since": rows.first().map(|r| r.0)},
    }))
}
