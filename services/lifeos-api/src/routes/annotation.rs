//! `/api/annotation` - workspace-scoped notes/highlights/questions/comments/
//! links attached to entities (docs/DATA-MODEL.md §2.4). Generalizes the
//! knowledge-atlas's old localStorage note layer into the shared DB.
//!
//! Unlike `events`, annotations are mutable and normal CRUD applies: the
//! append-only rule does not cover this table. Each write still appends a
//! matching `annotation.*` event so the domain log stays complete.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_annotation, Annotation, COLS_ANNOTATION};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::json;

/// Serialize an optional JSON value into a text column, or `None` for an
/// absent/`null` value (kept out of the row rather than stored as "null").
fn opt_json_to_text(v: &serde_json::Value) -> Option<String> {
    if v.is_null() {
        None
    } else {
        serde_json::to_string(v).ok()
    }
}

#[derive(Deserialize)]
pub struct CreateAnnotation {
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    anchor: serde_json::Value,
    #[serde(default)]
    attrs: serde_json::Value,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateAnnotation>,
) -> ApiResult<Json<Annotation>> {
    let has_body = req.body.as_ref().is_some_and(|b| !b.trim().is_empty());
    // Reject empty shells: an annotation must carry text, an anchor, or a subject.
    if !has_body && req.anchor.is_null() && req.entity_id.is_none() {
        return Err(ApiError::BadRequest(
            "annotation needs a body, anchor, or entity_id".into(),
        ));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("ann");
    let now = now_secs();
    let kind = req.kind.filter(|k| !k.trim().is_empty()).unwrap_or_else(|| "note".into());
    let attrs_str = if req.attrs.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&req.attrs).unwrap_or_else(|_| "{}".into())
    };
    let created_by = req.created_by.unwrap_or_else(|| "user".into());

    state
        .conn
        .execute(
            "INSERT INTO annotations \
             (id, workspace_id, entity_id, kind, body, anchor, attrs, created_by, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                req.entity_id.clone(),
                kind.clone(),
                req.body,
                opt_json_to_text(&req.anchor),
                attrs_str,
                created_by,
                now,
                now
            ],
        )
        .await?;

    emit(
        &state.conn,
        &workspace_id,
        "annotation.created",
        Some(&id),
        "api",
        &json!({ "kind": kind, "entity_id": req.entity_id }),
    )
    .await?;
    fetch_one(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    entity_id: Option<String>,
    kind: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Annotation>>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = format!("SELECT {COLS_ANNOTATION} FROM annotations WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    let mut next = 2;
    for (col, val) in [("entity_id", &params.entity_id), ("kind", &params.kind)] {
        if let Some(v) = val {
            sql.push_str(&format!(" AND {col} = ?{next}"));
            binds.push(v.clone());
            next += 1;
        }
    }
    let limit = params.limit.unwrap_or(500).min(2000);
    let offset = params.offset.unwrap_or(0);
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit} OFFSET {offset}"));

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_annotation).await?))
}

#[derive(Deserialize)]
pub struct UpdateAnnotation {
    kind: Option<String>,
    body: Option<String>,
    entity_id: Option<String>,
    /// If present, replaces the whole anchor blob (row-level last-push-wins).
    anchor: Option<serde_json::Value>,
    /// If present, replaces the whole attrs blob.
    attrs: Option<serde_json::Value>,
    workspace_id: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateAnnotation>,
) -> ApiResult<Json<Annotation>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    // Ensure it exists in this tenant before mutating (404 otherwise).
    let _ = fetch_one(&state, &workspace_id, &id).await?;

    let anchor_str = req.anchor.as_ref().map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".into()));
    let attrs_str = req.attrs.as_ref().map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".into()));

    state
        .conn
        .execute(
            "UPDATE annotations SET \
               kind = COALESCE(?1, kind), \
               body = COALESCE(?2, body), \
               entity_id = COALESCE(?3, entity_id), \
               anchor = COALESCE(?4, anchor), \
               attrs = COALESCE(?5, attrs), \
               updated_at = ?6 \
             WHERE id = ?7 AND workspace_id = ?8",
            libsql::params![
                req.kind,
                req.body,
                req.entity_id,
                anchor_str,
                attrs_str,
                now_secs(),
                id.clone(),
                workspace_id.clone()
            ],
        )
        .await?;

    emit(&state.conn, &workspace_id, "annotation.updated", Some(&id), "api", &json!({})).await?;
    fetch_one(&state, &workspace_id, &id).await
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Annotation>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    // 404 if it isn't this tenant's row; also the value we return post-delete.
    let existing = fetch_one(&state, &workspace_id, &id).await?;

    state
        .conn
        .execute(
            "DELETE FROM annotations WHERE id = ?1 AND workspace_id = ?2",
            libsql::params![id.clone(), workspace_id.clone()],
        )
        .await?;

    emit(&state.conn, &workspace_id, "annotation.deleted", Some(&id), "api", &json!({})).await?;
    Ok(existing)
}

/// Fetch one annotation scoped to a workspace, or 404.
async fn fetch_one(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Annotation>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_ANNOTATION} FROM annotations WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_annotation(&row)?)),
        None => Err(ApiError::NotFound(format!("annotation '{id}' not found"))),
    }
}
