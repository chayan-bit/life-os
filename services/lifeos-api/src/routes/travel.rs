//! Travel module (issue #62, docs/MODULES.md §3.7). Trip/leg/place are
//! plain user-authored entities - created through the generic `POST
//! /api/entity` like Trading/Social/Marketing, no bespoke route needed. Only
//! two things here are genuinely special: `book` is gated (an actual flight/
//! hotel purchase is an irreversible outward action, so it only ever
//! creates a `pending_approval` draft via `integrations::draft_action`,
//! never touching `state.browser` - the same structural guarantee every
//! other gated write in this repo has), and `parse_emails` is free (it only
//! reads already-synced `email` entities and derives `booking` entities
//! locally, no external call).

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::index_entity;
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::integrations::draft_action;
use crate::models::Entity;
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct Book {
    trip_id: String,
    provider: String,
    item: String,
    cost: Option<f64>,
    workspace_id: Option<String>,
}

/// `POST /api/travel/book` - gated: only ever drafts, structurally cannot
/// reach a real browser session (mirrors `routes/browser.rs::act`).
pub async fn book(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<Book>) -> ApiResult<Json<Entity>> {
    if req.trip_id.trim().is_empty() || req.provider.trim().is_empty() || req.item.trim().is_empty() {
        return Err(ApiError::BadRequest("trip_id, provider, and item are required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let attrs = json!({ "trip_id": req.trip_id, "provider": req.provider, "item": req.item, "cost": req.cost });
    let entity = draft_action(&state, &workspace_id, "travel", "book", attrs).await?;
    Ok(Json(entity))
}

/// Naive, deterministic keyword match - real AI-driven extraction is
/// deferred (same "real but simple, not AI-powered" precedent as reading.rs
/// §3.6's naive_summary/link_topics).
const BOOKING_KEYWORDS: &[&str] =
    &["flight", "itinerary", "confirmation", "reservation", "hotel", "booking", "e-ticket", "boarding pass"];

fn looks_like_booking(subject: &str, snippet: &str) -> bool {
    let haystack = format!("{subject} {snippet}").to_lowercase();
    BOOKING_KEYWORDS.iter().any(|k| haystack.contains(k))
}

#[derive(Deserialize)]
pub struct ParseEmails {
    workspace_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ParseEmailsResult {
    created: i64,
    skipped: i64,
    total: i64,
}

/// `POST /api/travel/parse-emails` - free: scans already-synced `email`
/// entities (issue #56) for booking-shaped text and idempotently derives
/// `booking` entities. No external call - the emails are already local.
pub async fn parse_emails(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ParseEmails>,
) -> ApiResult<Json<ParseEmailsResult>> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let mut rows = state
        .conn
        .query(
            "SELECT id, attrs FROM entities WHERE workspace_id = ?1 AND module = 'email' AND type = 'email'",
            libsql::params![workspace_id.clone()],
        )
        .await?;

    let mut emails = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let attrs_str: Option<String> = row.get(1)?;
        let attrs: Value = attrs_str.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        emails.push((id, attrs));
    }

    let total = emails.len() as i64;
    let mut created = 0i64;
    let mut skipped = 0i64;

    for (email_id, attrs) in emails {
        let subject = attrs.get("subject").and_then(Value::as_str).unwrap_or("");
        let snippet = attrs.get("snippet").and_then(Value::as_str).unwrap_or("");
        if !looks_like_booking(subject, snippet) {
            continue;
        }

        let booking_id = format!("booking_{workspace_id}_{}", lifeos_vcs::hash_bytes(email_id.as_bytes()));
        let confirmation = extract_confirmation(subject, snippet);
        let booking_attrs = json!({
            "provider": Value::Null,
            "confirmation": confirmation,
            "cost": Value::Null,
            "file_ref": Value::Null,
            "source_email_id": email_id,
        });
        if upsert_booking(&state, &workspace_id, &booking_id, &booking_attrs).await? {
            emit(&state.conn, &workspace_id, "booking.added", Some(&booking_id), "api", &booking_attrs).await.ok();
            created += 1;
        } else {
            skipped += 1;
        }
    }

    Ok(Json(ParseEmailsResult { created, skipped, total }))
}

/// Pulls a confirmation-shaped alphanumeric token (6+ chars, at least one
/// digit) out of the subject/snippet if one exists - a naive heuristic, not
/// a guarantee.
fn extract_confirmation(subject: &str, snippet: &str) -> Value {
    for word in format!("{subject} {snippet}").split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if cleaned.len() >= 6 && cleaned.chars().any(|c| c.is_ascii_digit()) && cleaned.chars().any(|c| c.is_ascii_alphabetic()) {
            return Value::String(cleaned);
        }
    }
    Value::Null
}

async fn upsert_booking(state: &AppState, workspace_id: &str, id: &str, attrs: &Value) -> ApiResult<bool> {
    let now = now_secs();
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    let rows_affected = state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'travel', 'booking', NULL, NULL, NULL, NULL, ?3, 'api', NULL, ?4, ?4) \
             ON CONFLICT(id) DO NOTHING",
            libsql::params![id, workspace_id, attrs_str, now],
        )
        .await?;
    if rows_affected > 0 {
        if let Err(e) = index_entity(&state.conn, id).await {
            tracing::warn!("derived index upsert failed for {id}: {e}");
        }
    }
    Ok(rows_affected > 0)
}
