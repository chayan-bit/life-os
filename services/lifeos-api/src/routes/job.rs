//! `/api/jobs` (read) + `/api/job` (enqueue). The job queue `lifeos-drain`
//! claims from. Enqueue is shared by the planned ingest/pipeline routes.

use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_job, Job, COLS_JOB};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;

/// Insert a queued job and return its id. Reused by planned routes.
pub async fn enqueue(
    state: &AppState,
    workspace_id: &str,
    kind: &str,
    payload: &serde_json::Value,
    priority: i64,
) -> ApiResult<String> {
    let id = new_id("job");
    let payload_str = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    state
        .conn
        .execute(
            "INSERT INTO jobs (id, workspace_id, kind, payload, status, priority, attempts, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, 0, ?6)",
            libsql::params![id.clone(), workspace_id, kind, payload_str, priority, now_secs()],
        )
        .await?;
    Ok(id)
}

#[derive(Deserialize)]
pub struct CreateJob {
    kind: String,
    #[serde(default)]
    payload: serde_json::Value,
    priority: Option<i64>,
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateJob>,
) -> ApiResult<Json<Job>> {
    if req.kind.trim().is_empty() {
        return Err(ApiError::BadRequest("kind is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = enqueue(&state, &workspace_id, &req.kind, &req.payload, req.priority.unwrap_or(0)).await?;
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_JOB} FROM jobs WHERE id = ?1"),
            libsql::params![id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| ApiError::Internal("job vanished".into()))?;
    Ok(Json(read_job(&row)?))
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    limit: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Job>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = format!("SELECT {COLS_JOB} FROM jobs WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    let mut next = 2;
    for (col, val) in [("status", &params.status), ("kind", &params.kind)] {
        if let Some(v) = val {
            sql.push_str(&format!(" AND {col} = ?{next}"));
            binds.push(v.clone());
            next += 1;
        }
    }
    let limit = params.limit.unwrap_or(200).min(2000);
    sql.push_str(&format!(" ORDER BY priority DESC, created_at DESC LIMIT {limit}"));

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_job).await?))
}
