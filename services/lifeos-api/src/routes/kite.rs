//! Zerodha Kite Connect connect flow + read-only positions proxy (issue #51,
//! docs/SECURITY.md §1). The agent-facing surface is `GET /api/broker/positions`
//! only - there is no place/modify/cancel/GTT route anywhere in this file, on
//! this router, or in `kite::KiteClient`. Real orders flow through the
//! separate human-typed-confirmation executor, never this API.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::crypto;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::kite::{login_url, KiteClient};
use crate::models::{read_connection, Connection};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// A Kite access token is valid until Kite's daily invalidation (~6am IST the
/// next day). We don't track exact IST rollover here - a conservative 20h TTL
/// forces re-login well before the token could actually still be live, so a
/// stale-but-not-yet-expired row is never trusted past what Kite itself allows.
const KITE_TOKEN_TTL_SECS: i64 = 20 * 3600;

fn kite_or_501(state: &AppState) -> ApiResult<&dyn KiteClient> {
    state
        .kite
        .as_deref()
        .ok_or_else(|| ApiError::NotImplemented("Kite is not configured - see docs/MANUAL-SETUP.md #51".into()))
}

fn encryption_key_or_501(state: &AppState) -> ApiResult<&crypto::EncryptionKey> {
    state
        .config
        .secret_encryption_key
        .as_ref()
        .ok_or_else(|| ApiError::NotImplemented("LIFEOS_SECRET_ENCRYPTION_KEY is not set - see docs/MANUAL-SETUP.md #51".into()))
}

#[derive(Deserialize)]
pub struct LoginUrlParams {
    workspace_id: Option<String>,
}

/// `GET /api/connections/kite/login-url` - the URL the frontend redirects to
/// for the daily Kite login. No secret is exchanged yet.
pub async fn login_url_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<LoginUrlParams>,
) -> ApiResult<Json<Value>> {
    kite_or_501(&state)?;
    let api_key = state
        .config
        .kite_api_key
        .as_ref()
        .ok_or_else(|| ApiError::NotImplemented("KITE_API_KEY is not set - see docs/MANUAL-SETUP.md #51".into()))?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    Ok(Json(json!({ "login_url": login_url(api_key) })))
}

#[derive(Deserialize)]
pub struct CompleteKite {
    /// `request_token` from Kite's login redirect query string.
    request_token: String,
    workspace_id: Option<String>,
}

/// `POST /api/connections/kite/complete` - exchange the daily request_token
/// for an access_token, envelope-encrypt it, and store the connection. The
/// access_token is never returned in the response.
pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CompleteKite>,
) -> ApiResult<Json<Connection>> {
    if req.request_token.trim().is_empty() {
        return Err(ApiError::BadRequest("request_token is required".into()));
    }
    let kite = kite_or_501(&state)?;
    let key = encryption_key_or_501(&state)?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let session = kite.generate_session(&req.request_token).await?;
    let secret_enc = crypto::encrypt(&session.access_token, key)?;

    let id = new_id("conn");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO connections \
             (id, workspace_id, provider, account_handle, nango_connection_id, secret_enc, scopes, expires_at, status, created_at) \
             VALUES (?1, ?2, 'kite', ?3, NULL, ?4, 'read', ?5, 'active', ?6)",
            libsql::params![id.clone(), workspace_id.clone(), session.user_id, secret_enc, now + KITE_TOKEN_TTL_SECS, now],
        )
        .await?;

    emit(&state.conn, &workspace_id, "connection.connected", Some(&id), "api", &json!({ "provider": "kite" })).await?;

    fetch_connection(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct PositionsParams {
    workspace_id: Option<String>,
}

/// `GET /api/broker/positions` - read-only proxy to Kite's positions
/// endpoint. This is the ONLY market-data route the API exposes; no
/// place/modify/cancel/GTT route exists on this router (docs/SECURITY.md §1).
pub async fn positions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<PositionsParams>,
) -> ApiResult<Json<Value>> {
    let kite = kite_or_501(&state)?;
    let key = encryption_key_or_501(&state)?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut rows = state
        .conn
        .query(
            "SELECT id, secret_enc, expires_at FROM connections \
             WHERE workspace_id = ?1 AND provider = 'kite' AND status = 'active' \
             ORDER BY created_at DESC LIMIT 1",
            libsql::params![workspace_id.clone()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| ApiError::NotFound("no active kite connection for this workspace".into()))?;
    let secret_enc: Option<String> = row.get(1)?;
    let expires_at: Option<i64> = row.get(2)?;
    let secret_enc = secret_enc.ok_or_else(|| ApiError::Internal("kite connection missing secret_enc".into()))?;
    if expires_at.is_some_and(|exp| now_secs() >= exp) {
        return Err(ApiError::BadRequest("kite access token has expired - complete the daily login again".into()));
    }

    let access_token = crypto::decrypt(&secret_enc, key)?;
    kite.positions(&access_token).await.map(Json)
}

async fn fetch_connection(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Connection>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {} FROM connections WHERE id = ?1 AND workspace_id = ?2", crate::models::COLS_CONNECTION),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_connection(&row)?)),
        None => Err(ApiError::NotFound(format!("connection '{id}' not found"))),
    }
}
