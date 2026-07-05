//! `/api/connections` - the owned-credential connect/disconnect surface
//! (issue #47, docs/INTEGRATIONS.md). The agent-facing handle is always a
//! `connectionId`; raw provider tokens never pass through this API.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_connection, Connection, COLS_CONNECTION};
use crate::nango::EndUser;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::json;

fn nango_or_501(state: &AppState) -> ApiResult<&dyn crate::nango::NangoClient> {
    state
        .nango
        .as_deref()
        .ok_or_else(|| ApiError::NotImplemented("Nango is not configured - see docs/MANUAL-SETUP.md".into()))
}

#[derive(Deserialize)]
pub struct StartSession {
    /// Nango `provider_config_key`, e.g. "github", "google".
    provider: String,
    workspace_id: Option<String>,
}

/// `POST /api/connections/session` - mint a Nango Connect session token the
/// frontend hands to Nango's Connect UI to run the OAuth dance. No token of
/// any kind is stored yet; that happens in `complete` once the flow finishes.
pub async fn start_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartSession>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.provider.trim().is_empty() {
        return Err(ApiError::BadRequest("provider is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let nango = nango_or_501(&state)?;
    let session = nango
        .create_connect_session(
            EndUser { id: workspace_id.clone(), email: None },
            vec![req.provider.clone()],
        )
        .await?;

    Ok(Json(json!({ "session_token": session.token, "provider": req.provider })))
}

#[derive(Deserialize)]
pub struct CompleteConnection {
    /// The `connectionId` Nango's Connect UI returns after a successful OAuth flow.
    connection_id: String,
    provider: String,
    #[serde(default)]
    account_handle: Option<String>,
    workspace_id: Option<String>,
}

/// `POST /api/connections/complete` - verify the connection with Nango and
/// record the `connectionId` handle. Never persists a token - only Nango's
/// vault ever holds one.
pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CompleteConnection>,
) -> ApiResult<Json<Connection>> {
    if req.connection_id.trim().is_empty() || req.provider.trim().is_empty() {
        return Err(ApiError::BadRequest("connection_id and provider are required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let nango = nango_or_501(&state)?;
    // Verify with Nango rather than trusting the client's claim outright.
    let verified = nango.get_connection(&req.connection_id, &req.provider).await?;

    let id = new_id("conn");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO connections \
             (id, workspace_id, provider, account_handle, nango_connection_id, secret_enc, scopes, expires_at, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, 'active', ?6)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                verified.provider_config_key.clone(),
                req.account_handle,
                verified.connection_id.clone(),
                now
            ],
        )
        .await?;

    emit(
        &state.conn,
        &workspace_id,
        "connection.connected",
        Some(&id),
        "api",
        &json!({ "provider": verified.provider_config_key }),
    )
    .await?;

    fetch_one(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    provider: Option<String>,
}

/// `GET /api/connections` - metadata only (provider, handle, status). Never
/// returns a token.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Connection>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = format!("SELECT {COLS_CONNECTION} FROM connections WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    if let Some(p) = &params.provider {
        sql.push_str(" AND provider = ?2");
        binds.push(p.clone());
    }
    sql.push_str(" ORDER BY created_at DESC");

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_connection).await?))
}

/// `DELETE /api/connections/:id` - revoke with Nango and mark the row
/// `revoked` (soft - kept for audit, matching the append-only events model).
pub async fn disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Connection>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let existing = fetch_one(&state, &workspace_id, &id).await?.0;

    if let Some(nango_id) = &existing.nango_connection_id {
        let nango = nango_or_501(&state)?;
        nango.delete_connection(nango_id, &existing.provider).await?;
    }

    state
        .conn
        .execute(
            "UPDATE connections SET status = 'revoked' WHERE id = ?1 AND workspace_id = ?2",
            libsql::params![id.clone(), workspace_id.clone()],
        )
        .await?;

    emit(
        &state.conn,
        &workspace_id,
        "connection.disconnected",
        Some(&id),
        "api",
        &json!({ "provider": existing.provider }),
    )
    .await?;

    fetch_one(&state, &workspace_id, &id).await
}

async fn fetch_one(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Connection>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_CONNECTION} FROM connections WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_connection(&row)?)),
        None => Err(ApiError::NotFound(format!("connection '{id}' not found"))),
    }
}
