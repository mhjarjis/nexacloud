//! `/api/account/*` (your own 2FA, password, API tokens) and `/api/users/*`
//! (admins manage panel users).

use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit::{audited, AuditInfo};
use crate::auth::Identity;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::users::{self, Role};

fn done(res: ApiResult<Response>, info: AuditInfo) -> Response {
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn no_tokens(me: &Identity) -> ApiResult<()> {
    if me.token_scope.is_some() {
        return Err(ApiError::new(axum::http::StatusCode::FORBIDDEN, "sign in to the panel to change account security"));
    }
    Ok(())
}

// ------------------------------------------------------------------ 2FA

/// Start 2FA setup: a new secret + otpauth URI (shown as a QR code).
pub async fn totp_setup(State(state): State<AppState>, Extension(me): Extension<Identity>, headers: axum::http::HeaderMap) -> Response {
    let info = AuditInfo::action("account.2fa_setup", me.username.clone());
    let res = (|| {
        no_tokens(&me)?;
        let secret = users::new_secret();
        users::set_pending_secret(&state.db, &me.username, &secret)?;
        let host = headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("panel").split(':').next().unwrap_or("panel").to_string();
        Ok(Json(json!({"secret": secret, "uri": users::otpauth_uri(&secret, &me.username, &host)})).into_response())
    })();
    done(res, info)
}

#[derive(Deserialize)]
pub struct CodeBody {
    code: String,
}

pub async fn totp_enable(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<CodeBody>) -> Response {
    let info = AuditInfo::action("account.2fa_enable", me.username.clone());
    let res = (|| {
        no_tokens(&me)?;
        let codes = users::enable_totp(&state.db, &me.username, &b.code).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(json!({"enabled": true, "recovery_codes": codes})).into_response())
    })();
    done(res, info)
}

pub async fn totp_disable(State(state): State<AppState>, Extension(me): Extension<Identity>) -> Response {
    let info = AuditInfo::action("account.2fa_disable", me.username.clone());
    let res = (|| {
        no_tokens(&me)?;
        users::disable_totp(&state.db, &me.username)?;
        Ok(Json(json!({"enabled": false})).into_response())
    })();
    done(res, info)
}

// ------------------------------------------------------------------ password

#[derive(Deserialize)]
pub struct PasswordBody {
    password: String,
}

fn check_new_password(p: &str) -> ApiResult<()> {
    if p.chars().count() < 12 || p.len() > 1024 {
        return Err(ApiError::bad_request("use at least 12 characters"));
    }
    Ok(())
}

/// Change your own password (not the built-in admin: that one is in config).
pub async fn change_password(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<PasswordBody>) -> Response {
    let info = AuditInfo::action("account.password", me.username.clone());
    let res = async {
        no_tokens(&me)?;
        check_new_password(&b.password)?;
        let p = b.password.clone();
        let hash = tokio::task::spawn_blocking(move || crate::auth::hash_password(&p))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))??;
        users::update(&state.db, &me.username, None, None, Some(&hash)).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    done(res, info)
}

// ------------------------------------------------------------------ API tokens

pub async fn tokens(State(state): State<AppState>, Extension(me): Extension<Identity>) -> ApiResult<Json<Value>> {
    let mine = if me.role == Role::Admin && me.token_scope.is_none() { None } else { Some(me.username.as_str()) };
    Ok(Json(json!(users::list_tokens(&state.db, mine)?)))
}

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    scope: String,
    #[serde(default)]
    expires_days: Option<i64>,
}

pub async fn token_create(State(state): State<AppState>, Extension(me): Extension<Identity>, Json(b): Json<NewToken>) -> Response {
    let info = AuditInfo::action("account.token_create", format!("{}: {}", me.username, b.name));
    let res = (|| {
        no_tokens(&me)?;
        if me.role == Role::Viewer && b.scope != "read" {
            return Err(ApiError::bad_request("viewers can only create read tokens"));
        }
        let (t, secret) = users::create_token(&state.db, &me.username, &b.name, &b.scope, b.expires_days).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(json!({"token": t, "secret": secret})).into_response())
    })();
    done(res, info)
}

pub async fn token_revoke(State(state): State<AppState>, Extension(me): Extension<Identity>, UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("account.token_revoke", id.clone());
    let res = (|| {
        let scope = if me.role == Role::Admin { None } else { Some(me.username.as_str()) };
        if users::revoke_token(&state.db, &id, scope)? {
            Ok(Json(json!({"ok": true})).into_response())
        } else {
            Err(ApiError::not_found("token not found"))
        }
    })();
    done(res, info)
}

// ------------------------------------------------------------------ users (admin)

pub async fn users_list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(users::list(&state.db)?)))
}

#[derive(Deserialize)]
pub struct NewUser {
    username: String,
    role: String,
    #[serde(default)]
    projects: Vec<String>,
    password: String,
}

pub async fn user_create(State(state): State<AppState>, Json(b): Json<NewUser>) -> Response {
    let info = AuditInfo::action("users.create", b.username.clone());
    let res = async {
        let role = Role::parse(&b.role).ok_or_else(|| ApiError::bad_request("role must be admin, developer or viewer"))?;
        check_new_password(&b.password)?;
        let p = b.password.clone();
        let hash = tokio::task::spawn_blocking(move || crate::auth::hash_password(&p))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))??;
        users::create(&state.db, &b.username.trim().to_ascii_lowercase(), role, &b.projects, &hash).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    done(res, info)
}

#[derive(Deserialize)]
pub struct EditUser {
    role: Option<String>,
    projects: Option<Vec<String>>,
    password: Option<String>,
    #[serde(default)]
    reset_2fa: bool,
}

pub async fn user_update(State(state): State<AppState>, UrlPath(name): UrlPath<String>, Json(b): Json<EditUser>) -> Response {
    let info = AuditInfo::action("users.update", name.clone());
    let res = async {
        users::get(&state.db, &name)?.ok_or_else(|| ApiError::not_found("no such user"))?;
        let role = match &b.role {
            Some(r) => Some(Role::parse(r).ok_or_else(|| ApiError::bad_request("role must be admin, developer or viewer"))?),
            None => None,
        };
        let hash = match &b.password {
            Some(p) => {
                check_new_password(p)?;
                let p = p.clone();
                Some(tokio::task::spawn_blocking(move || crate::auth::hash_password(&p)).await.map_err(|e| ApiError::internal(e.to_string()))??)
            }
            None => None,
        };
        users::update(&state.db, &name, role, b.projects.as_deref(), hash.as_deref()).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        if b.reset_2fa {
            users::disable_totp(&state.db, &name)?;
        }
        if hash.is_some() || b.reset_2fa {
            state.sessions.remove_user(&name);
        }
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    done(res, info)
}

pub async fn user_delete(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let info = AuditInfo::action("users.delete", name.clone());
    let res = (|| {
        if users::delete(&state.db, &name).map_err(|e| ApiError::bad_request(format!("{e:#}")))? {
            state.sessions.remove_user(&name);
            Ok(Json(json!({"ok": true})).into_response())
        } else {
            Err(ApiError::not_found("no such user"))
        }
    })();
    done(res, info)
}
