//! Release-loop candidate configs (`/api/configs`, issue #98,
//! docs/HARNESS-LOOP.md §4). A candidate moves `draft -> shadow ->
//! promoted|rejected`. The active pointer per `kind` is a `vcs_refs` row
//! (`kind='config_active', name=<configs.kind>, snapshot_ref=<configs.id>`),
//! the same named-pointer/atomic-flip shape `lifeos-vcs` branches/tags
//! already use (issue #84), reused here rather than a second pointer
//! table. `promote`/`rollback` are the only writes that flip the active
//! pointer, and both emit an `events` row - nothing here auto-activates;
//! the caller (`harness config promote`, human-typed only) is what
//! decides to call this route at all.

use crate::auth::resolve_workspace;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateConfig {
    kind: String,
    payload: Value,
    workspace_id: Option<String>,
}

/// Creates a draft candidate config. Nothing is activated by this call.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateConfig>,
) -> ApiResult<Json<Value>> {
    if req.kind.trim().is_empty() {
        return Err(ApiError::BadRequest("kind is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let id = new_id("cfg");
    let now = now_secs();
    let payload_str = serde_json::to_string(&req.payload).unwrap_or_else(|_| "{}".into());

    state
        .conn
        .execute(
            "INSERT INTO configs (id, workspace_id, kind, payload, status, shadow_summary, created_at, promoted_at) \
             VALUES (?1, ?2, ?3, ?4, 'draft', NULL, ?5, NULL)",
            libsql::params![id.clone(), workspace_id, req.kind, payload_str, now],
        )
        .await?;

    row_json(&state, &id).await
}

#[derive(Deserialize)]
pub struct ShadowBody {
    shadow_summary: Value,
}

/// Attaches a shadow-replay summary (computed by the caller against
/// `route.jsonl` - replay logic lives in one place, not duplicated into
/// Rust) and moves the candidate to `status='shadow'`.
pub async fn shadow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ShadowBody>,
) -> ApiResult<Json<Value>> {
    require_status(&state, &id, "draft").await?;
    let summary_str = serde_json::to_string(&req.shadow_summary).unwrap_or_else(|_| "{}".into());
    state
        .conn
        .execute(
            "UPDATE configs SET status = 'shadow', shadow_summary = ?2 WHERE id = ?1",
            libsql::params![id.clone(), summary_str],
        )
        .await?;
    row_json(&state, &id).await
}

/// Human-gated: flips the active pointer for this config's `kind` to this
/// id, marks it `promoted`, and emits `events(type='config.promoted')`.
/// Never called by an agent/hook/cron - only `harness config promote`
/// (human-typed CLI) calls this route.
pub async fn promote(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let cfg = load_config(&state, &id).await?;
    let workspace_id = cfg["workspace_id"].as_str().unwrap_or_default().to_string();
    let kind = cfg["kind"].as_str().unwrap_or_default().to_string();
    let now = now_secs();

    let previous_ref = active_ref(&state, &workspace_id, &kind).await?;

    state
        .conn
        .execute(
            "UPDATE configs SET status = 'promoted', promoted_at = ?2 WHERE id = ?1",
            libsql::params![id.clone(), now],
        )
        .await?;
    upsert_active_ref(&state, &workspace_id, &kind, &id, now).await?;
    emit_event(&state, &workspace_id, "config.promoted", &id, &kind, previous_ref.as_deref()).await?;

    row_json(&state, &id).await
}

#[derive(Deserialize)]
pub struct RollbackBody {
    kind: String,
    workspace_id: Option<String>,
}

/// Human-gated: flips the active pointer for `kind` back to the
/// previously-promoted config (the 2nd-most-recent `status='promoted'`
/// row by `promoted_at`), and emits `events(type='config.rolledback')`.
pub async fn rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RollbackBody>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let current_ref = active_ref(&state, &workspace_id, &req.kind).await?;

    let mut rows = state
        .conn
        .query(
            // promoted_at has second granularity, so ties within the same
            // second need a deterministic tiebreaker - id is a ULID
            // (lexically sortable by creation time), so ORDER BY id DESC
            // resolves ties in the same chronological order.
            "SELECT id FROM configs WHERE workspace_id = ?1 AND kind = ?2 AND status = 'promoted' \
             ORDER BY promoted_at DESC, id DESC LIMIT 1 OFFSET 1",
            libsql::params![workspace_id.clone(), req.kind.clone()],
        )
        .await?;
    let target_id: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Err(ApiError::BadRequest(format!("no prior promoted config to roll back to for kind '{}'", req.kind))),
    };

    let now = now_secs();
    upsert_active_ref(&state, &workspace_id, &req.kind, &target_id, now).await?;
    emit_event(&state, &workspace_id, "config.rolledback", &target_id, &req.kind, current_ref.as_deref()).await?;

    row_json(&state, &target_id).await
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
}

/// Lists candidates, newest first, plus each distinct kind's active id.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = "SELECT id, workspace_id, kind, payload, status, shadow_summary, created_at, promoted_at \
                    FROM configs WHERE workspace_id = ?1"
        .to_string();
    let mut binds: Vec<String> = vec![workspace_id.clone()];
    let mut next = 2;
    for (col, val) in [("kind", &params.kind), ("status", &params.status)] {
        if let Some(v) = val {
            sql.push_str(&format!(" AND {col} = ?{next}"));
            binds.push(v.clone());
            next += 1;
        }
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    let mut configs = Vec::new();
    while let Some(row) = rows.next().await? {
        configs.push(row_to_json(&row)?);
    }

    let mut active_rows = state
        .conn
        .query(
            "SELECT name, snapshot_ref FROM vcs_refs WHERE workspace_id = ?1 AND kind = 'config_active'",
            libsql::params![workspace_id],
        )
        .await?;
    let mut active = serde_json::Map::new();
    while let Some(row) = active_rows.next().await? {
        let name: String = row.get(0)?;
        let snapshot_ref: String = row.get(1)?;
        active.insert(name, json!(snapshot_ref));
    }

    Ok(Json(json!({ "configs": configs, "active": active })))
}

async fn require_status(state: &AppState, id: &str, expected: &str) -> ApiResult<()> {
    let cfg = load_config(state, id).await?;
    if cfg["status"].as_str() != Some(expected) {
        return Err(ApiError::BadRequest(format!(
            "config '{id}' must be in status '{expected}' (is '{}')",
            cfg["status"]
        )));
    }
    Ok(())
}

async fn load_config(state: &AppState, id: &str) -> ApiResult<Value> {
    let mut rows = state
        .conn
        .query(
            "SELECT id, workspace_id, kind, payload, status, shadow_summary, created_at, promoted_at \
             FROM configs WHERE id = ?1",
            libsql::params![id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => row_to_json(&row),
        None => Err(ApiError::NotFound(format!("config '{id}' not found"))),
    }
}

async fn row_json(state: &AppState, id: &str) -> ApiResult<Json<Value>> {
    Ok(Json(load_config(state, id).await?))
}

fn row_to_json(row: &libsql::Row) -> ApiResult<Value> {
    let id: String = row.get(0)?;
    let workspace_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let payload_str: String = row.get(3)?;
    let status: String = row.get(4)?;
    let shadow_summary_str: Option<String> = row.get(5)?;
    let created_at: i64 = row.get(6)?;
    let promoted_at: Option<i64> = row.get(7)?;

    let payload: Value = serde_json::from_str(&payload_str).unwrap_or(json!({}));
    let shadow_summary: Value = shadow_summary_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);

    Ok(json!({
        "id": id,
        "workspace_id": workspace_id,
        "kind": kind,
        "payload": payload,
        "status": status,
        "shadow_summary": shadow_summary,
        "created_at": created_at,
        "promoted_at": promoted_at,
    }))
}

async fn active_ref(state: &AppState, workspace_id: &str, kind: &str) -> ApiResult<Option<String>> {
    let mut rows = state
        .conn
        .query(
            "SELECT snapshot_ref FROM vcs_refs WHERE workspace_id = ?1 AND kind = 'config_active' AND name = ?2",
            libsql::params![workspace_id, kind],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

async fn upsert_active_ref(
    state: &AppState,
    workspace_id: &str,
    kind: &str,
    config_id: &str,
    now: i64,
) -> ApiResult<()> {
    state
        .conn
        .execute(
            "INSERT INTO vcs_refs (workspace_id, kind, name, snapshot_ref, updated_at) \
             VALUES (?1, 'config_active', ?2, ?3, ?4) \
             ON CONFLICT (workspace_id, kind, name) DO UPDATE SET snapshot_ref = excluded.snapshot_ref, updated_at = excluded.updated_at",
            libsql::params![workspace_id, kind, config_id, now],
        )
        .await?;
    Ok(())
}

async fn emit_event(
    state: &AppState,
    workspace_id: &str,
    event_type: &str,
    config_id: &str,
    kind: &str,
    previous_ref: Option<&str>,
) -> ApiResult<()> {
    let attrs = json!({ "config_id": config_id, "kind": kind, "previous_ref": previous_ref }).to_string();
    state
        .conn
        .execute(
            "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'api', ?6)",
            libsql::params![
                new_id("evt"),
                workspace_id,
                now_secs(),
                event_type,
                config_id,
                attrs
            ],
        )
        .await?;
    Ok(())
}
