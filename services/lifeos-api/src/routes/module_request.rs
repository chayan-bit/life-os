//! `POST /api/module-request` - self-extension intake. Records the request,
//! appends a `module.requested` event, and enqueues a `module_build` job for the
//! Mac harness to drain. The cloud surface may only enqueue - it never builds.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{read_module_request, ModuleRequest, COLS_MODULE_REQUEST};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateModuleRequest {
    prompt: String,
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateModuleRequest>,
) -> ApiResult<Json<Value>> {
    if req.prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("req");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO module_requests (id, workspace_id, prompt, status, error, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 'queued', NULL, ?4, ?5)",
            libsql::params![id.clone(), workspace_id.clone(), req.prompt.clone(), now, now],
        )
        .await?;

    emit(
        &state.conn,
        &workspace_id,
        "module.requested",
        Some(&id),
        "api",
        &json!({ "prompt": req.prompt }),
    )
    .await?;

    let job_id = super::job::enqueue(
        &state,
        &workspace_id,
        "module_build",
        &json!({ "request_id": id, "prompt": req.prompt }),
        0,
    )
    .await?;

    tracing::info!(request_id = %id, %job_id, "module request queued");
    Ok(Json(json!({
        "id": id,
        "request_id": id,
        "job_id": job_id,
        "status": "queued",
    })))
}

/// `GET /api/module-request/:id` - lets the requester poll the
/// queued->building->installed|failed lifecycle (issue #76), including the
/// honest failure message when the build didn't make it.
pub async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<ModuleRequest>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_MODULE_REQUEST} FROM module_requests WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id.clone(), workspace_id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("module_request '{id}' not found")))?;
    Ok(Json(read_module_request(&row)?))
}
