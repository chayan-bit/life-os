//! Gmail thin proxy tool (issue #53, docs/INTEGRATIONS.md). `list` reads
//! straight through Nango's proxy; `send` only ever drafts (docs/SECURITY.md
//! §2) - this file has no code path that calls Gmail's send API. `sync`
//! (issue #56, docs/MODULES.md §3.1) is also free (`gmail.sync` is an
//! unconditionally free tool) - it only ever reads, materializing Gmail
//! messages as `email`/`email_thread` entities for the triage board.

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

const PROVIDER: &str = "google-mail";

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// `GET /api/gmail/list` - free read: proxies to Gmail's `messages.list`.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let mut query = Vec::new();
    if let Some(q) = &params.q {
        query.push(("q", q.as_str()));
    }
    let body = proxy_call(&state, &workspace_id, PROVIDER, "GET", "gmail/v1/users/me/messages", &query, None).await?;
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct SendGmail {
    to: String,
    subject: String,
    #[serde(default)]
    body: String,
    workspace_id: Option<String>,
}

/// `POST /api/gmail/send` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never calls Gmail.
pub async fn send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendGmail>,
) -> ApiResult<Json<Entity>> {
    if req.to.trim().is_empty() || req.subject.trim().is_empty() {
        return Err(ApiError::BadRequest("to and subject are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "to": req.to, "subject": req.subject, "body": req.body });
    let entity = draft_action(&state, &workspace_id, "gmail", "send", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct SyncGmail {
    workspace_id: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
}

fn header_value(headers: &[Value], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|h| h.get("name").and_then(Value::as_str).map(|n| n.eq_ignore_ascii_case(name)) == Some(true))
        .and_then(|h| h.get("value"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `POST /api/gmail/sync` - free (`gmail.sync` is unconditionally free,
/// docs/MODULES.md §3.1): materializes Gmail messages as `email` +
/// `email_thread` entities so the triage board has something to render.
/// Idempotent - re-syncing the same message is a no-op (`INSERT ... ON
/// CONFLICT DO NOTHING` keyed by a deterministic, workspace-scoped id).
pub async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncGmail>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let max_results = req.max_results.unwrap_or(20).min(100);

    let list = proxy_call(
        &state,
        &workspace_id,
        PROVIDER,
        "GET",
        "gmail/v1/users/me/messages",
        &[("maxResults", &max_results.to_string())],
        None,
    )
    .await?;
    let stubs = list.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut synced = 0u32;
    let mut skipped = 0u32;
    for stub in &stubs {
        let Some(gmail_id) = stub.get("id").and_then(Value::as_str) else { continue };

        let detail = proxy_call(
            &state,
            &workspace_id,
            PROVIDER,
            "GET",
            &format!("gmail/v1/users/me/messages/{gmail_id}"),
            &[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "To"),
                ("metadataHeaders", "Subject"),
            ],
            None,
        )
        .await?;

        let gmail_thread_id = detail.get("threadId").and_then(Value::as_str).unwrap_or(gmail_id);
        let mail_headers = detail
            .get("payload")
            .and_then(|p| p.get("headers"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let from = header_value(&mail_headers, "From");
        let to = header_value(&mail_headers, "To");
        let subject = header_value(&mail_headers, "Subject").unwrap_or_else(|| "(no subject)".into());
        let snippet = detail.get("snippet").and_then(Value::as_str).unwrap_or_default();
        let label_ids = detail.get("labelIds").cloned().unwrap_or_else(|| json!([]));
        let unread = label_ids.as_array().is_some_and(|ls| ls.iter().any(|l| l == "UNREAD"));

        let thread_entity_id = format!("email_thread_{workspace_id}_{gmail_thread_id}");
        if upsert_thread(&state, &workspace_id, gmail_thread_id, &subject).await? {
            emit(
                &state.conn,
                &workspace_id,
                "email.received",
                Some(&thread_entity_id),
                "gmail",
                &json!({ "gmail_thread_id": gmail_thread_id }),
            )
            .await
            .ok();
        }

        let attrs = json!({
            "gmail_id": gmail_id,
            "gmail_thread_id": gmail_thread_id,
            "from": from,
            "to": to.map(|t| vec![t]).unwrap_or_default(),
            "subject": subject,
            "snippet": snippet,
            "label_ids": label_ids,
            "unread": unread,
        });
        let email_entity_id = format!("email_{workspace_id}_{gmail_id}");
        if upsert_email(&state, &workspace_id, gmail_id, &subject, &attrs).await? {
            synced += 1;
            emit(&state.conn, &workspace_id, "email.received", Some(&email_entity_id), "gmail", &attrs).await.ok();
        } else {
            skipped += 1;
        }
    }

    Ok(Json(json!({ "synced": synced, "skipped": skipped, "total": stubs.len() })))
}

async fn upsert_email(state: &AppState, workspace_id: &str, gmail_id: &str, title: &str, attrs: &Value) -> ApiResult<bool> {
    let id = format!("email_{workspace_id}_{gmail_id}");
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    // `status` (not `attrs.triage_status`) drives the triage board - it's the
    // one field GenericBoard's drag-to-move PATCHes (frontend/src/core/
    // renderers/GenericBoard.jsx), so the triage state must live in the
    // top-level column for a column move to actually persist.
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'email', 'email', NULL, ?3, 'now', NULL, ?4, 'gmail', NULL, ?5, ?5) \
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

async fn upsert_thread(state: &AppState, workspace_id: &str, gmail_thread_id: &str, subject: &str) -> ApiResult<bool> {
    let id = format!("email_thread_{workspace_id}_{gmail_thread_id}");
    let now = now_secs();
    let attrs_str = json!({ "gmail_thread_id": gmail_thread_id }).to_string();
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'email', 'email_thread', NULL, ?3, NULL, NULL, ?4, 'gmail', NULL, ?5, ?5) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id.clone(), workspace_id, subject, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, &id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}
