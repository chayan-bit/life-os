//! Web Push subscriptions (issue #103, `docs/PLATFORM-SYSTEMS.md`). Storage
//! only: the frontend service worker subscribes via the browser Push API and
//! hands us the subscription; actually sending a push (VAPID-signed,
//! mirroring the Telegram digest) needs a `web-push`-equivalent sender and
//! VAPID keypair this base doesn't wire up yet - deferred, same honesty
//! pattern as `routes/planned.rs`'s queued-but-undrained jobs.

use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SubscribeRequest {
    endpoint: String,
    keys: Value,
    workspace_id: Option<String>,
}

/// `POST /api/push/subscribe` - upserts a subscription by (workspace,
/// endpoint), so re-subscribing (a rotated push endpoint) is idempotent.
pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SubscribeRequest>,
) -> ApiResult<Json<Value>> {
    if req.endpoint.trim().is_empty() {
        return Err(ApiError::BadRequest("endpoint is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let keys_str = serde_json::to_string(&req.keys).unwrap_or_else(|_| "{}".into());
    let id = new_id("push");
    state
        .conn
        .execute(
            "INSERT INTO push_subscriptions (id, workspace_id, endpoint, keys_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (workspace_id, endpoint) DO UPDATE SET keys_json = excluded.keys_json",
            libsql::params![id.clone(), workspace_id, req.endpoint.clone(), keys_str, now_secs()],
        )
        .await?;
    Ok(Json(json!({ "subscribed": true })))
}

#[derive(Deserialize)]
pub struct UnsubscribeRequest {
    endpoint: String,
    workspace_id: Option<String>,
}

/// `POST /api/push/unsubscribe` - idempotent removal.
pub async fn unsubscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UnsubscribeRequest>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    state
        .conn
        .execute(
            "DELETE FROM push_subscriptions WHERE workspace_id = ?1 AND endpoint = ?2",
            libsql::params![workspace_id, req.endpoint],
        )
        .await?;
    Ok(Json(json!({ "unsubscribed": true })))
}
