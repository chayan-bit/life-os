//! Google Drive thin proxy tool (issue #53, docs/INTEGRATIONS.md). `list`
//! reads straight through Nango's proxy; `upload`/`share` only ever draft
//! (docs/SECURITY.md §2) - this file has no code path that calls Drive's
//! upload/permissions APIs. `sync` (issue #58, docs/MODULES.md §3.3) is
//! also free - it only ever reads, materializing Drive files as `file`
//! entities so the Files module has something to browse. Local
//! version-history commits (`routes/files.rs`) are a separate, non-Drive
//! concern.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::ApiError;
use crate::error::ApiResult;
use crate::ids::now_secs;
use crate::integrations::{draft_action, proxy_call};
use crate::models::Entity;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

const PROVIDER: &str = "google-drive";

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/drive/list` - free read: proxies to `files.list`.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let body = proxy_call(&state, &workspace_id, PROVIDER, "GET", "drive/v3/files", &[], None).await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct UploadFile {
    name: String,
    /// Where the file bytes actually live (e.g. a `lifeos-vcs` blob ref) -
    /// this route only drafts the intent, it never reads or sends bytes.
    source_ref: String,
    workspace_id: Option<String>,
}

/// `POST /api/drive/upload` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Drive.
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UploadFile>,
) -> ApiResult<Json<Entity>> {
    if req.name.trim().is_empty() || req.source_ref.trim().is_empty() {
        return Err(ApiError::BadRequest("name and source_ref are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "name": req.name, "source_ref": req.source_ref });
    let entity = draft_action(&state, &workspace_id, "drive", "upload", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct ShareFile {
    entity_id: String,
    target: String,
    workspace_id: Option<String>,
}

/// `POST /api/drive/share` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Drive's permissions API.
pub async fn share(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ShareFile>,
) -> ApiResult<Json<Entity>> {
    if req.entity_id.trim().is_empty() || req.target.trim().is_empty() {
        return Err(ApiError::BadRequest("entity_id and target are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "entity_id": req.entity_id, "target": req.target });
    let entity = draft_action(&state, &workspace_id, "drive", "share", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct SyncDrive {
    workspace_id: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
}

/// `POST /api/drive/sync` - free (`drive.sync` is unconditionally free,
/// docs/MODULES.md §3.3): materializes Drive files as `file` entities.
/// Idempotent - re-syncing the same file is a no-op (`INSERT ... ON
/// CONFLICT DO NOTHING` keyed by a deterministic, workspace-scoped id).
/// No bytes are fetched or stored - `blob_ref` stays `null` for
/// Drive-sourced files until a real download/re-upload path exists; that's
/// a distinct concern from local `files.rs::commit`'s content-addressed
/// version history.
pub async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncDrive>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let max_results = req.max_results.unwrap_or(50).min(250);

    let list = proxy_call(
        &state,
        &workspace_id,
        PROVIDER,
        "GET",
        "drive/v3/files",
        &[("pageSize", &max_results.to_string())],
        None,
    )
    .await?;
    let items = list.get("files").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut synced = 0u32;
    let mut skipped = 0u32;
    for item in &items {
        let Some(drive_id) = item.get("id").and_then(Value::as_str) else { continue };
        let name = item.get("name").and_then(Value::as_str).unwrap_or("(untitled)");
        let mime = item.get("mimeType").and_then(Value::as_str).unwrap_or_default();
        let size = item.get("size").and_then(Value::as_str).unwrap_or("0");
        let parent_folder = item
            .get("parents")
            .and_then(Value::as_array)
            .and_then(|p| p.first())
            .and_then(Value::as_str)
            .unwrap_or_default();

        let attrs = json!({
            "name": name,
            "mime": mime,
            "size": size,
            "blob_ref": Value::Null,
            "drive_id": drive_id,
            "version_no": Value::Null,
            "parent_folder": parent_folder,
        });
        let entity_id = format!("file_drive_{workspace_id}_{drive_id}");
        if upsert_drive_file(&state, &workspace_id, drive_id, name, &attrs).await? {
            synced += 1;
            emit(&state.conn, &workspace_id, "file.imported", Some(&entity_id), "google-drive", &attrs).await.ok();
            // Auto-trigger ingest on file.imported (docs/MEDIA-INTELLIGENCE.md
            // §4, issue #91) - only when there's real content to ingest.
            // Drive-synced files have no download path yet (see this fn's doc
            // comment: blob_ref stays NULL), so this guard means auto-enqueue
            // doesn't fire for Drive imports today - it's forward-wired for
            // once a real download/re-upload path lands, rather than
            // enqueueing a job lifeos-ingest can't process.
            if let Some(blob_ref) = attrs.get("blob_ref").and_then(Value::as_str) {
                if let Err(e) = super::job::enqueue(
                    &state,
                    &workspace_id,
                    "ingest",
                    &json!({ "entity_id": entity_id, "blob_ref": blob_ref }),
                    0,
                )
                .await
                {
                    tracing::warn!("auto-ingest enqueue failed for {entity_id}: {e:?}");
                }
            }
        } else {
            skipped += 1;
        }
    }

    Ok(Json(json!({ "synced": synced, "skipped": skipped, "total": items.len() })))
}

async fn upsert_drive_file(
    state: &AppState,
    workspace_id: &str,
    drive_id: &str,
    name: &str,
    attrs: &Value,
) -> ApiResult<bool> {
    let id = format!("file_drive_{workspace_id}_{drive_id}");
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'files', 'file', NULL, ?3, NULL, NULL, ?4, 'google-drive', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id.clone(), workspace_id, name, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, &id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}
