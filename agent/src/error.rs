//! JSON error type and the middleware that guarantees every /api error is JSON.

use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Response extension carrying an error's message (read by the audit log).
#[derive(Debug, Clone)]
pub struct ErrorText(pub String);

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    /// `403 {"error":"sudo_required"}`: re-enter the password via
    /// `POST /api/auth/sudo`, then retry.
    pub fn sudo_required() -> Self {
        Self::new(StatusCode::FORBIDDEN, "sudo_required")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (self.status, Json(json!({ "error": self.message }))).into_response();
        r.extensions_mut().insert(ErrorText(self.message));
        r
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("internal error: {e:#}");
        ApiError::internal(format!("{e:#}"))
    }
}

/// Rewrites any non-JSON 4xx/5xx response (axum extractor rejections, 405s,
/// ...) into `{"error": "..."}`.
pub async fn json_errors(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let status = resp.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return resp;
    }
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.as_bytes().starts_with(b"application/json"))
        .unwrap_or(false);
    if is_json {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let bytes = to_bytes(body, 16 * 1024).await.unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    let msg = if text.is_empty() || text.len() > 500 {
        status
            .canonical_reason()
            .unwrap_or("error")
            .to_ascii_lowercase()
    } else {
        text
    };
    let mut out = ApiError::new(status, msg).into_response();
    // Keep useful headers such as `Allow` on 405.
    if let Some(allow) = parts.headers.get(header::ALLOW) {
        out.headers_mut().insert(header::ALLOW, allow.clone());
    }
    out
}
