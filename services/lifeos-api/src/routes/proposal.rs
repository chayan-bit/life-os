//! `/api/proposal` - "Notion + GitHub combined": propose a changeset to shared
//! entities, review the diff, then merge or reject (issue #148). The
//! GitHub-analog on the entity graph.
//!
//! A proposal is itself an ENTITY (`module='system'`, `type='proposal'`) - no
//! new table, no migration. Its `attrs` carry the change set:
//!
//! ```json
//! {
//!   "title": "...", "base_ref": "main", "reviewers": [...], "status": "open",
//!   "changes": [
//!     { "entity_id": "ent_x", "base_updated_at": 123, "base_version": "evt_...",
//!       "attr_patch": { "priority": { "before": "low", "after": "high" } } }
//!   ]
//! }
//! ```
//!
//! Changes are captured as an event-sourced patch list (before/after per
//! changed attr), never raw SQL. Merge applies each patch as a normal
//! `entity.updated` event stamped `caused_by_event_id` -> the proposal's
//! creation event, so a merge is fully reversible and auditable via the
//! append-only log (docs/DATA-MODEL.md §4). Review comments are plain
//! annotations on the proposal entity (`POST /api/annotation` with
//! `entity_id` = the proposal id) - there is no parallel comment store.
//!
//! Conflict check: each change records the target's `updated_at` AND its latest
//! event id (`base_version`) at draft time. `updated_at` is only
//! second-granularity, so a same-second edit could slip past it; the event id
//! is monotonic and changes on every write, so it is the robust drift signal.
//! On merge, if any target drifted, nothing is applied - the proposal is
//! flagged `needs_rebase` and a 409 is returned (all-or-nothing).

use crate::audit::emit;
use crate::auth::{bearer_claims, resolve_role, resolve_workspace, Role};
use crate::db::{index_entity, workspace_exists};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_entity, Entity, COLS_ENTITY};
use crate::reconcile::replay_entity_attrs;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use libsql::Connection;
use serde::Deserialize;
use serde_json::{json, Map, Value};

const PROPOSAL_MODULE: &str = "system";
const PROPOSAL_TYPE: &str = "proposal";
/// Bounds so a single request can't smuggle an unbounded change set.
const MAX_CHANGES: usize = 200;
const MAX_ATTRS_PER_CHANGE: usize = 100;

// ------------------------------------------------------------ request shapes

#[derive(Deserialize)]
pub struct ChangeInput {
    entity_id: String,
    /// Flat map of `attr name -> proposed new value` (the "after" side). The
    /// "before" side is read from the live entity at draft time, not trusted
    /// from the client.
    #[serde(default)]
    patch: Map<String, Value>,
}

#[derive(Deserialize)]
pub struct CreateProposal {
    title: String,
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    reviewers: Vec<String>,
    #[serde(default)]
    changes: Vec<ChangeInput>,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

/// The lone `workspace_id` query param the read routes accept (local-first;
/// a JWT claim still overrides it).
#[derive(Deserialize)]
pub struct WsQuery {
    workspace_id: Option<String>,
}

/// Merge/reject accept an optional explicit `workspace_id` (local-first) the
/// same way the entity routes do; identity/role still come from auth.
#[derive(Deserialize, Default)]
pub struct ActionBody {
    #[serde(default)]
    workspace_id: Option<String>,
}

// -------------------------------------------------------------- shared reads

/// A live view of a target entity: its (reconciled) attrs, its `updated_at`,
/// and its latest event id - the two drift signals the conflict check uses.
struct Snapshot {
    attrs: Value,
    updated_at: i64,
    version: Option<String>,
}

/// Latest event id for an entity, or `None` if it has no events yet. Events use
/// monotonic ULID ids, so this strictly increases on every write - a robust
/// "has this changed?" marker that second-granularity `updated_at` can't give.
async fn latest_event_id(conn: &Connection, ws: &str, entity_id: &str) -> ApiResult<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT id FROM events WHERE workspace_id = ?1 AND entity_id = ?2 ORDER BY id DESC LIMIT 1",
            libsql::params![ws, entity_id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get(0)?),
        None => None,
    })
}

/// Snapshot a target entity, or `None` if it does not exist in this workspace.
/// Attrs come from replaying the event log (reconcile machinery) so a forced
/// sync conflict can't feed us stale row bytes; it falls back to the row's own
/// attrs when the log carries no snapshot.
async fn snapshot(conn: &Connection, ws: &str, entity_id: &str) -> ApiResult<Option<Snapshot>> {
    let mut rows = conn
        .query(
            "SELECT attrs, updated_at FROM entities WHERE id = ?1 AND workspace_id = ?2",
            libsql::params![entity_id, ws],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let attrs_str: String = row.get(0)?;
    let updated_at: i64 = row.get(1)?;
    let row_attrs: Value = serde_json::from_str(&attrs_str).unwrap_or_else(|_| json!({}));
    let attrs = replay_entity_attrs(conn, ws, entity_id).await?.unwrap_or(row_attrs);
    let version = latest_event_id(conn, ws, entity_id).await?;
    Ok(Some(Snapshot { attrs, updated_at, version }))
}

/// Fetch a proposal entity scoped to a workspace, or 404. The `type` guard
/// keeps a stray entity id from resolving as a proposal.
async fn fetch_proposal(state: &AppState, ws: &str, id: &str) -> ApiResult<Entity> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1 AND workspace_id = ?2 AND type = ?3"),
            libsql::params![id, ws, PROPOSAL_TYPE],
        )
        .await?;
    match rows.next().await? {
        Some(row) => read_entity(&row),
        None => Err(ApiError::NotFound(format!("proposal '{id}' not found"))),
    }
}

// ------------------------------------------------------------------- helpers

/// Resolve `(workspace, actor_user_id?, role)` for a write action. `explicit`
/// is the body's `workspace_id` (local-first); a JWT claim overrides it.
async fn caller(
    state: &AppState,
    headers: &HeaderMap,
    explicit: Option<&str>,
) -> ApiResult<(String, Option<String>, Role)> {
    let workspace = resolve_workspace(headers, &state.config, explicit)?;
    let user_id = bearer_claims(headers, &state.config.jwt_secret).map(|c| c.sub);
    let role = resolve_role(&state.conn, &workspace, user_id.as_deref()).await?;
    Ok((workspace, user_id, role))
}

/// Merging/rejecting a proposal is a review-authority action: editor or owner
/// only. A viewer is already blocked by the strict-mode write middleware; an
/// agent passes that middleware (ordinary write) so it must be denied here.
fn require_editor(role: Role) -> ApiResult<()> {
    match role {
        Role::Owner | Role::Editor => Ok(()),
        _ => Err(ApiError::Forbidden(
            "merging or rejecting a proposal requires an editor or owner".into(),
        )),
    }
}

/// Append an event carrying a `caused_by_event_id` causal pointer (the
/// `audit::emit` helper doesn't take one). Used to link each applied
/// `entity.updated` back to the proposal's creation event.
async fn emit_caused_by(
    conn: &Connection,
    ws: &str,
    event_type: &str,
    entity_id: Option<&str>,
    actor: &str,
    attrs: &Value,
    caused_by: Option<&str>,
) -> ApiResult<String> {
    let id = new_id("evt");
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs, caused_by_event_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        libsql::params![id.clone(), ws, now_secs(), event_type, entity_id, actor, attrs_str, caused_by],
    )
    .await?;
    Ok(id)
}

/// The proposal's creation event id (the causal root every merge points at).
async fn proposal_created_event_id(conn: &Connection, ws: &str, id: &str) -> ApiResult<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT id FROM events WHERE workspace_id = ?1 AND entity_id = ?2 \
             AND type = 'proposal.created' ORDER BY id ASC LIMIT 1",
            libsql::params![ws, id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get(0)?),
        None => None,
    })
}

/// Persist new `attrs` (and mirror `status` into the entity column for cheap
/// list filtering) onto a proposal row.
async fn persist_proposal_attrs(state: &AppState, ws: &str, id: &str, attrs: &Value) -> ApiResult<()> {
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let status = attrs.get("status").and_then(|v| v.as_str()).unwrap_or("open");
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = ?1, status = ?2, updated_at = ?3 \
             WHERE id = ?4 AND workspace_id = ?5 AND type = ?6",
            libsql::params![attrs_str, status, now_secs(), id, ws, PROPOSAL_TYPE],
        )
        .await?;
    Ok(())
}

// -------------------------------------------------------------------- routes

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateProposal>,
) -> ApiResult<Json<Entity>> {
    if req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title is required".into()));
    }
    if req.changes.is_empty() {
        return Err(ApiError::BadRequest("a proposal needs at least one change".into()));
    }
    if req.changes.len() > MAX_CHANGES {
        return Err(ApiError::BadRequest(format!("too many changes (max {MAX_CHANGES})")));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    // Build the stored change list: read each live target for the "before"
    // side + the base version, and reject a change against a missing entity.
    let mut changes = Vec::with_capacity(req.changes.len());
    for ch in &req.changes {
        if ch.entity_id.trim().is_empty() {
            return Err(ApiError::BadRequest("each change needs an entity_id".into()));
        }
        if ch.patch.is_empty() {
            return Err(ApiError::BadRequest("each change needs at least one changed attr".into()));
        }
        if ch.patch.len() > MAX_ATTRS_PER_CHANGE {
            return Err(ApiError::BadRequest(format!(
                "too many attrs in one change (max {MAX_ATTRS_PER_CHANGE})"
            )));
        }
        let snap = snapshot(&state.conn, &workspace_id, &ch.entity_id)
            .await?
            .ok_or_else(|| ApiError::BadRequest(format!("target entity '{}' not found", ch.entity_id)))?;
        let current = snap.attrs.as_object().cloned().unwrap_or_default();
        let mut attr_patch = Map::new();
        for (attr, after) in &ch.patch {
            let before = current.get(attr).cloned().unwrap_or(Value::Null);
            attr_patch.insert(attr.clone(), json!({ "before": before, "after": after }));
        }
        changes.push(json!({
            "entity_id": ch.entity_id,
            "base_updated_at": snap.updated_at,
            "base_version": snap.version,
            "attr_patch": attr_patch,
        }));
    }

    let attrs = json!({
        "title": req.title,
        "base_ref": req.base_ref,
        "reviewers": req.reviewers,
        "status": "open",
        "changes": changes,
    });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    let id = new_id("prop");
    let now = now_secs();

    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'open', NULL, ?6, 'api', NULL, ?7, ?7)",
            libsql::params![id.clone(), workspace_id.clone(), PROPOSAL_MODULE, PROPOSAL_TYPE, req.title, attrs_str, now],
        )
        .await?;

    // Snapshot attrs on the creation event so the proposal is itself
    // reconcilable, and so merges have a causal root to point at.
    emit(
        &state.conn,
        &workspace_id,
        "proposal.created",
        Some(&id),
        "user",
        &json!({ "attrs": attrs }),
    )
    .await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }
    Ok(Json(fetch_proposal(&state, &workspace_id, &id).await?))
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Entity>>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let mut sql = format!(
        "SELECT {COLS_ENTITY} FROM entities WHERE workspace_id = ?1 AND module = ?2 AND type = ?3"
    );
    let mut binds: Vec<String> = vec![workspace_id, PROPOSAL_MODULE.into(), PROPOSAL_TYPE.into()];
    if let Some(status) = &params.status {
        sql.push_str(" AND status = ?4");
        binds.push(status.clone());
    }
    let limit = params.limit.unwrap_or(200).min(2000);
    let offset = params.offset.unwrap_or(0);
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit} OFFSET {offset}"));

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_entity).await?))
}

pub async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<WsQuery>,
) -> ApiResult<Json<Entity>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    Ok(Json(fetch_proposal(&state, &workspace_id, &id).await?))
}

/// Structured before/after diff for the UI. `before` is the LIVE current attr
/// value (recomputed, not the draft-time snapshot); `after` is the proposed
/// value. Per-entity `conflict` flags drift from the recorded base. Read-only.
pub async fn diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<WsQuery>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let proposal = fetch_proposal(&state, &workspace_id, &id).await?;
    let attrs = &proposal.attrs;
    let changes = attrs.get("changes").and_then(|c| c.as_array()).cloned().unwrap_or_default();

    let mut entities = Vec::with_capacity(changes.len());
    for ch in &changes {
        let entity_id = ch.get("entity_id").and_then(|v| v.as_str()).unwrap_or_default();
        let base_updated_at = ch.get("base_updated_at").and_then(|v| v.as_i64());
        let base_version = ch.get("base_version").and_then(|v| v.as_str());
        let attr_patch = ch.get("attr_patch").and_then(|v| v.as_object()).cloned().unwrap_or_default();

        let snap = snapshot(&state.conn, &workspace_id, entity_id).await?;
        let (current, current_updated, current_version, exists) = match &snap {
            Some(s) => (
                s.attrs.as_object().cloned().unwrap_or_default(),
                Some(s.updated_at),
                s.version.clone(),
                true,
            ),
            None => (Map::new(), None, None, false),
        };
        let conflict = !exists
            || current_updated != base_updated_at
            || current_version.as_deref() != base_version;

        let mut attr_rows = Vec::with_capacity(attr_patch.len());
        for (attr, ba) in &attr_patch {
            let before = current.get(attr).cloned().unwrap_or(Value::Null);
            let after = ba.get("after").cloned().unwrap_or(Value::Null);
            attr_rows.push(json!({
                "attr": attr,
                "before": before,
                "after": after,
                "changed": before != after,
            }));
        }
        entities.push(json!({
            "entity_id": entity_id,
            "exists": exists,
            "base_updated_at": base_updated_at,
            "current_updated_at": current_updated,
            "conflict": conflict,
            "attrs": attr_rows,
        }));
    }

    let needs_rebase = entities.iter().any(|e| e["conflict"] == json!(true));
    Ok(Json(json!({
        "proposal_id": id,
        "title": attrs.get("title").cloned().unwrap_or(Value::Null),
        "status": attrs.get("status").cloned().unwrap_or(Value::Null),
        "base_ref": attrs.get("base_ref").cloned().unwrap_or(Value::Null),
        "reviewers": attrs.get("reviewers").cloned().unwrap_or(json!([])),
        "needs_rebase": needs_rebase,
        "entities": entities,
    })))
}

pub async fn merge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActionBody>,
) -> ApiResult<Json<Value>> {
    let (workspace_id, actor, role) = caller(&state, &headers, body.workspace_id.as_deref()).await?;
    require_editor(role)?;

    let proposal = fetch_proposal(&state, &workspace_id, &id).await?;
    let status = proposal.attrs.get("status").and_then(|v| v.as_str()).unwrap_or("open");
    if status != "open" {
        return Err(ApiError::Conflict(format!("proposal is already '{status}'")));
    }
    let changes = proposal
        .attrs
        .get("changes")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    // Phase 1: conflict-check EVERY target before touching anything. A merge is
    // all-or-nothing (no partial application) - if any target drifted or
    // vanished, we refuse and flag the proposal for rebase.
    let mut conflicts = Vec::new();
    let mut plans: Vec<(String, Value)> = Vec::new();
    for ch in &changes {
        let entity_id = ch.get("entity_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let base_updated_at = ch.get("base_updated_at").and_then(|v| v.as_i64());
        let base_version = ch.get("base_version").and_then(|v| v.as_str()).map(str::to_string);
        let attr_patch = ch.get("attr_patch").and_then(|v| v.as_object()).cloned().unwrap_or_default();

        match snapshot(&state.conn, &workspace_id, &entity_id).await? {
            None => conflicts.push(json!({ "entity_id": entity_id, "reason": "entity no longer exists" })),
            Some(s) => {
                if Some(s.updated_at) != base_updated_at || s.version != base_version {
                    conflicts.push(json!({
                        "entity_id": entity_id,
                        "reason": "entity changed since the proposal was drafted",
                    }));
                } else {
                    let mut new_attrs = s.attrs.as_object().cloned().unwrap_or_default();
                    for (attr, ba) in &attr_patch {
                        new_attrs.insert(attr.clone(), ba.get("after").cloned().unwrap_or(Value::Null));
                    }
                    plans.push((entity_id, Value::Object(new_attrs)));
                }
            }
        }
    }

    if !conflicts.is_empty() {
        let mut flagged = proposal.attrs.as_object().cloned().unwrap_or_default();
        flagged.insert("needs_rebase".into(), json!(true));
        flagged.insert("conflicts".into(), json!(conflicts));
        persist_proposal_attrs(&state, &workspace_id, &id, &Value::Object(flagged)).await?;
        emit(
            &state.conn,
            &workspace_id,
            "proposal.needs_rebase",
            Some(&id),
            "user",
            &json!({ "conflicts": conflicts }),
        )
        .await?;
        return Err(ApiError::Conflict(format!(
            "proposal needs rebase: {} target(s) changed since drafting",
            conflicts.len()
        )));
    }

    // Phase 2: apply every patch as a normal entity.updated event stamped with
    // the causal pointer back to this proposal's creation.
    let cause = proposal_created_event_id(&state.conn, &workspace_id, &id).await?;
    let now = now_secs();
    for (entity_id, new_attrs) in &plans {
        let attrs_str = serde_json::to_string(new_attrs).unwrap_or_else(|_| "{}".into());
        state
            .conn
            .execute(
                "UPDATE entities SET attrs = ?1, updated_at = ?2 WHERE id = ?3 AND workspace_id = ?4",
                libsql::params![attrs_str, now, entity_id.clone(), workspace_id.clone()],
            )
            .await?;
        emit_caused_by(
            &state.conn,
            &workspace_id,
            "entity.updated",
            Some(entity_id),
            "proposal",
            &json!({ "attrs": new_attrs, "proposal_id": id }),
            cause.as_deref(),
        )
        .await?;
        if let Err(e) = index_entity(&state.conn, entity_id).await {
            tracing::warn!("derived index upsert failed for {entity_id}: {e}");
        }
    }

    let mut merged = proposal.attrs.as_object().cloned().unwrap_or_default();
    merged.insert("status".into(), json!("merged"));
    merged.insert("merged_at".into(), json!(now));
    merged.insert("merged_by".into(), json!(actor));
    merged.remove("needs_rebase");
    merged.remove("conflicts");
    persist_proposal_attrs(&state, &workspace_id, &id, &Value::Object(merged)).await?;
    emit(
        &state.conn,
        &workspace_id,
        "proposal.merged",
        Some(&id),
        "user",
        &json!({ "applied": plans.len(), "by": actor }),
    )
    .await?;

    Ok(Json(json!({ "id": id, "status": "merged", "applied": plans.len() })))
}

pub async fn reject(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActionBody>,
) -> ApiResult<Json<Value>> {
    let (workspace_id, actor, role) = caller(&state, &headers, body.workspace_id.as_deref()).await?;
    require_editor(role)?;

    let proposal = fetch_proposal(&state, &workspace_id, &id).await?;
    let status = proposal.attrs.get("status").and_then(|v| v.as_str()).unwrap_or("open");
    if status != "open" {
        return Err(ApiError::Conflict(format!("proposal is already '{status}'")));
    }

    let mut rejected = proposal.attrs.as_object().cloned().unwrap_or_default();
    rejected.insert("status".into(), json!("rejected"));
    rejected.insert("rejected_by".into(), json!(actor));
    persist_proposal_attrs(&state, &workspace_id, &id, &Value::Object(rejected)).await?;
    emit(
        &state.conn,
        &workspace_id,
        "proposal.rejected",
        Some(&id),
        "user",
        &json!({ "by": actor }),
    )
    .await?;
    Ok(Json(json!({ "id": id, "status": "rejected" })))
}
