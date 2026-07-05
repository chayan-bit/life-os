//! WhatsApp via self-hosted GOWA (issue #52, docs/INTEGRATIONS.md). QR-pair
//! flow + inbound webhook capture. There is no send route here - sending is
//! gated (docs/SECURITY.md §2): `send` only creates a draft entity, and
//! `whatsapp::WhatsAppClient` has no send method for it to call even if it
//! wanted to.

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::{index_entity, workspace_exists};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{read_connection, read_entity, Connection, Entity, COLS_ENTITY};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn whatsapp_or_501(state: &AppState) -> ApiResult<&dyn crate::whatsapp::WhatsAppClient> {
    state
        .whatsapp
        .as_deref()
        .ok_or_else(|| ApiError::NotImplemented("GOWA is not configured - see docs/MANUAL-SETUP.md #52".into()))
}

#[derive(Deserialize)]
pub struct StartSession {
    workspace_id: Option<String>,
}

/// `POST /api/connections/whatsapp/session` - register a GOWA device slot
/// keyed by `workspace_id`, so inbound webhook events (`session_id`) route
/// straight back to this workspace with no lookup table needed.
pub async fn start_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartSession>,
) -> ApiResult<Json<Connection>> {
    let whatsapp = whatsapp_or_501(&state)?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    whatsapp.create_device(&workspace_id).await?;

    let id = new_id("conn");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO connections \
             (id, workspace_id, provider, account_handle, nango_connection_id, secret_enc, scopes, expires_at, status, created_at) \
             VALUES (?1, ?2, 'whatsapp', ?3, NULL, NULL, 'inbound-only', NULL, 'pending', ?4)",
            libsql::params![id.clone(), workspace_id.clone(), workspace_id.clone(), now],
        )
        .await?;

    emit(&state.conn, &workspace_id, "connection.connected", Some(&id), "api", &json!({ "provider": "whatsapp" })).await?;

    fetch_connection(&state, &workspace_id, &id).await
}

#[derive(Deserialize)]
pub struct WorkspaceParam {
    workspace_id: Option<String>,
}

/// `GET /api/connections/whatsapp/qr` - a link to the current QR code image
/// for the device-pairing scan (served by GOWA itself, local network only).
pub async fn qr(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<WorkspaceParam>,
) -> ApiResult<Json<Value>> {
    let whatsapp = whatsapp_or_501(&state)?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let qr_link = whatsapp.login_qr(&workspace_id).await?;
    Ok(Json(json!({ "qr_link": qr_link })))
}

/// `GET /api/connections/whatsapp/status` - poll pairing status; flips the
/// connection to `active` once GOWA reports the device as `logged_in`.
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<WorkspaceParam>,
) -> ApiResult<Json<Value>> {
    let whatsapp = whatsapp_or_501(&state)?;
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let connected = whatsapp.status(&workspace_id).await?;

    if connected {
        state
            .conn
            .execute(
                "UPDATE connections SET status = 'active' \
                 WHERE workspace_id = ?1 AND provider = 'whatsapp' AND status = 'pending'",
                libsql::params![workspace_id.clone()],
            )
            .await?;
    }

    Ok(Json(json!({ "connected": connected, "status": if connected { "active" } else { "pending" } })))
}

/// `POST /api/webhooks/whatsapp` - inbound event receiver. Verifies
/// `X-Hub-Signature-256: sha256=<hex>` (HMAC-SHA256 of the raw body) before
/// trusting anything in the payload - this endpoint is unauthenticated
/// otherwise, reachable by anyone who has the URL.
pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    let hmac_key = state
        .config
        .gowa_webhook_secret
        .as_ref()
        .ok_or_else(|| ApiError::NotImplemented("GOWA_WEBHOOK_SECRET is not set - see docs/MANUAL-SETUP.md #52".into()))?;

    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("sha256="))
        .ok_or_else(|| ApiError::Unauthorized("missing or malformed x-hub-signature-256".into()))?;
    let signature_bytes =
        hex::decode(signature).map_err(|_| ApiError::Unauthorized("x-hub-signature-256 is not valid hex".into()))?;

    let mut mac = HmacSha256::new_from_slice(hmac_key.as_bytes())
        .map_err(|_| ApiError::Internal("invalid webhook hmac key".into()))?;
    mac.update(&body);
    if mac.verify_slice(&signature_bytes).is_err() {
        return Err(ApiError::Unauthorized("x-hub-signature-256 does not match".into()));
    }

    #[derive(Deserialize)]
    struct Envelope {
        event: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        payload: Value,
    }
    let envelope: Envelope =
        serde_json::from_slice(&body).map_err(|_| ApiError::BadRequest("malformed webhook payload".into()))?;
    let workspace_id = envelope
        .session_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::BadRequest("webhook payload missing session_id".into()))?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let is_echo = envelope.payload.get("is_from_me").and_then(Value::as_bool).unwrap_or(false);
    if envelope.event == "message" && !is_echo {
        let title = envelope.payload.get("body").and_then(Value::as_str).map(|s| s.chars().take(120).collect::<String>());

        let id = new_id("ent");
        let now = now_secs();
        let attrs_str = serde_json::to_string(&envelope.payload).unwrap_or_else(|_| "{}".into());
        state
            .conn
            .execute(
                "INSERT INTO entities \
                 (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
                 VALUES (?1, ?2, 'integrations', 'whatsapp_message', NULL, ?3, NULL, NULL, ?4, 'gowa', NULL, ?5, ?6)",
                libsql::params![id.clone(), workspace_id.clone(), title, attrs_str, now, now],
            )
            .await?;
        emit(&state.conn, &workspace_id, "whatsapp.message.received", Some(&id), "gowa", &json!({})).await?;
        if let Err(e) = index_entity(&state.conn, &id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }

    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SendWhatsApp {
    to: String,
    message: String,
    workspace_id: Option<String>,
}

/// `POST /api/whatsapp/send` - a gated action (docs/SECURITY.md §2): creates
/// a draft entity awaiting approval. Never calls GOWA - there is no path
/// from this handler to an actual send.
pub async fn send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendWhatsApp>,
) -> ApiResult<Json<Entity>> {
    if req.to.trim().is_empty() || req.message.trim().is_empty() {
        return Err(ApiError::BadRequest("to and message are required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("ent");
    let now = now_secs();
    let attrs = json!({ "to": req.to, "message": req.message });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'integrations', 'whatsapp_send', NULL, ?3, 'pending_approval', NULL, ?4, 'api', NULL, ?5, ?6)",
            libsql::params![id.clone(), workspace_id.clone(), req.to.clone(), attrs_str, now, now],
        )
        .await?;
    emit(&state.conn, &workspace_id, "whatsapp.send.drafted", Some(&id), "api", &attrs).await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }

    let mut rows = state
        .conn
        .query(&format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1"), libsql::params![id.clone()])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_entity(&row)?)),
        None => Err(ApiError::Internal("draft entity vanished after insert".into())),
    }
}

async fn fetch_connection(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Connection>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {} FROM connections WHERE id = ?1 AND workspace_id = ?2", crate::models::COLS_CONNECTION),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_connection(&row)?)),
        None => Err(ApiError::NotFound(format!("connection '{id}' not found"))),
    }
}
