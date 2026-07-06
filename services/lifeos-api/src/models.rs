//! Serializable row shapes + libSQL row readers shared across handlers.
//!
//! Each `COLS_*` constant is the canonical SELECT column order its reader
//! expects - keep the two in lockstep.

use crate::error::ApiError;
use libsql::Row;
use serde::Serialize;

/// Drain a result set into a `Vec` using a per-row reader.
pub async fn collect<T>(
    mut rows: libsql::Rows,
    reader: impl Fn(&Row) -> Result<T, ApiError>,
) -> Result<Vec<T>, ApiError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(reader(&row)?);
    }
    Ok(out)
}

fn parse_attrs(s: Option<String>) -> serde_json::Value {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Object(Default::default()))
}

/// Parse an optional JSON text column into an optional value. Unlike
/// `parse_attrs`, a NULL column stays `None` (not an empty object) - used by
/// `annotations.anchor`, which is legitimately absent for whole-entity notes.
fn parse_opt_attrs(s: Option<String>) -> Option<serde_json::Value> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

// ---------------------------------------------------------------- entities

pub const COLS_ENTITY: &str =
    "id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at";

#[derive(Serialize)]
pub struct Entity {
    pub id: String,
    pub workspace_id: String,
    pub module: String,
    pub r#type: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub tier: Option<String>,
    pub attrs: serde_json::Value,
    pub source: Option<String>,
    pub blob_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn read_entity(row: &Row) -> Result<Entity, ApiError> {
    Ok(Entity {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        module: row.get(2)?,
        r#type: row.get(3)?,
        parent_id: row.get(4)?,
        title: row.get(5)?,
        status: row.get(6)?,
        tier: row.get(7)?,
        attrs: parse_attrs(row.get(8)?),
        source: row.get(9)?,
        blob_ref: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

// ------------------------------------------------------------------- edges

pub const COLS_EDGE: &str =
    "id, workspace_id, src_id, dst_id, dst_ref, rel, state, created_by, created_at";

#[derive(Serialize)]
pub struct Edge {
    pub id: String,
    pub workspace_id: String,
    pub src_id: String,
    pub dst_id: Option<String>,
    pub dst_ref: Option<String>,
    pub rel: String,
    pub state: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
}

pub fn read_edge(row: &Row) -> Result<Edge, ApiError> {
    Ok(Edge {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        src_id: row.get(2)?,
        dst_id: row.get(3)?,
        dst_ref: row.get(4)?,
        rel: row.get(5)?,
        state: row.get(6)?,
        created_by: row.get(7)?,
        created_at: row.get(8)?,
    })
}

// ------------------------------------------------------------------ events

pub const COLS_EVENT: &str = "id, workspace_id, ts, type, entity_id, actor, attrs, run_id, tier, model, tokens_in, tokens_out, cost, latency_ms, error, outcome, eval_score, gated";

#[derive(Serialize)]
pub struct Event {
    pub id: String,
    pub workspace_id: String,
    pub ts: i64,
    pub r#type: String,
    pub entity_id: Option<String>,
    pub actor: Option<String>,
    pub attrs: serde_json::Value,
    pub run_id: Option<String>,
    pub tier: Option<String>,
    pub model: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost: Option<f64>,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub outcome: Option<String>,
    pub eval_score: Option<f64>,
    pub gated: Option<i64>,
}

pub fn read_event(row: &Row) -> Result<Event, ApiError> {
    Ok(Event {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        ts: row.get(2)?,
        r#type: row.get(3)?,
        entity_id: row.get(4)?,
        actor: row.get(5)?,
        attrs: parse_attrs(row.get(6)?),
        run_id: row.get(7)?,
        tier: row.get(8)?,
        model: row.get(9)?,
        tokens_in: row.get(10)?,
        tokens_out: row.get(11)?,
        cost: row.get(12)?,
        latency_ms: row.get(13)?,
        error: row.get(14)?,
        outcome: row.get(15)?,
        eval_score: row.get(16)?,
        gated: row.get(17)?,
    })
}

// ------------------------------------------------------------- annotations

pub const COLS_ANNOTATION: &str =
    "id, workspace_id, entity_id, kind, body, anchor, attrs, created_by, created_at, updated_at";

/// Workspace-scoped note/highlight/question/comment/link attached to an entity
/// (or the workspace at large when `entity_id` is null). Unlike `events`,
/// annotations are mutable and are plain CRUD - not the audit log.
#[derive(Serialize)]
pub struct Annotation {
    pub id: String,
    pub workspace_id: String,
    pub entity_id: Option<String>,
    pub kind: String,
    pub body: Option<String>,
    pub anchor: Option<serde_json::Value>,
    pub attrs: serde_json::Value,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn read_annotation(row: &Row) -> Result<Annotation, ApiError> {
    Ok(Annotation {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        entity_id: row.get(2)?,
        kind: row.get(3)?,
        body: row.get(4)?,
        anchor: parse_opt_attrs(row.get(5)?),
        attrs: parse_attrs(row.get(6)?),
        created_by: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

// -------------------------------------------------------------------- jobs

pub const COLS_JOB: &str =
    "id, workspace_id, kind, payload, status, priority, run_after, claimed_by, claimed_at, attempts, created_at";

#[derive(Serialize)]
pub struct Job {
    pub id: String,
    pub workspace_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub status: String,
    pub priority: Option<i64>,
    pub run_after: Option<i64>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<i64>,
    pub attempts: Option<i64>,
    pub created_at: i64,
}

pub fn read_job(row: &Row) -> Result<Job, ApiError> {
    Ok(Job {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        kind: row.get(2)?,
        payload: parse_attrs(row.get(3)?),
        status: row.get(4)?,
        priority: row.get(5)?,
        run_after: row.get(6)?,
        claimed_by: row.get(7)?,
        claimed_at: row.get(8)?,
        attempts: row.get(9)?,
        created_at: row.get(10)?,
    })
}

// ------------------------------------------------------------- connections

/// Deliberately excludes `secret_enc` and `nango_connection_id` is the only
/// handle exposed - never a raw token (docs/SECURITY.md §1).
pub const COLS_CONNECTION: &str =
    "id, workspace_id, provider, account_handle, nango_connection_id, scopes, expires_at, status, created_at";

#[derive(Serialize)]
pub struct Connection {
    pub id: String,
    pub workspace_id: String,
    pub provider: String,
    pub account_handle: Option<String>,
    pub nango_connection_id: Option<String>,
    pub scopes: Option<String>,
    pub expires_at: Option<i64>,
    pub status: Option<String>,
    pub created_at: i64,
}

pub fn read_connection(row: &Row) -> Result<Connection, ApiError> {
    Ok(Connection {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        provider: row.get(2)?,
        account_handle: row.get(3)?,
        nango_connection_id: row.get(4)?,
        scopes: row.get(5)?,
        expires_at: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
    })
}

// ----------------------------------------------------------- module_requests

pub const COLS_MODULE_REQUEST: &str =
    "id, workspace_id, prompt, status, error, created_at, updated_at, chat_id";

#[derive(Serialize)]
pub struct ModuleRequest {
    pub id: String,
    pub workspace_id: String,
    pub prompt: String,
    /// queued -> building -> installed | failed (docs/SELF-EXTENSION.md §1, issue #76).
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Telegram chat to notify on install/failure (issue #78). `None` for
    /// API-originated requests, which have no chat behind them.
    pub chat_id: Option<String>,
}

pub fn read_module_request(row: &Row) -> Result<ModuleRequest, ApiError> {
    Ok(ModuleRequest {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        prompt: row.get(2)?,
        status: row.get(3)?,
        error: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        chat_id: row.get(7)?,
    })
}
