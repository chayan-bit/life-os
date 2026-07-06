//! Live activity feed + presence (issue #150) - cheap multiplayer awareness
//! before CRDTs land: see teammates' recent actions and who is online.
//!
//! Three surfaces, all workspace-scoped:
//! - `POST /api/presence/ping` upserts one `presence` heartbeat for the caller.
//!   Deliberately a dedicated UPSERTED table, NOT an `events` row: `events` is
//!   the append-only domain log that is never pruned, and per-tab ~30s
//!   heartbeats would flood it forever (see migrations/0021_presence.sql).
//! - `GET /api/presence` lists the users seen within `PRESENCE_TTL_SECS`.
//! - `GET /api/events/stream` tails NEW domain events after a cursor over SSE,
//!   resumable via `Last-Event-ID` (or `?after=`) and filterable by module/kind.
//!   It polls the append-only log (`id > cursor`, ULIDs are time-ordered) on a
//!   short interval - the same no-new-infra pattern as `stream::modules`, and
//!   it never holds a DB handle across the poll sleep.

use crate::auth::{bearer_claims, resolve_workspace};
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::models::{collect, read_event, Event, COLS_EVENT};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use libsql::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::time::Duration;

/// How long after its last heartbeat a user is still counted "present". A tab
/// heartbeats every ~30s, so 120s tolerates a few missed beats before a user
/// drops off. Overridable via `LIFEOS_PRESENCE_TTL_SECS`.
const DEFAULT_PRESENCE_TTL_SECS: i64 = 120;

/// How often the activity stream polls the append-only log for new events. Kept
/// tight (issue #150 acceptance: two sessions see each other within ~2s) while
/// still cheap - each poll is one indexed `id > cursor` scan.
const STREAM_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Max events drained per poll, so a burst can't build an unbounded frame.
const STREAM_BATCH_LIMIT: u32 = 100;

fn presence_ttl_secs() -> i64 {
    std::env::var("LIFEOS_PRESENCE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_PRESENCE_TTL_SECS)
}

/// The caller's stable user id for presence: the verified JWT subject when one
/// is present (the frontend always sends its `key_token` after login), else a
/// single shared local identity so personal local-first mode still shows a dot
/// without inventing per-request users.
fn caller_user_id(headers: &HeaderMap, secret: &str) -> String {
    bearer_claims(headers, secret)
        .map(|c| c.sub)
        .unwrap_or_else(|| "local".to_string())
}

#[derive(Deserialize)]
pub struct PingParams {
    workspace_id: Option<String>,
}

/// `POST /api/presence/ping` - record that the caller is alive right now.
/// Idempotent UPSERT keyed on (workspace, user); the newest heartbeat wins.
/// A benign authenticated write (telemetry), never an outward/gated action.
pub async fn ping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<PingParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let user_id = caller_user_id(&headers, &state.config.jwt_secret);
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO presence (workspace_id, user_id, last_seen) VALUES (?1, ?2, ?3) \
             ON CONFLICT(workspace_id, user_id) DO UPDATE SET last_seen = excluded.last_seen",
            libsql::params![workspace_id.clone(), user_id.clone(), now],
        )
        .await?;
    Ok(Json(json!({ "user_id": user_id, "last_seen": now })))
}

/// One present user, as returned by `GET /api/presence`.
#[derive(serde::Serialize)]
pub struct PresentUser {
    pub user_id: String,
    pub last_seen: i64,
}

/// The users active in `workspace_id` within the last `ttl_secs` relative to
/// `now`. Factored out (and taking `now`/`ttl` explicitly) so the TTL cutoff is
/// unit-tested deterministically without sleeping.
pub async fn active_users(
    conn: &Connection,
    workspace_id: &str,
    now: i64,
    ttl_secs: i64,
) -> ApiResult<Vec<PresentUser>> {
    let cutoff = now - ttl_secs;
    let rows = conn
        .query(
            "SELECT user_id, last_seen FROM presence \
             WHERE workspace_id = ?1 AND last_seen >= ?2 ORDER BY last_seen DESC",
            libsql::params![workspace_id.to_string(), cutoff],
        )
        .await?;
    collect(rows, |row| {
        Ok(PresentUser {
            user_id: row.get(0)?,
            last_seen: row.get(1)?,
        })
    })
    .await
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/presence` - who is online in the workspace right now.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let ttl = presence_ttl_secs();
    let users = active_users(&state.conn, &workspace_id, now_secs(), ttl).await?;
    Ok(Json(json!({ "ttl_secs": ttl, "present": users })))
}

/// Fetch the domain events after `after_id` for the activity feed, workspace-
/// scoped and optionally filtered by `kind` (the event `type`) and `module`.
///
/// `module` filters by the subject entity's module, so it INNER JOINs `entities`
/// (events carry no module column; module-scoped feeds only want events about
/// entities anyway). Events are ULID-keyed, so `id > after_id` over the PK index
/// is the correct, cheap "everything newer than the cursor" scan. Factored out
/// of the SSE handler so the since-cursor semantics are unit-tested directly.
pub async fn events_since(
    conn: &Connection,
    workspace_id: &str,
    after_id: &str,
    module: Option<&str>,
    kind: Option<&str>,
    limit: u32,
) -> ApiResult<Vec<Event>> {
    // Table-qualify the shared column list so it survives the optional JOIN.
    let cols = COLS_EVENT
        .split(", ")
        .map(|c| format!("e.{c}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut sql = format!("SELECT {cols} FROM events e");
    if module.is_some() {
        sql.push_str(" JOIN entities en ON en.id = e.entity_id");
    }
    sql.push_str(" WHERE e.workspace_id = ?1 AND e.id > ?2");

    let mut binds: Vec<String> = vec![workspace_id.to_string(), after_id.to_string()];
    let mut next = 3;
    if let Some(k) = kind {
        sql.push_str(&format!(" AND e.type = ?{next}"));
        binds.push(k.to_string());
        next += 1;
    }
    if let Some(m) = module {
        sql.push_str(&format!(" AND en.module = ?{next}"));
        binds.push(m.to_string());
    }
    let limit = limit.min(500);
    sql.push_str(&format!(" ORDER BY e.id ASC LIMIT {limit}"));

    let rows = conn.query(&sql, libsql::params_from_iter(binds)).await?;
    collect(rows, read_event).await
}

/// The most recent event id in a workspace, or `""` if there are none yet. Used
/// to seed a fresh stream (no cursor) so it emits only genuinely NEW events
/// rather than replaying the whole log.
async fn latest_event_id(conn: &Connection, workspace_id: &str) -> ApiResult<String> {
    let mut rows = conn
        .query(
            "SELECT id FROM events WHERE workspace_id = ?1 ORDER BY id DESC LIMIT 1",
            libsql::params![workspace_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => String::new(),
    })
}

#[derive(Deserialize)]
pub struct StreamParams {
    workspace_id: Option<String>,
    /// Resume cursor: emit events strictly after this id. Superseded by the
    /// `Last-Event-ID` header when the client sends one (the SSE resume norm).
    after: Option<String>,
    module: Option<String>,
    kind: Option<String>,
}

/// `GET /api/events/stream` - Server-Sent Events tail of the workspace's
/// append-only domain log, for the live activity feed.
///
/// The frontend reaches this with `fetch` + a readable stream (not `EventSource`,
/// which cannot send `Authorization`), so the usual bearer/`X-Workspace-Id` auth
/// path works unchanged - identity is resolved from real request headers, no
/// token is ever smuggled through the query string. Resume is via `Last-Event-ID`
/// (or the `?after=` fallback); a fresh, cursorless connection starts from the
/// current tip so it streams only new events.
pub async fn events_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<StreamParams>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    // `Last-Event-ID` (browser/standard resume header) wins over `?after=`.
    let header_cursor = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let query_cursor = params.after.filter(|s| !s.is_empty());
    let mut last_id = match header_cursor.or(query_cursor) {
        Some(c) => c,
        // No cursor: seed from the tip so we don't replay the whole log.
        None => latest_event_id(&state.conn, &workspace_id).await?,
    };

    let module = params.module.filter(|s| !s.is_empty());
    let kind = params.kind.filter(|s| !s.is_empty());

    let stream = async_stream::stream! {
        loop {
            tokio::time::sleep(STREAM_POLL_INTERVAL).await;

            let events = match events_since(
                &state.conn,
                &workspace_id,
                &last_id,
                module.as_deref(),
                kind.as_deref(),
                STREAM_BATCH_LIMIT,
            )
            .await
            {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!("activity stream query failed: {e:?}");
                    continue;
                }
            };

            for ev in events {
                last_id = ev.id.clone();
                let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                // `.id()` sets the SSE id so a reconnect resumes past it.
                yield Ok(SseEvent::default().id(ev.id).event(ev.r#type).data(payload));
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
