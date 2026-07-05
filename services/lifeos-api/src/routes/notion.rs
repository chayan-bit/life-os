//! Notion thin proxy tool (issue #53, docs/INTEGRATIONS.md). `list` reads
//! straight through Nango's proxy; `create` only ever drafts
//! (docs/SECURITY.md §2) - this file has no code path that calls Notion's
//! page-create API. Two-way sync (issue #59, docs/MODULES.md §3.4): `sync`
//! is free and reads Notion pages/databases in, mirroring each page as a
//! native `note` entity linked by a `note ─mirrors→ notion_page` edge;
//! `push` is gated and only ever drafts the local note's content as a
//! pending Notion update - this file has no code path that calls Notion's
//! page-update API either, so "edits propagate back" still goes through the
//! same approve→execute queue as every other outward write.

use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::ApiError;
use crate::error::ApiResult;
use crate::audit::emit;
use crate::ids::{new_id, now_secs};
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

const PROVIDER: &str = "notion";

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/notion/list` - free read: proxies to Notion's `/v1/search`
/// (Notion models "list everything" as a search with an empty query).
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let body = proxy_call(&state, &workspace_id, PROVIDER, "POST", "v1/search", &[], Some(json!({}))).await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct CreatePage {
    parent_id: String,
    title: String,
    workspace_id: Option<String>,
}

/// `POST /api/notion/create` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Notion.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreatePage>,
) -> ApiResult<Json<Entity>> {
    if req.parent_id.trim().is_empty() || req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("parent_id and title are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "parent_id": req.parent_id, "title": req.title });
    let entity = draft_action(&state, &workspace_id, "notion", "create", attrs).await?;
    Ok(Json(entity))
}

fn extract_title(item: &Value) -> String {
    if item.get("object").and_then(Value::as_str) == Some("database") {
        return item
            .get("title")
            .and_then(Value::as_array)
            .and_then(|t| t.first())
            .and_then(|t| t.get("plain_text"))
            .and_then(Value::as_str)
            .unwrap_or("(untitled database)")
            .to_string();
    }
    item.get("properties")
        .and_then(Value::as_object)
        .and_then(|props| props.values().find(|p| p.get("type").and_then(Value::as_str) == Some("title")))
        .and_then(|p| p.get("title"))
        .and_then(Value::as_array)
        .and_then(|t| t.first())
        .and_then(|t| t.get("plain_text"))
        .and_then(Value::as_str)
        .unwrap_or("(untitled)")
        .to_string()
}

#[derive(Deserialize)]
pub struct SyncNotion {
    workspace_id: Option<String>,
}

/// `POST /api/notion/sync` - free (`note.sync` is unconditionally free,
/// docs/MODULES.md §3.4): materializes Notion's `/v1/search` results as
/// `notion_page`/`notion_db` mirror entities, plus a native `note` entity
/// per page linked by a `note ─mirrors→ notion_page` edge - the local
/// entity a user actually edits. Idempotent on both the mirror entities and
/// the edge.
pub async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncNotion>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let body = proxy_call(&state, &workspace_id, PROVIDER, "POST", "v1/search", &[], Some(json!({}))).await?;
    let items = body.get("results").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut synced = 0u32;
    let mut skipped = 0u32;
    for item in &items {
        let Some(notion_id) = item.get("id").and_then(Value::as_str) else { continue };
        let object = item.get("object").and_then(Value::as_str).unwrap_or("page");
        let title = extract_title(item);

        if object == "database" {
            let attrs = json!({ "notion_id": notion_id, "title": title });
            let id = format!("notion_db_{workspace_id}_{notion_id}");
            if upsert_mirror(&state, &workspace_id, "notion_db", &id, &title, &attrs).await? {
                synced += 1;
                emit(&state.conn, &workspace_id, "note.synced", Some(&id), "notion", &attrs).await.ok();
            } else {
                skipped += 1;
            }
            continue;
        }

        let last_edited = item.get("last_edited_time").and_then(Value::as_str).unwrap_or_default();
        let page_attrs = json!({ "notion_id": notion_id, "title": title, "last_edited_time": last_edited });
        let page_id = format!("notion_page_{workspace_id}_{notion_id}");
        let page_new = upsert_mirror(&state, &workspace_id, "notion_page", &page_id, &title, &page_attrs).await?;

        let note_attrs = json!({ "notion_id": notion_id, "mirrors": page_id });
        let note_id = format!("note_{workspace_id}_{notion_id}");
        let note_new = upsert_mirror(&state, &workspace_id, "note", &note_id, &title, &note_attrs).await?;
        ensure_mirror_edge(&state, &workspace_id, &note_id, &page_id).await?;

        if page_new || note_new {
            synced += 1;
            emit(&state.conn, &workspace_id, "note.synced", Some(&note_id), "notion", &page_attrs).await.ok();
        } else {
            skipped += 1;
        }
    }

    Ok(Json(json!({ "synced": synced, "skipped": skipped, "total": items.len() })))
}

async fn upsert_mirror(
    state: &AppState,
    workspace_id: &str,
    entity_type: &str,
    id: &str,
    title: &str,
    attrs: &Value,
) -> ApiResult<bool> {
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'notion', ?3, NULL, ?4, NULL, NULL, ?5, 'notion', NULL, ?6, ?6) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id, workspace_id, entity_type, title, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}

async fn ensure_mirror_edge(state: &AppState, workspace_id: &str, note_id: &str, page_id: &str) -> ApiResult<()> {
    let mut rows = state
        .conn
        .query(
            "SELECT 1 FROM edges WHERE workspace_id = ?1 AND src_id = ?2 AND dst_id = ?3 AND rel = 'mirrors'",
            libsql::params![workspace_id, note_id, page_id],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(());
    }
    state
        .conn
        .execute(
            "INSERT INTO edges (id, workspace_id, src_id, dst_id, dst_ref, rel, state, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, 'mirrors', 'accepted', 'notion-sync', ?5)",
            libsql::params![new_id("edg"), workspace_id, note_id, page_id, now_secs()],
        )
        .await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct PushNote {
    entity_id: String,
    workspace_id: Option<String>,
}

/// `POST /api/notion/push` - gated (docs/SECURITY.md §2): "edits propagate
/// back" only ever creates a draft entity describing the local note's
/// current content as a pending Notion page update - never calls Notion's
/// page-update API.
pub async fn push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PushNote>,
) -> ApiResult<Json<Entity>> {
    if req.entity_id.trim().is_empty() {
        return Err(ApiError::BadRequest("entity_id is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let mut rows = state
        .conn
        .query(
            "SELECT title, attrs FROM entities WHERE id = ?1 AND workspace_id = ?2 AND module = 'notion' AND type = 'note'",
            libsql::params![req.entity_id.clone(), workspace_id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(ApiError::NotFound(format!("note '{}' not found", req.entity_id)));
    };
    let title: Option<String> = row.get(0)?;
    let attrs_str: Option<String> = row.get(1)?;
    let note_attrs: Value = attrs_str.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
    let notion_id = note_attrs.get("notion_id").cloned().unwrap_or(Value::Null);

    let attrs = json!({ "entity_id": req.entity_id, "notion_id": notion_id, "title": title });
    let entity = draft_action(&state, &workspace_id, "notion", "push", attrs).await?;
    Ok(Json(entity))
}
