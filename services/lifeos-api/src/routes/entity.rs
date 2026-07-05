//! `/api/entity` - the generic entity CRUD the whole system rests on.
//! Every operation is workspace-scoped (RLS-style) at this layer.

use crate::audit::emit;
use crate::db::{index_entity, workspace_exists};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_entity, Entity, COLS_ENTITY};
use crate::auth::resolve_workspace;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct CreateEntity {
    module: String,
    r#type: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    attrs: serde_json::Value,
    /// Optional explicit tenant; otherwise resolved from auth/headers/default.
    #[serde(default)]
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateEntity>,
) -> ApiResult<Json<Entity>> {
    if req.module.trim().is_empty() || req.r#type.trim().is_empty() {
        return Err(ApiError::BadRequest("module and type are required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("ent");
    let now = now_secs();
    let attrs_str = if req.attrs.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&req.attrs).unwrap_or_else(|_| "{}".into())
    };

    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                req.module,
                req.r#type,
                req.parent_id,
                req.title,
                req.status,
                req.tier,
                attrs_str,
                req.source.unwrap_or_else(|| "api".into()),
                now,
                now
            ],
        )
        .await?;

    // attrs is snapshotted on the event so a forced sync conflict can be
    // repaired by replaying events in causal (id/ULID) order - see reconcile.rs.
    emit(
        &state.conn,
        &workspace_id,
        "entity.created",
        Some(&id),
        "api",
        &json!({ "attrs": req.attrs }),
    )
    .await?;
    // Keep the lexical search index live (best-effort; boot rebuild reconciles).
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }
    fetch_one(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    module: Option<String>,
    r#type: Option<String>,
    status: Option<String>,
    parent_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Entity>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    // Build a positional-parameter query (?1, ?2, ...) - never string-interpolate values.
    let mut sql = format!("SELECT {COLS_ENTITY} FROM entities WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    let mut next = 2;
    for (col, val) in [
        ("module", &params.module),
        ("type", &params.r#type),
        ("status", &params.status),
        ("parent_id", &params.parent_id),
    ] {
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
    Ok(Json(collect(rows, read_entity).await?))
}

pub async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Entity>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    fetch_one(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct UpdateEntity {
    title: Option<String>,
    status: Option<String>,
    tier: Option<String>,
    parent_id: Option<String>,
    /// If present, replaces the whole attrs blob (row-level last-push-wins).
    attrs: Option<serde_json::Value>,
    workspace_id: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateEntity>,
) -> ApiResult<Json<Entity>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    // Ensure it exists in this tenant before mutating.
    let _ = fetch_one(&state, &workspace_id, &id).await?;

    // COALESCE keeps existing values for fields the caller omitted.
    let attrs_str = req
        .attrs
        .as_ref()
        .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".into()));

    state
        .conn
        .execute(
            "UPDATE entities SET \
               title = COALESCE(?1, title), \
               status = COALESCE(?2, status), \
               tier = COALESCE(?3, tier), \
               parent_id = COALESCE(?4, parent_id), \
               attrs = COALESCE(?5, attrs), \
               updated_at = ?6 \
             WHERE id = ?7 AND workspace_id = ?8",
            libsql::params![
                req.title,
                req.status,
                req.tier,
                req.parent_id,
                attrs_str,
                now_secs(),
                id.clone(),
                workspace_id.clone()
            ],
        )
        .await?;

    // Snapshot the attrs blob that was actually applied (or {} if this update
    // didn't touch attrs) so reconcile.rs can replay the full causal history.
    emit(
        &state.conn,
        &workspace_id,
        "entity.updated",
        Some(&id),
        "api",
        &json!({ "attrs": req.attrs }),
    )
    .await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }
    fetch_one(&state, &workspace_id, &id).await
}

/// Repair `attrs` from a forced sync conflict by replaying the entity's
/// `entity.created`/`entity.updated` events in causal order - see
/// `crate::reconcile`. No-op if the event log has nothing to replay.
pub async fn reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Entity>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    // Ensure it exists in this tenant before mutating.
    let _ = fetch_one(&state, &workspace_id, &id).await?;
    crate::reconcile::reconcile_entity(&state.conn, &workspace_id, &id).await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }
    fetch_one(&state, &workspace_id, &id).await
}

/// Fetch one entity scoped to a workspace, or 404.
async fn fetch_one(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Entity>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_entity(&row)?)),
        None => Err(ApiError::NotFound(format!("entity '{id}' not found"))),
    }
}
