//! `GET /api/stream/modules` - Server-Sent Events feed of module lifecycle
//! events (`module.requested`, `module.installed`) so the SPA can hot-add a
//! tab the moment a self-extension build lands, with no manual refresh and no
//! polling on the client. Implemented as a poll-the-append-only-log loop
//! rather than a pub/sub bus: `events` is already the single source of truth
//! and ULIDs are lexically time-ordered, so "new since last seen id" is a
//! cheap, correct query with no extra infrastructure.

use crate::auth::resolve_workspace;
use crate::models::{collect, read_event, COLS_EVENT};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::sse::{Event as SseEvent, Sse},
};
use futures::stream::Stream;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
pub struct StreamParams {
    workspace_id: Option<String>,
}

// `EventSource` (unlike `fetch`) cannot set custom request headers, so the
// usual X-Workspace-Id/Authorization auth path is unavailable here - the
// workspace is instead passed as a query param, same fallback precedence as
// every other route's `explicit` argument to `resolve_workspace`.
pub async fn modules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<StreamParams>,
) -> crate::error::ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let stream = async_stream::stream! {
        let mut last_id = String::new();
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let sql = format!(
                "SELECT {COLS_EVENT} FROM events \
                 WHERE workspace_id = ?1 \
                 AND type IN ('module.requested', 'module.building', 'module.installed', 'module.failed') \
                 AND id > ?2 ORDER BY id ASC LIMIT 50"
            );
            let rows = match state.conn.query(&sql, libsql::params![workspace_id.clone(), last_id.clone()]).await {
                Ok(r) => r,
                Err(e) => { tracing::warn!("module stream query failed: {e}"); continue; }
            };
            let events = match collect(rows, read_event).await {
                Ok(e) => e,
                Err(e) => { tracing::warn!("module stream decode failed: {e:?}"); continue; }
            };

            for ev in events {
                last_id = ev.id.clone();
                let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                yield Ok(SseEvent::default().event(ev.r#type.clone()).data(payload));
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}
