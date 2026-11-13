//! `/api/jobs`: list, detail, cancel, live log over SSE.

use std::convert::Infallible;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::audit::{audited, AuditInfo};
use crate::changes;
use crate::error::{ApiError, ApiResult};
use crate::jobs::{CancelError, Job, LIST_LIMIT};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<usize>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<Job>>> {
    let limit = q.limit.unwrap_or(LIST_LIMIT).clamp(1, 500);
    Ok(Json(state.jobs.list(limit)?))
}

pub async fn get(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Job>> {
    state
        .jobs
        .get(&id)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("job not found"))
}

/// `POST /api/jobs/{id}/cancel` (sudo).
pub async fn cancel(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("job.cancel", id.clone()).job(&id);
    let res = match state.jobs.cancel(&id) {
        Ok(()) => {
            if let Some(cid) = state.jobs.get(&id).ok().flatten().and_then(|j| j.change_id) {
                let _ = changes::mark_cancelled_if_pending(&state.db, &cid);
            }
            Json(json!({"ok": true})).into_response()
        }
        Err(CancelError::NotFound) => ApiError::not_found("job not found").into_response(),
        Err(CancelError::AlreadyFinished) => {
            ApiError::conflict("the job already finished").into_response()
        }
        Err(CancelError::NotCancellable) => ApiError::conflict(
            "this job applies a change and can't be stopped half-way; it will finish or revert on its own",
        )
        .into_response(),
    };
    audited(res, info)
}

/// Read what was appended to the log since `offset` (at most 256 KiB).
fn read_more(path: &Path, offset: &mut u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.seek(SeekFrom::Start(*offset)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = (&mut f).take(256 * 1024).read_to_end(&mut buf);
    *offset += buf.len() as u64;
    String::from_utf8_lossy(&buf).into_owned()
}

fn log_event(line: &str) -> Event {
    Event::default().event("log").data(line.replace('\r', ""))
}

/// `GET /api/jobs/{id}/stream`: `text/event-stream`. Sends the existing log
/// as `log` events (one per line), follows new lines, and ends with one
/// `status` event `{"status": "...", "error": ...}` when the job finishes.
pub async fn stream(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Response> {
    if state.jobs.status(&id).is_none() {
        return Err(ApiError::not_found("job not found"));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    let jobs = state.jobs.clone();
    let path = jobs.log_path(&id);
    tokio::spawn(async move {
        let mut offset = 0u64;
        let mut partial = String::new();
        loop {
            // Status first: a job writes its last line before its final
            // status, so once we see "finished" the file read below has it all.
            let status = jobs.status(&id);
            loop {
                let chunk = read_more(&path, &mut offset);
                if chunk.is_empty() {
                    break;
                }
                partial.push_str(&chunk);
                while let Some(i) = partial.find('\n') {
                    let line: String = partial.drain(..=i).collect();
                    if tx
                        .send(log_event(line.trim_end_matches('\n')))
                        .await
                        .is_err()
                    {
                        return; // client went away
                    }
                }
            }
            match status {
                Some((s, error)) if s.is_finished() => {
                    if !partial.is_empty() {
                        let _ = tx.send(log_event(&partial)).await;
                    }
                    let ev = Event::default()
                        .event("status")
                        .json_data(json!({"status": s, "error": error}))
                        .unwrap_or_else(|_| Event::default().event("status").data("{}"));
                    let _ = tx.send(ev).await;
                    return;
                }
                None => return,
                _ => {}
            }
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|e| (Ok::<Event, Infallible>(e), rx))
    });
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}
