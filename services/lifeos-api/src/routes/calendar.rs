//! Google Calendar thin proxy tool (issue #53, docs/INTEGRATIONS.md). `list`
//! reads straight through Nango's proxy; `create`/`move` only ever draft
//! (docs/SECURITY.md §2) - this file has no code path that calls Calendar's
//! insert/patch APIs. `sync` (issue #57, docs/MODULES.md §3.2) is also free
//! (`cal.sync` is an unconditionally free tool) - it only ever reads,
//! materializing Calendar events as `calendar_event` entities for the
//! calendar/agenda views.

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

const PROVIDER: &str = "google-calendar";

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/calendar/list` - free read: proxies to `events.list` on the
/// primary calendar.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let body =
        proxy_call(&state, &workspace_id, PROVIDER, "GET", "calendar/v3/calendars/primary/events", &[], None).await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct CreateEvent {
    summary: String,
    start: String,
    end: String,
    workspace_id: Option<String>,
}

/// `POST /api/calendar/create` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Calendar.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateEvent>,
) -> ApiResult<Json<Entity>> {
    if req.summary.trim().is_empty() || req.start.trim().is_empty() || req.end.trim().is_empty() {
        return Err(ApiError::BadRequest("summary, start, and end are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "summary": req.summary, "start": req.start, "end": req.end });
    let entity = draft_action(&state, &workspace_id, "calendar", "create", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct MoveEvent {
    event_id: String,
    start: String,
    end: String,
    workspace_id: Option<String>,
}

/// `POST /api/calendar/move` - gated (docs/SECURITY.md §2): only creates a
/// draft entity describing the requested reschedule, never calls Calendar's
/// patch API.
pub async fn move_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MoveEvent>,
) -> ApiResult<Json<Entity>> {
    if req.event_id.trim().is_empty() || req.start.trim().is_empty() || req.end.trim().is_empty() {
        return Err(ApiError::BadRequest("event_id, start, and end are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "event_id": req.event_id, "start": req.start, "end": req.end });
    let entity = draft_action(&state, &workspace_id, "calendar", "move", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct SyncCalendar {
    workspace_id: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
}

/// `POST /api/calendar/sync` - free (`cal.sync` is unconditionally free,
/// docs/MODULES.md §3.2): materializes Calendar events as `calendar_event`
/// entities so the calendar/agenda views have something to render.
/// Idempotent - re-syncing the same event is a no-op (`INSERT ... ON
/// CONFLICT DO NOTHING` keyed by a deterministic, workspace-scoped id).
pub async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncCalendar>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let max_results = req.max_results.unwrap_or(50).min(250);

    let list = proxy_call(
        &state,
        &workspace_id,
        PROVIDER,
        "GET",
        "calendar/v3/calendars/primary/events",
        &[("maxResults", &max_results.to_string())],
        None,
    )
    .await?;
    let items = list.get("items").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut synced = 0u32;
    let mut skipped = 0u32;
    for item in &items {
        let Some(source_uid) = item.get("id").and_then(Value::as_str) else { continue };
        let title = item.get("summary").and_then(Value::as_str).unwrap_or("(no title)");
        let start = item
            .get("start")
            .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let end = item
            .get("end")
            .and_then(|e| e.get("dateTime").or_else(|| e.get("date")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let location = item.get("location").and_then(Value::as_str).unwrap_or_default();
        let recurrence = item.get("recurrence").cloned().unwrap_or_else(|| json!([]));
        let attendees = item
            .get("attendees")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.get("email").and_then(Value::as_str)).collect::<Vec<_>>())
            .unwrap_or_default();

        let attrs = json!({
            "title": title,
            "start": start,
            "end": end,
            "attendees": attendees,
            "location": location,
            "recurrence": recurrence,
            "source_uid": source_uid,
        });
        let entity_id = format!("calendar_event_{workspace_id}_{source_uid}");
        if upsert_event(&state, &workspace_id, source_uid, title, &attrs).await? {
            synced += 1;
            emit(&state.conn, &workspace_id, "cal.synced", Some(&entity_id), "google-calendar", &attrs).await.ok();
        } else {
            skipped += 1;
        }
    }

    Ok(Json(json!({ "synced": synced, "skipped": skipped, "total": items.len() })))
}

async fn upsert_event(state: &AppState, workspace_id: &str, source_uid: &str, title: &str, attrs: &Value) -> ApiResult<bool> {
    let id = format!("calendar_event_{workspace_id}_{source_uid}");
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'calendar', 'calendar_event', NULL, ?3, NULL, NULL, ?4, 'google-calendar', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id.clone(), workspace_id, title, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, &id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}
