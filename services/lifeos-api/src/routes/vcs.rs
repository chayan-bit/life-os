//! `/api/vcs/*` - the generic lifeos-vcs CLI surface (issue #86): commit,
//! history, checkout. Unlike `/api/files/commit` (issue #58, Drive-file
//! materialization, content passed as a plain string with no real byte
//! storage), these routes are the first callers to actually persist bytes
//! through lifeos-vcs's real CAS + commit model (#81/#82): `store_blob`
//! chunk-hashes and stores the content, `commit_version` updates the
//! entity's `blob_ref` and appends the `version.created` event in one call.
//!
//! No update/delete/rewrite route exists here, matching docs/AGENT-CONTROL.md
//! §1: VCS internals only ever grow forward through these routes.
//!
//! Issue #87 adds the routes the TimeTravel frontend needs beyond commit/
//! history/checkout: `diff` (dispatches to lifeos-vcs's per-type diff, #85 -
//! real for text-backed types, an honest `supported: false` + blocking issue
//! for the rest), and read/forward-only `refs`/`branch`/`tag`/`snapshot` -
//! branches move, tags refuse to move, and there is still no way to force a
//! branch backward or delete a ref through this surface.

use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{read_entity, Entity, COLS_ENTITY};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct CommitFile {
    /// Existing entity id to commit a new version onto; omit to create a new file.
    entity_id: Option<String>,
    name: String,
    mime: Option<String>,
    /// Base64-encoded file bytes - the CLI reads the local file and encodes
    /// it client-side rather than the API trusting a client-supplied
    /// filesystem path (no path-traversal-shaped trust boundary).
    content_base64: String,
    #[serde(default)]
    message: Option<String>,
    workspace_id: Option<String>,
}

pub async fn commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CommitFile>,
) -> ApiResult<Json<Entity>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let content = base64::engine::general_purpose::STANDARD
        .decode(&req.content_base64)
        .map_err(|e| ApiError::BadRequest(format!("content_base64 is not valid base64: {e}")))?;

    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let blob_ref =
        lifeos_vcs::store_blob(&state.vcs_store, &content).map_err(|e| ApiError::Internal(format!("blob store failed: {e}")))?;
    let size = content.len() as u64;

    let (id, parent_blob_ref, is_new) = match &req.entity_id {
        Some(existing_id) => {
            let parent = existing_blob_ref(&state, &workspace_id, existing_id).await?;
            (existing_id.clone(), parent, false)
        }
        None => (new_id("file"), None, true),
    };

    if parent_blob_ref.as_deref() == Some(blob_ref.as_str()) {
        return Err(ApiError::BadRequest("content unchanged since the last version".into()));
    }

    let now = now_secs();
    if is_new {
        let attrs = json!({ "name": req.name, "mime": req.mime, "size": size });
        let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
        state
            .conn
            .execute(
                "INSERT INTO entities (id, workspace_id, module, type, title, attrs, source, blob_ref, created_at, updated_at) \
                 VALUES (?1, ?2, 'files', 'file', ?3, ?4, 'cli', ?5, ?6, ?6)",
                libsql::params![id.clone(), workspace_id.clone(), req.name.clone(), attrs_str, blob_ref.clone(), now],
            )
            .await?;
    }

    lifeos_vcs::commit_version(
        &state.conn,
        &workspace_id,
        &id,
        &blob_ref,
        parent_blob_ref.as_deref(),
        "cli",
        req.message.as_deref().unwrap_or(""),
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("commit_version failed: {e}")))?;

    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }

    fetch_one(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct CheckoutQuery {
    entity_id: String,
    /// Specific historical version to retrieve; omits to the entity's
    /// current (latest) `blob_ref`.
    blob_ref: Option<String>,
}

/// Retrieval by hash "checks out" a version - no separate mutating verb is
/// needed to look at old content (docs/VERSIONING.md §2.3 note under #82).
pub async fn checkout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CheckoutQuery>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let blob_ref = match q.blob_ref {
        Some(r) => r,
        None => existing_blob_ref(&state, &workspace_id, &q.entity_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("entity '{}' has no committed version", q.entity_id)))?,
    };

    let bytes = lifeos_vcs::read_blob(&state.vcs_store, &blob_ref)
        .map_err(|e| ApiError::NotFound(format!("blob '{blob_ref}' not found: {e}")))?;

    Ok(([("content-type", "application/octet-stream")], bytes).into_response())
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    entity_id: String,
}

/// Version history reconstructed from `events` - no separate history table
/// (docs/VERSIONING.md §2.3).
pub async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<lifeos_vcs::VersionEntry>>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let entries = lifeos_vcs::history(&state.conn, &workspace_id, &q.entity_id)
        .await
        .map_err(|e| ApiError::Internal(format!("history query failed: {e}")))?;
    Ok(Json(entries))
}

#[derive(Deserialize)]
pub struct DiffQuery {
    entity_id: String,
    old: String,
    /// Compares against the entity's current version when omitted.
    new: Option<String>,
}

/// Maps a committed file's name to the coarse `entity_type` `strategy_for`
/// (lifeos-vcs, issue #85) dispatches on. Only extensions with a real,
/// working diff pipeline map to a named type - everything else maps to
/// "binary" so `diff_blobs` names the specific blocking issue rather than
/// this route silently guessing.
fn diff_kind_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tscn") {
        "godot_tscn"
    } else if lower.ends_with(".tres") {
        "godot_tres"
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        "markdown"
    } else if lower.ends_with(".rs")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".py")
        || lower.ends_with(".json")
        || lower.ends_with(".toml")
        || lower.ends_with(".css")
    {
        "code"
    } else if lower.ends_with(".txt") {
        "text"
    } else if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".gif") {
        "image"
    } else if lower.ends_with(".mp4") || lower.ends_with(".mov") {
        "video"
    } else if lower.ends_with(".mp3") || lower.ends_with(".wav") {
        "audio"
    } else if lower.ends_with(".pdf") {
        "pdf"
    } else if lower.ends_with(".docx") {
        "docx"
    } else {
        "binary"
    }
}

/// A real diff for text-backed types; an honest `supported: false` +
/// blocking issue for everything else - never a silent no-op or fake diff
/// (docs/VERSIONING.md §3, issue #85).
pub async fn diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DiffQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let entity = fetch_one(&state, &workspace_id, &q.entity_id).await?.0;
    let title = entity.title.as_deref().unwrap_or("");
    let name = entity.attrs.get("name").and_then(|v| v.as_str()).unwrap_or(title);
    let kind = diff_kind_for(name);

    let new_ref = match q.new {
        Some(r) => r,
        None => existing_blob_ref(&state, &workspace_id, &q.entity_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("entity '{}' has no committed version", q.entity_id)))?,
    };

    match lifeos_vcs::diff_blobs(&state.vcs_store, &q.old, &new_ref, kind) {
        Ok(result) => Ok(Json(json!({
            "supported": true,
            "kind": kind,
            "summary": result.summary(),
            "inserted": result.inserted,
            "deleted": result.deleted,
            "lines": result.lines,
        }))),
        Err(lifeos_vcs::DiffError::UnsupportedKind { kind, blocked_by }) => {
            Ok(Json(json!({ "supported": false, "kind": kind, "blocked_by": blocked_by })))
        }
        Err(e) => Err(ApiError::Internal(format!("diff failed: {e}"))),
    }
}

#[derive(Deserialize)]
pub struct RefsQuery {
    kind: String,
}

/// Read-only branch/tag listing (docs/AGENT-CONTROL.md §1: VCS internals only
/// ever grow forward - no branch-force/rewrite route exists anywhere).
pub async fn list_refs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RefsQuery>,
) -> ApiResult<Json<Vec<lifeos_vcs::RefEntry>>> {
    if q.kind != "branch" && q.kind != "tag" {
        return Err(ApiError::BadRequest("kind must be 'branch' or 'tag'".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let refs = lifeos_vcs::list_refs(&state.conn, &workspace_id, &q.kind)
        .await
        .map_err(|e| ApiError::Internal(format!("list_refs failed: {e}")))?;
    Ok(Json(refs))
}

#[derive(Deserialize)]
pub struct CreateRefBody {
    name: String,
    workspace_id: Option<String>,
}

/// Snapshots the workspace's current state and points a moving branch at it.
pub async fn create_branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRefBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let snapshot_ref = lifeos_vcs::create_snapshot(&state.conn, &state.vcs_store, &workspace_id)
        .await
        .map_err(|e| ApiError::Internal(format!("create_snapshot failed: {e}")))?;
    let now = now_secs();
    lifeos_vcs::set_branch(&state.conn, &workspace_id, &req.name, &snapshot_ref, now)
        .await
        .map_err(|e| ApiError::Internal(format!("set_branch failed: {e}")))?;
    Ok(Json(json!({ "name": req.name, "snapshot_ref": snapshot_ref, "updated_at": now })))
}

/// Snapshots the workspace's current state and points a fixed tag at it.
/// Refuses (400) if the tag name already points at a different snapshot -
/// tags never move (docs/VERSIONING.md §2.4).
pub async fn create_tag(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRefBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let snapshot_ref = lifeos_vcs::create_snapshot(&state.conn, &state.vcs_store, &workspace_id)
        .await
        .map_err(|e| ApiError::Internal(format!("create_snapshot failed: {e}")))?;
    let now = now_secs();
    lifeos_vcs::set_tag(&state.conn, &workspace_id, &req.name, &snapshot_ref, now)
        .await
        .map_err(|e| match e {
            lifeos_vcs::SnapshotError::TagImmutable { name } => {
                ApiError::BadRequest(format!("tag '{name}' already points elsewhere and cannot be moved"))
            }
            other => ApiError::Internal(format!("set_tag failed: {other}")),
        })?;
    Ok(Json(json!({ "name": req.name, "snapshot_ref": snapshot_ref, "updated_at": now })))
}

#[derive(Deserialize)]
pub struct SnapshotQuery {
    snapshot_ref: String,
}

/// Reads a snapshot's `{entity_id -> blob_ref}` manifest - "show me
/// everything as it was 3 weeks ago" (docs/VERSIONING.md §2.4).
pub async fn read_snapshot(
    State(state): State<AppState>,
    Query(q): Query<SnapshotQuery>,
) -> ApiResult<Json<lifeos_vcs::SnapshotManifest>> {
    let manifest = lifeos_vcs::read_snapshot(&state.vcs_store, &q.snapshot_ref)
        .map_err(|e| ApiError::NotFound(format!("snapshot '{}' not found: {e}", q.snapshot_ref)))?;
    Ok(Json(manifest))
}

async fn existing_blob_ref(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Option<String>> {
    let mut rows = state
        .conn
        .query(
            "SELECT blob_ref FROM entities WHERE id = ?1 AND workspace_id = ?2",
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<String>>(0)?),
        None => Err(ApiError::NotFound(format!("entity '{id}' not found"))),
    }
}

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

#[derive(Deserialize)]
pub struct BlobQuery {
    blob_ref: String,
    workspace_id: Option<String>,
}

/// `GET /api/vcs/blob` - fetch any blob by content hash with cross-backend
/// fallback (issues #108/#109, docs/STORAGE-BACKENDS.md §5): local CAS
/// first, then the workspace's primary/mirror backends. Bytes are
/// BLAKE3-verified wherever they came from. Always served as octet-stream -
/// rendering decisions (markdown vs placeholder) belong to the frontend,
/// which knows the entity's mime.
pub async fn blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BlobQuery>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&headers, &state.config, q.workspace_id.as_deref())?;
    let bytes = crate::storage::read_blob(&state, &workspace_id, &q.blob_ref).await?;
    Ok(([("content-type", "application/octet-stream")], bytes).into_response())
}
