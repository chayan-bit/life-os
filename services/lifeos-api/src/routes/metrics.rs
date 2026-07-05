//! `GET /api/metrics` - pure SQL aggregation over `events` (+ entity counts) for
//! dashboards. Workspace-scoped. Replaces the frontend's hardcoded mock stats.

use crate::auth::resolve_workspace;
use crate::error::ApiResult;
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde_json::{json, Map, Value};

pub async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, None)?;

    // Roll-up over the run log.
    let mut rows = state
        .conn
        .query(
            "SELECT \
               COUNT(*), \
               COUNT(run_id), \
               COALESCE(SUM(tokens_in),0), \
               COALESCE(SUM(tokens_out),0), \
               COALESCE(SUM(cost),0.0), \
               COALESCE(AVG(latency_ms),0.0), \
               COALESCE(AVG(eval_score),0.0), \
               COALESCE(SUM(gated),0) \
             FROM events WHERE workspace_id = ?1",
            libsql::params![ws.clone()],
        )
        .await?;
    let r = rows.next().await?;
    let (events, runs, tin, tout, cost, lat, eval, gated) = match r {
        Some(row) => (
            row.get::<i64>(0)?,
            row.get::<i64>(1)?,
            row.get::<i64>(2)?,
            row.get::<i64>(3)?,
            row.get::<f64>(4)?,
            row.get::<f64>(5)?,
            row.get::<f64>(6)?,
            row.get::<i64>(7)?,
        ),
        None => (0, 0, 0, 0, 0.0, 0.0, 0.0, 0),
    };

    let entities = scalar_count(
        &state,
        "SELECT COUNT(*) FROM entities WHERE workspace_id = ?1",
        &ws,
    )
    .await?;
    let jobs_queued = scalar_count(
        &state,
        "SELECT COUNT(*) FROM jobs WHERE workspace_id = ?1 AND status = 'queued'",
        &ws,
    )
    .await?;
    let connections = scalar_count(
        &state,
        "SELECT COUNT(*) FROM connections WHERE workspace_id = ?1 AND status = 'active'",
        &ws,
    )
    .await?;

    Ok(Json(json!({
        "workspace_id": ws,
        "entities": entities,
        "events": events,
        "harness_runs": runs,
        "tokens_in": tin,
        "tokens_out": tout,
        "cost": cost,
        "avg_latency_ms": lat,
        "avg_eval_score": eval,
        "gated_actions": gated,
        "jobs_queued": jobs_queued,
        "active_connections": connections,
        "entities_by_module": group_count(&state, "module", "entities", &ws).await?,
        "entities_by_type": group_count(&state, "type", "entities", &ws).await?,
        "events_by_type": group_count(&state, "type", "events", &ws).await?,
        // `tier` is nullable (only #95's harness.run events set it today) -
        // COALESCE before grouping so a NULL row doesn't fail the String
        // conversion `group_count` assumes for always-non-null columns
        // like `module`/`type`/`status`.
        "events_by_tier": group_count_nullable(&state, "tier", "events", &ws).await?,
        // "phase" is only populated for events that stamp attrs.stage
        // (pipeline stage events, issues #92/#96) - most event types have
        // no phase concept yet, see docs/HARNESS-LOOP.md §3.
        "events_by_phase": group_count_json_field(&state, "attrs", "$.stage", "events", &ws).await?,
        "jobs_by_status": group_count(&state, "status", "jobs", &ws).await?,
    })))
}

async fn scalar_count(state: &AppState, sql: &str, ws: &str) -> ApiResult<i64> {
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    })
}

/// `{ "<group value>": <count>, ... }` for a column in a workspace-scoped table.
async fn group_count(state: &AppState, col: &str, table: &str, ws: &str) -> ApiResult<Value> {
    let sql = format!(
        "SELECT {col}, COUNT(*) FROM {table} WHERE workspace_id = ?1 GROUP BY {col} ORDER BY COUNT(*) DESC"
    );
    let mut rows = state.conn.query(&sql, libsql::params![ws]).await?;
    let mut map = Map::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        map.insert(key, json!(count));
    }
    Ok(Value::Object(map))
}

/// Same shape as `group_count`, for a nullable column - COALESCEs to
/// `"none"` first so a NULL row (e.g. most event `type`s don't set `tier`)
/// doesn't fail `row.get::<String>`'s NULL-to-String conversion.
async fn group_count_nullable(state: &AppState, col: &str, table: &str, ws: &str) -> ApiResult<Value> {
    let sql = format!(
        "SELECT COALESCE({col}, 'none') AS g, COUNT(*) \
         FROM {table} WHERE workspace_id = ?1 GROUP BY g ORDER BY COUNT(*) DESC"
    );
    let mut rows = state.conn.query(&sql, libsql::params![ws]).await?;
    let mut map = Map::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        map.insert(key, json!(count));
    }
    Ok(Value::Object(map))
}

/// Same shape as `group_count`, but the group key is `json_extract`ed out
/// of a JSON column instead of being a column itself (issue #97's "phase"
/// breakdown: `json_extract(attrs, '$.stage')`). Rows with no matching key
/// group under `"none"`.
async fn group_count_json_field(
    state: &AppState,
    json_col: &str,
    json_path: &str,
    table: &str,
    ws: &str,
) -> ApiResult<Value> {
    let sql = format!(
        "SELECT COALESCE(json_extract({json_col}, '{json_path}'), 'none') AS g, COUNT(*) \
         FROM {table} WHERE workspace_id = ?1 GROUP BY g ORDER BY COUNT(*) DESC"
    );
    let mut rows = state.conn.query(&sql, libsql::params![ws]).await?;
    let mut map = Map::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        map.insert(key, json!(count));
    }
    Ok(Value::Object(map))
}
