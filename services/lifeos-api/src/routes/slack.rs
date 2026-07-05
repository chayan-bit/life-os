//! Slack thin proxy tool (issue #53, docs/INTEGRATIONS.md). `list` reads
//! straight through Nango's proxy; `post` only ever drafts (docs/SECURITY.md
//! §2) - this file has no code path that calls Slack's `chat.postMessage`.
//! `sync` (issue #60, docs/MODULES.md §3.5) is also free - it only ever
//! reads, materializing Slack channels/messages as entities so Slack works
//! as a second capture/notify surface alongside Telegram.

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

const PROVIDER: &str = "slack";

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/slack/list` - free read: proxies to `conversations.list`.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let body = proxy_call(&state, &workspace_id, PROVIDER, "GET", "conversations.list", &[], None).await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct PostMessage {
    channel: String,
    text: String,
    workspace_id: Option<String>,
}

/// `POST /api/slack/post` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Slack.
pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PostMessage>,
) -> ApiResult<Json<Entity>> {
    if req.channel.trim().is_empty() || req.text.trim().is_empty() {
        return Err(ApiError::BadRequest("channel and text are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "channel": req.channel, "text": req.text });
    let entity = draft_action(&state, &workspace_id, "slack", "post", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct SyncSlack {
    workspace_id: Option<String>,
    #[serde(default)]
    max_channels: Option<u32>,
    #[serde(default)]
    max_messages_per_channel: Option<u32>,
}

/// `POST /api/slack/sync` - free (`slack.sync` is unconditionally free,
/// docs/MODULES.md §3.5): materializes Slack channels as `channel`
/// entities and their recent history as `message` entities. Idempotent -
/// re-syncing the same channel/message is a no-op (`INSERT ... ON CONFLICT
/// DO NOTHING` keyed by a deterministic, workspace-scoped id).
pub async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncSlack>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let max_channels = req.max_channels.unwrap_or(20).min(100);
    let max_messages = req.max_messages_per_channel.unwrap_or(50).min(200);

    let list =
        proxy_call(&state, &workspace_id, PROVIDER, "GET", "conversations.list", &[("limit", &max_channels.to_string())], None)
            .await?;
    let channels = list.get("channels").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut synced = 0u32;
    let mut skipped = 0u32;
    let mut total = 0u32;
    for channel in &channels {
        let Some(channel_id) = channel.get("id").and_then(Value::as_str) else { continue };
        let channel_name = channel.get("name").and_then(Value::as_str).unwrap_or("(unnamed)");

        let channel_attrs = json!({ "channel_id": channel_id, "name": channel_name });
        let channel_entity_id = format!("channel_{workspace_id}_{channel_id}");
        if upsert_channel(&state, &workspace_id, channel_id, channel_name, &channel_attrs).await? {
            synced += 1;
            emit(&state.conn, &workspace_id, "message.captured", Some(&channel_entity_id), "slack", &channel_attrs)
                .await
                .ok();
        } else {
            skipped += 1;
        }

        let history = proxy_call(
            &state,
            &workspace_id,
            PROVIDER,
            "GET",
            "conversations.history",
            &[("channel", channel_id), ("limit", &max_messages.to_string())],
            None,
        )
        .await?;
        let messages = history.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
        total += messages.len() as u32;

        for message in &messages {
            let Some(ts) = message.get("ts").and_then(Value::as_str) else { continue };
            let user = message.get("user").and_then(Value::as_str).unwrap_or_default();
            let text = message.get("text").and_then(Value::as_str).unwrap_or_default();

            let msg_attrs = json!({ "channel_id": channel_id, "user": user, "text": text, "ts": ts });
            let msg_entity_id = format!("message_{workspace_id}_{channel_id}_{ts}");
            if upsert_message(&state, &workspace_id, channel_id, ts, text, &msg_attrs).await? {
                synced += 1;
                emit(&state.conn, &workspace_id, "message.captured", Some(&msg_entity_id), "slack", &msg_attrs)
                    .await
                    .ok();
            } else {
                skipped += 1;
            }
        }
    }

    Ok(Json(json!({ "synced": synced, "skipped": skipped, "total": total + channels.len() as u32 })))
}

async fn upsert_channel(
    state: &AppState,
    workspace_id: &str,
    channel_id: &str,
    name: &str,
    attrs: &Value,
) -> ApiResult<bool> {
    let id = format!("channel_{workspace_id}_{channel_id}");
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'slack', 'channel', NULL, ?3, NULL, NULL, ?4, 'slack', NULL, ?5, ?5) \
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

async fn upsert_message(
    state: &AppState,
    workspace_id: &str,
    channel_id: &str,
    ts: &str,
    text: &str,
    attrs: &Value,
) -> ApiResult<bool> {
    let id = format!("message_{workspace_id}_{channel_id}_{ts}");
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'slack', 'message', NULL, ?3, NULL, NULL, ?4, 'slack', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id.clone(), workspace_id, text, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, &id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}
