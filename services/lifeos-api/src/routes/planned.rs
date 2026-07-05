//! Planned routes the frontend declares but whose services aren't built in the
//! base. Honest behavior, never a silent mock:
//!   - ingest / pipeline.run  -> enqueue a real job, return 202 + job_id
//!
//! As `lifeos-ingest` / `lifeos-pipelines` come online in later phases, these
//! enqueue paths already feed them via the job queue. `vcs.*` used to route
//! here as a 501 stub; it's now real (`routes/vcs.rs`, issue #86).

use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct IngestRequest {
    /// The file/asset entity to ingest (issue #88). Optional for back-compat
    /// with a bare uri/kind enqueue, but required for `lifeos-ingest` to
    /// actually process the job - see `lifeos_ingest::process_ingest_job`.
    entity_id: Option<String>,
    uri: Option<String>,
    kind: Option<String>,
    blob_ref: Option<String>,
    workspace_id: Option<String>,
}

pub async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let payload = json!({ "entity_id": req.entity_id, "uri": req.uri, "kind": req.kind, "blob_ref": req.blob_ref });
    let job_id = super::job::enqueue(&state, &workspace_id, "ingest", &payload, 0).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "queued", "job_id": job_id }))))
}

#[derive(Deserialize)]
pub struct PipelineRequest {
    pipeline: String,
    #[serde(default)]
    input: Value,
    workspace_id: Option<String>,
}

pub async fn pipeline_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PipelineRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if req.pipeline.trim().is_empty() {
        return Err(ApiError::BadRequest("pipeline is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let payload = json!({ "pipeline": req.pipeline, "input": req.input });
    let job_id = super::job::enqueue(&state, &workspace_id, "pipeline", &payload, 0).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "queued", "job_id": job_id }))))
}
