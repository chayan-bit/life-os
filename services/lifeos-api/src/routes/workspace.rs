//! `/api/workspace` - the resolved workspace's own row (name/plan), and
//! `/api/me` - the authenticated user from the bearer token's claims, if any.
//! Backs Profile.jsx's switch from localStorage-only edits to the real
//! control-plane data (issue #38).

use crate::auth::{bearer_claims, resolve_workspace};
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::state::AppState;
use axum::{
    extract::State,
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn get_workspace(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, None)?;
    let mut rows = state
        .conn
        .query(
            "SELECT id, name, plan, created_at, updated_at FROM workspaces WHERE id = ?1",
            libsql::params![ws.clone()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("workspace '{ws}' not found")))?;
    Ok(Json(json!({
        "id": row.get::<String>(0)?,
        "name": row.get::<String>(1)?,
        "plan": row.get::<String>(2)?,
        "created_at": row.get::<i64>(3)?,
        "updated_at": row.get::<i64>(4)?,
    })))
}

#[derive(Deserialize)]
pub struct UpdateWorkspace {
    name: Option<String>,
    plan: Option<String>,
}

pub async fn update_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateWorkspace>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, None)?;
    if req.name.is_none() && req.plan.is_none() {
        return Err(ApiError::BadRequest("at least one of name, plan is required".into()));
    }
    if let Some(name) = &req.name {
        if name.trim().is_empty() {
            return Err(ApiError::BadRequest("name cannot be empty".into()));
        }
        state
            .conn
            .execute(
                "UPDATE workspaces SET name = ?1, updated_at = ?2 WHERE id = ?3",
                libsql::params![name.clone(), now_secs(), ws.clone()],
            )
            .await?;
    }
    if let Some(plan) = &req.plan {
        state
            .conn
            .execute(
                "UPDATE workspaces SET plan = ?1, updated_at = ?2 WHERE id = ?3",
                libsql::params![plan.clone(), now_secs(), ws.clone()],
            )
            .await?;
    }
    get_workspace(State(state), headers).await
}

pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let claims = match bearer_claims(&headers, &state.config.jwt_secret) {
        Some(c) => c,
        // Soft-auth: no token presented (or invalid) is not an error - the
        // frontend's demo identity has no backend-minted token.
        None => return Ok(Json(json!({ "authenticated": false }))),
    };

    let mut rows = state
        .conn
        .query(
            "SELECT id, email, name FROM users WHERE id = ?1",
            libsql::params![claims.sub.clone()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(json!({
            "authenticated": true,
            "id": row.get::<String>(0)?,
            "email": row.get::<String>(1)?,
            "name": row.get::<Option<String>>(2)?,
            "workspace_id": claims.workspace_id,
        }))),
        None => Ok(Json(json!({ "authenticated": false }))),
    }
}
