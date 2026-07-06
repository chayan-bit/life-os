//! Web Push subscriptions (issue #103, `docs/PLATFORM-SYSTEMS.md`). This
//! route group is storage only: the frontend service worker subscribes via
//! the browser Push API and hands us the subscription. Actually sending a
//! push (VAPID-signed, mirroring the Telegram digest) is `lifeos-drain`'s
//! `push` module (issue #151) - a separate process/crate, so it isn't wired
//! up here; this file just stores/serves what that sender needs.

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

/// `GET /api/push/vapid-public-key` - the `applicationServerKey` the frontend
/// needs to call `pushManager.subscribe`. Read-only (docs/SECURITY.md §1: "reads
/// are free") and workspace-agnostic - the VAPID keypair is one per deployment,
/// not per tenant, mirroring `lifeos-drain`'s `LIFEOS_VAPID_PUBLIC_KEY` env var
/// exactly so both processes are always configured with the same keypair.
/// `enabled: false` (no `publicKey`) when the env var isn't set, so the
/// frontend can skip calling `subscribe` with no key rather than fail obscurely.
pub async fn vapid_public_key() -> Json<Value> {
    match std::env::var("LIFEOS_VAPID_PUBLIC_KEY").ok().filter(|v| !v.is_empty()) {
        Some(key) => Json(json!({ "enabled": true, "publicKey": key })),
        None => Json(json!({ "enabled": false, "publicKey": null })),
    }
}
