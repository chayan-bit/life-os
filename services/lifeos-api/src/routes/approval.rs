//! `/api/approvals` + `/api/approval/:id/{approve,deny}` (issue #142).
//!
//! The human-JWT surface for the gating state machine every outward or
//! irreversible action lands in (docs/SECURITY.md §2): a draft, a T3+ build
//! gate, or a storage-backend switch all sit as a `pending_approval` /
//! `awaiting_approval` entity until a human resolves it here. Mirrors the
//! Worker's `approvals.ts` semantics exactly - CAS status transition, one
//! `<type>.approved`/`.rejected` event, and (on approve) an `execute_approval`
//! job for `lifeos-drain` to actually run. The API itself never performs the
//! outward effect; it only records the decision and enqueues the work.
//!
//! Two safety properties are load-bearing and tested:
//!   1. The transition is a CAS (`UPDATE ... WHERE status IN (pending states)`),
//!      so a double-approve (two taps racing) resolves once and the loser gets
//!      a 409, never a duplicate event/job.
//!   2. A gate carrying `attrs.requires_typed_confirm` (T5 subsystems) refuses a
//!      bare approve: the body must carry `{ "typed": "<node/entity name>" }`
//!      matching exactly, server-enforced - a button can never bypass it.

use crate::audit::emit;
use crate::error::{ApiError, ApiResult};
use crate::models::{collect, read_entity, Entity, COLS_ENTITY};
use super::job::enqueue as job_enqueue;
use crate::auth::resolve_workspace;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::json;

/// The two statuses a resolvable entity can sit in: `pending_approval` (drafts,
/// storage backends) and `awaiting_approval` (build gates, gate.js). Kept as one
/// list so list + CAS agree on exactly what "pending" means.
const PENDING_STATUSES: [&str; 2] = ["pending_approval", "awaiting_approval"];

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// Lists everything awaiting a human decision in this workspace, newest first.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Entity>>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let rows = state
        .conn
        .query(
            &format!(
                "SELECT {COLS_ENTITY} FROM entities \
                 WHERE workspace_id = ?1 AND status IN ('pending_approval', 'awaiting_approval') \
                 ORDER BY created_at DESC LIMIT 500"
            ),
            libsql::params![workspace_id],
        )
        .await?;
    Ok(Json(collect(rows, read_entity).await?))
}

#[derive(Deserialize)]
pub struct ResolveBody {
    #[serde(default)]
    typed: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

/// Approve a pending entity: enforces typed-confirm, CAS-transitions to
/// `approved`, emits `<type>.approved`, and enqueues an `execute_approval` job.
pub async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ResolveBody>,
) -> ApiResult<Json<Entity>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let entity = load_pending(&state, &workspace_id, &id).await?;

    if requires_typed_confirm(&entity) {
        let phrase = confirm_phrase(&entity);
        let typed = req.typed.as_deref().unwrap_or("").trim();
        if typed != phrase {
            return Err(ApiError::BadRequest(format!(
                "this approval requires typed confirmation - resend with typed = '{phrase}'"
            )));
        }
    }

    cas_transition(&state, &workspace_id, &id, "approved").await?;
    emit(&state.conn, &workspace_id, &format!("{}.approved", entity.r#type), Some(&id), "api", &json!({})).await?;
    job_enqueue(
        &state,
        &workspace_id,
        "execute_approval",
        &json!({ "entity_id": id, "entity_type": entity.r#type }),
        0,
    )
    .await?;

    fetch_one(&state, &workspace_id, &id).await
}

/// Deny a pending entity: CAS-transitions to `rejected` and emits
/// `<type>.rejected`. No job is enqueued - a denied action never executes.
pub async fn deny(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ResolveBody>,
) -> ApiResult<Json<Entity>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let entity = load_pending(&state, &workspace_id, &id).await?;

    cas_transition(&state, &workspace_id, &id, "rejected").await?;
    emit(&state.conn, &workspace_id, &format!("{}.rejected", entity.r#type), Some(&id), "api", &json!({})).await?;

    fetch_one(&state, &workspace_id, &id).await
}

/// Loads an entity scoped to the workspace and asserts it is still pending. A
/// missing row is 404; an already-resolved one is 409 (the race loser).
async fn load_pending(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Entity> {
    let entity = read_entity_scoped(state, workspace_id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("approval '{id}' not found")))?;
    let status = entity.status.as_deref().unwrap_or("");
    if !PENDING_STATUSES.contains(&status) {
        return Err(ApiError::Conflict(format!("approval '{id}' is already {status}")));
    }
    Ok(entity)
}

/// The CAS write: only flips a row still in a pending status, so a second
/// concurrent resolve matches zero rows and 409s rather than double-firing.
async fn cas_transition(state: &AppState, workspace_id: &str, id: &str, to: &str) -> ApiResult<()> {
    let n = state
        .conn
        .execute(
            "UPDATE entities SET status = ?1, updated_at = ?2 \
             WHERE id = ?3 AND workspace_id = ?4 \
               AND status IN ('pending_approval', 'awaiting_approval')",
            libsql::params![to, crate::ids::now_secs(), id, workspace_id],
        )
        .await?;
    if n == 0 {
        return Err(ApiError::Conflict(format!("approval '{id}' was already resolved")));
    }
    Ok(())
}

fn requires_typed_confirm(entity: &Entity) -> bool {
    entity.attrs.get("requires_typed_confirm").and_then(|v| v.as_bool()) == Some(true)
}

/// The phrase a typed-confirm gate demands: the build node id when present
/// (gate.js stamps `attrs.node`), else the entity title, else its id.
fn confirm_phrase(entity: &Entity) -> String {
    if let Some(node) = entity.attrs.get("node").and_then(|v| v.as_str()) {
        if !node.is_empty() {
            return node.to_string();
        }
    }
    entity.title.clone().unwrap_or_else(|| entity.id.clone())
}

async fn read_entity_scoped(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Option<Entity>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1 AND workspace_id = ?2"),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(read_entity(&row)?)),
        None => Ok(None),
    }
}

async fn fetch_one(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Entity>> {
    read_entity_scoped(state, workspace_id, id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("approval '{id}' not found")))
}
