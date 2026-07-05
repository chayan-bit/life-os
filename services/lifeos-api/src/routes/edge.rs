//! `/api/edge` - graph relations between entities (and to external refs).

use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_edge, Edge, COLS_EDGE};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateEdge {
    src_id: String,
    rel: String,
    dst_id: Option<String>,
    dst_ref: Option<String>,
    state: Option<String>,
    created_by: Option<String>,
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateEdge>,
) -> ApiResult<Json<Edge>> {
    if req.src_id.trim().is_empty() || req.rel.trim().is_empty() {
        return Err(ApiError::BadRequest("src_id and rel are required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("edg");
    state
        .conn
        .execute(
            "INSERT INTO edges (id, workspace_id, src_id, dst_id, dst_ref, rel, state, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                req.src_id,
                req.dst_id,
                req.dst_ref,
                req.rel,
                req.state.unwrap_or_else(|| "accepted".into()),
                req.created_by.unwrap_or_else(|| "api".into()),
                now_secs()
            ],
        )
        .await?;

    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_EDGE} FROM edges WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| ApiError::Internal("edge vanished".into()))?;
    Ok(Json(read_edge(&row)?))
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    src_id: Option<String>,
    dst_id: Option<String>,
    rel: Option<String>,
    state: Option<String>,
    limit: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Edge>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = format!("SELECT {COLS_EDGE} FROM edges WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    let mut next = 2;
    for (col, val) in [
        ("src_id", &params.src_id),
        ("dst_id", &params.dst_id),
        ("rel", &params.rel),
        ("state", &params.state),
    ] {
        if let Some(v) = val {
            sql.push_str(&format!(" AND {col} = ?{next}"));
            binds.push(v.clone());
            next += 1;
        }
    }
    let limit = params.limit.unwrap_or(500).min(2000);
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit}"));

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_edge).await?))
}

#[derive(Deserialize)]
pub struct UpdateEdge {
    /// The only mutable field: the edge's lifecycle state ('pending' -> 'accepted').
    /// `edges` is otherwise immutable; relations are created or dropped, not rewritten.
    state: String,
    workspace_id: Option<String>,
}

/// Transition an edge's `state` (the pending/accepted lifecycle). Workspace-scoped.
pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateEdge>,
) -> ApiResult<Json<Edge>> {
    if req.state.trim().is_empty() {
        return Err(ApiError::BadRequest("state is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let changed = state
        .conn
        .execute(
            "UPDATE edges SET state = ?1 WHERE id = ?2 AND workspace_id = ?3",
            libsql::params![req.state, id.clone(), workspace_id.clone()],
        )
        .await?;
    if changed == 0 {
        return Err(ApiError::NotFound(format!("edge '{id}' not found")));
    }

    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_EDGE} FROM edges WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| ApiError::Internal("edge vanished".into()))?;
    Ok(Json(read_edge(&row)?))
}
