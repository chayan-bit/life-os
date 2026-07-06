//! Real login/session (issue #100, docs/SECURITY.md §5): `POST /api/login`
//! verifies a password against `users.password_hash` and issues an access
//! token (unchanged JWT shape, still what `resolve_workspace` verifies)
//! plus a rotating refresh token backed by the new `sessions` table.
//! `POST /api/session/refresh` rotates (old session revoked, new one
//! issued) rather than just re-signing, so a stolen refresh token has a
//! bounded lifetime even if never explicitly revoked.
//! `POST /api/logout` revokes a single session.
//!
//! `POST /api/account/set-password` is a narrow bootstrap: it only ever
//! succeeds for a user whose `password_hash` is currently NULL (the
//! personal account seeded by `db.rs::seed()` before issue #100, or any
//! other pre-#100 row) - once a password is set, this route can never be
//! used to overwrite it, so it cannot be used to take over an already-
//! secured account.

use crate::auth::{hash_password, hash_refresh_token, issue_token, new_refresh_token, verify_password, REFRESH_TOKEN_TTL_SECS};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> ApiResult<Json<Value>> {
    let email = req.email.trim();
    let user = find_user_by_email(&state, email)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid email or password".into()))?;

    let stored_hash = user.password_hash.as_deref().ok_or_else(|| {
        ApiError::BadRequest(
            "this account has no password set yet - use POST /api/account/set-password once".into(),
        )
    })?;
    if !verify_password(&req.password, stored_hash) {
        return Err(ApiError::BadRequest("invalid email or password".into()));
    }

    let workspace_id = primary_workspace(&state, &user.id).await?;
    let key_token = issue_token(&state.config.jwt_secret, &user.id, &workspace_id, email);
    let refresh_token = create_session(&state, &user.id, &workspace_id).await?;

    Ok(Json(json!({
        "user_id": user.id,
        "workspace_id": workspace_id,
        "key_token": key_token,
        "refresh_token": refresh_token,
    })))
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    refresh_token: String,
}

/// Rotates a valid, unexpired, unrevoked refresh token: revokes it and
/// issues a fresh access token + fresh refresh token backed by a new
/// session row. A reused (already-revoked) or expired token is rejected -
/// this is what bounds a leaked refresh token's blast radius.
pub async fn refresh(State(state): State<AppState>, Json(req): Json<RefreshRequest>) -> ApiResult<Json<Value>> {
    let hash = hash_refresh_token(&req.refresh_token);
    let now = now_secs();

    let mut rows = state
        .conn
        .query(
            "SELECT id, user_id, workspace_id FROM sessions \
             WHERE refresh_token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2",
            libsql::params![hash, now],
        )
        .await?;
    let (session_id, user_id, workspace_id): (String, String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?, row.get(2)?),
        None => return Err(ApiError::BadRequest("invalid, expired, or already-used refresh token".into())),
    };

    state
        .conn
        .execute(
            "UPDATE sessions SET revoked_at = ?2 WHERE id = ?1",
            libsql::params![session_id, now],
        )
        .await?;

    let email = user_email(&state, &user_id).await?;
    let key_token = issue_token(&state.config.jwt_secret, &user_id, &workspace_id, &email);
    let new_refresh = create_session(&state, &user_id, &workspace_id).await?;

    Ok(Json(json!({
        "key_token": key_token,
        "refresh_token": new_refresh,
        "workspace_id": workspace_id,
    })))
}

#[derive(Deserialize)]
pub struct LogoutRequest {
    refresh_token: String,
}

/// Revokes one session. Idempotent - revoking an already-revoked or
/// unknown token still returns success (nothing to leak by distinguishing).
pub async fn logout(State(state): State<AppState>, Json(req): Json<LogoutRequest>) -> ApiResult<Json<Value>> {
    let hash = hash_refresh_token(&req.refresh_token);
    state
        .conn
        .execute(
            "UPDATE sessions SET revoked_at = ?2 WHERE refresh_token_hash = ?1 AND revoked_at IS NULL",
            libsql::params![hash, now_secs()],
        )
        .await?;
    Ok(Json(json!({ "status": "logged_out" })))
}

#[derive(Deserialize)]
pub struct SetPasswordRequest {
    email: String,
    password: String,
}

/// One-time bootstrap for a pre-#100 passwordless account (see module docs).
pub async fn set_password(
    State(state): State<AppState>,
    Json(req): Json<SetPasswordRequest>,
) -> ApiResult<Json<Value>> {
    let email = req.email.trim();
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest("password must be at least 8 characters".into()));
    }
    let user = find_user_by_email(&state, email)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no account for '{email}'")))?;

    let password_hash =
        hash_password(&req.password).map_err(|e| ApiError::Internal(format!("password hashing failed: {e}")))?;
    // Atomic guard: the `password_hash IS NULL` predicate is checked and
    // written in the same statement, so a concurrent second request can
    // never both pass a pre-check and then overwrite an already-set
    // password (the read-then-write race this route used to have).
    let rows_changed = state
        .conn
        .execute(
            "UPDATE users SET password_hash = ?2, updated_at = ?3 WHERE id = ?1 AND password_hash IS NULL",
            libsql::params![user.id.clone(), password_hash, now_secs()],
        )
        .await?;
    if rows_changed == 0 {
        return Err(ApiError::BadRequest(
            "this account already has a password - use POST /api/login".into(),
        ));
    }

    Ok(Json(json!({ "status": "password_set" })))
}

/// Creates a session row and returns the plaintext refresh token (only ever
/// returned here - the DB only ever stores its hash).
pub async fn create_session(state: &AppState, user_id: &str, workspace_id: &str) -> ApiResult<String> {
    let token = new_refresh_token();
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO sessions (id, user_id, workspace_id, refresh_token_hash, created_at, expires_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            libsql::params![
                new_id("sess"),
                user_id,
                workspace_id,
                hash_refresh_token(&token),
                now,
                now + REFRESH_TOKEN_TTL_SECS
            ],
        )
        .await?;
    Ok(token)
}

struct FoundUser {
    id: String,
    password_hash: Option<String>,
}

async fn find_user_by_email(state: &AppState, email: &str) -> ApiResult<Option<FoundUser>> {
    let mut rows = state
        .conn
        .query(
            "SELECT id, password_hash FROM users WHERE email = ?1",
            libsql::params![email],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(FoundUser { id: row.get(0)?, password_hash: row.get(1)? }),
        None => None,
    })
}

async fn user_email(state: &AppState, user_id: &str) -> ApiResult<String> {
    let mut rows = state
        .conn
        .query("SELECT email FROM users WHERE id = ?1", libsql::params![user_id])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err(ApiError::Internal(format!("user '{user_id}' vanished"))),
    }
}

/// The membership created earliest is treated as a user's "primary" workspace
/// for login (mirrors the old `register.rs::lookup_existing` ordering).
async fn primary_workspace(state: &AppState, user_id: &str) -> ApiResult<String> {
    let mut rows = state
        .conn
        .query(
            "SELECT workspace_id FROM memberships WHERE user_id = ?1 ORDER BY created_at ASC LIMIT 1",
            libsql::params![user_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err(ApiError::Internal(format!("user '{user_id}' has no workspace membership"))),
    }
}
