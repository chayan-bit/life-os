//! Local file version-history commit (issue #58, docs/VERSIONING.md). Free
//! and local-only: `commit` never touches Drive or any external provider -
//! it hashes content with `lifeos_vcs::hash_bytes` (BLAKE3), upserts a
//! `file` entity's `blob_ref`, and appends a `version.created` event whose
//! `parent_blob_ref` chains to the file's previous version. Version history
//! is therefore just a query over `events` (`GET /api/event?entity_id=<id>
//! &type=version.created`) - no separate history table (docs/VERSIONING.md
//! §2.3).

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{read_entity, Entity, COLS_ENTITY};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct CommitFile {
    /// Existing `file` entity id to commit a new version onto; omit to
    /// create a new file.
    entity_id: Option<String>,
    name: String,
    mime: Option<String>,
    /// Raw text content for this version. Binary blob storage and the
    /// FastCDC-chunked large-file path (docs/VERSIONING.md §2.2) are
    /// deferred - this ships the content-addressed commit/history model for
    /// small text/config/doc files first.
    content: String,
    #[serde(default)]
    message: Option<String>,
    parent_folder: Option<String>,
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
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let blob_ref = lifeos_vcs::hash_bytes(req.content.as_bytes());
    let size = req.content.len() as u64;

    let (id, parent_blob_ref) = match &req.entity_id {
        Some(existing_id) => {
            let parent = existing_blob_ref(&state, &workspace_id, existing_id).await?;
            (existing_id.clone(), parent)
        }
        None => (new_id("file"), None),
    };

    if parent_blob_ref.as_deref() == Some(blob_ref.as_str()) {
        return Err(ApiError::BadRequest("content unchanged since the last version".into()));
    }

    let version_no = version_count(&state, &workspace_id, &id).await? + 1;
    let attrs = json!({
        "name": req.name,
        "mime": req.mime,
        "size": size,
        "blob_ref": blob_ref,
        "version_no": version_no,
        "parent_folder": req.parent_folder,
    });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'files', 'file', NULL, ?3, NULL, NULL, ?4, 'local', ?5, ?6, ?6) \
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, attrs = excluded.attrs, \
             blob_ref = excluded.blob_ref, updated_at = excluded.updated_at",
            libsql::params![id.clone(), workspace_id.clone(), req.name.clone(), attrs_str, blob_ref.clone(), now],
        )
        .await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }

    emit(
        &state.conn,
        &workspace_id,
        "version.created",
        Some(&id),
        "api",
        &json!({
            "entity_id": id,
            "blob_ref": blob_ref,
            "parent_blob_ref": parent_blob_ref,
            "message": req.message.unwrap_or_default(),
        }),
    )
    .await?;

    // Auto-trigger ingest on version.created (docs/MEDIA-INTELLIGENCE.md §4,
    // issue #91). The third documented trigger, asset.generated, has no
    // emitter anywhere in this codebase yet - no Design/Marketing module
    // exists to produce generated assets - so it isn't wired here; wire it
    // at that module's emit site once it lands. A failed enqueue never fails
    // the commit itself, same fire-and-forget reasoning as `Embedder::embed`.
    if let Err(e) = super::job::enqueue(
        &state,
        &workspace_id,
        "ingest",
        &json!({ "entity_id": id, "blob_ref": blob_ref }),
        0,
    )
    .await
    {
        tracing::warn!("auto-ingest enqueue failed for {id}: {e:?}");
    }

    fetch_one(&state, &workspace_id, &id).await
}

async fn existing_blob_ref(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Option<String>> {
    let mut rows = state
        .conn
        .query(
            "SELECT blob_ref FROM entities WHERE id = ?1 AND workspace_id = ?2 AND module = 'files'",
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<String>>(0)?),
        None => Err(ApiError::NotFound(format!("file '{id}' not found"))),
    }
}

async fn version_count(state: &AppState, workspace_id: &str, entity_id: &str) -> ApiResult<i64> {
    let mut rows = state
        .conn
        .query(
            "SELECT COUNT(*) FROM events WHERE workspace_id = ?1 AND entity_id = ?2 AND type = 'version.created'",
            libsql::params![workspace_id, entity_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<i64>(0)?),
        None => Ok(0),
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
