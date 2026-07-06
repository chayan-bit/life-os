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

    let agent_turns = agent_turns_gated_split(&state, &ws).await?;
    let recovery = recovery_totals(&state, &ws).await?;
    let strategy_leaderboard = strategy_leaderboard(&state, &ws).await?;

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
        // Issue #145 (Observe dashboard): the aggregates below all read
        // fields `server/agent/loop.js::persistTurn` and
        // `server/build/commit.js::emitBuildEvent` already stamp on
        // `agent.turn` / `build.*` events - no new event kinds, no new table.
        "agent_turns_total": agent_turns.0,
        "agent_turns_gated": agent_turns.1,
        "agent_turns_allowed": agent_turns.0 - agent_turns.1,
        "turns_by_day": turns_by_day(&state, &ws).await?,
        "cache_by_result": cache_by_result(&state, &ws).await?,
        "recovery_action_count": recovery.0,
        "recovery_turns_count": recovery.1,
        "recovery_by_kind": recovery_by_kind(&state, &ws).await?,
        // Strategy optimizer leaderboard (issue #138 library, #156 wiring) -
        // flattened across every live decision group (rag.rewrite,
        // planner.prompt) so the Observe dashboard can render one table.
        "strategy_leaderboard": strategy_leaderboard,
        "recent_build_nodes": recent_build_nodes(&state, &ws).await?,
        "build_runs_by_outcome": group_count_nullable_where(
            &state, "outcome", "events", &ws, "type = 'build.completed'",
        )
        .await?,
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

/// Same shape as `group_count_nullable`, additionally scoped by a caller-supplied
/// SQL predicate (e.g. `type = 'build.completed'`) - used where the grouping
/// column is only meaningful for one event `type` among many in the shared
/// `events` table (issue #145's per-build-run outcome breakdown).
async fn group_count_nullable_where(
    state: &AppState,
    col: &str,
    table: &str,
    ws: &str,
    predicate: &str,
) -> ApiResult<Value> {
    let sql = format!(
        "SELECT COALESCE({col}, 'none') AS g, COUNT(*) \
         FROM {table} WHERE workspace_id = ?1 AND {predicate} GROUP BY g ORDER BY COUNT(*) DESC"
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

/// `(total agent.turn rows, rows with gated = 1)` - the run-log's
/// `gated`/`type` columns already carry this; `agent_turns_allowed` is just
/// the difference, computed by the caller so this stays a single query.
async fn agent_turns_gated_split(state: &AppState, ws: &str) -> ApiResult<(i64, i64)> {
    let mut rows = state
        .conn
        .query(
            "SELECT COUNT(*), COALESCE(SUM(gated), 0) FROM events \
             WHERE workspace_id = ?1 AND type = 'agent.turn'",
            libsql::params![ws],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => (row.get::<i64>(0)?, row.get::<i64>(1)?),
        None => (0, 0),
    })
}

/// Cache hit breakdown (issue #127's two-layer cache): `attrs.cache` is
/// `'exact'` | `'semantic'` on a cache-served turn, absent everywhere else -
/// `group_count_json_field` already COALESCEs a missing key to `"none"`, so
/// this is that helper scoped to `type = 'agent.turn'` only.
async fn cache_by_result(state: &AppState, ws: &str) -> ApiResult<Value> {
    let sql = "SELECT COALESCE(json_extract(attrs, '$.cache'), 'none') AS g, COUNT(*) \
               FROM events WHERE workspace_id = ?1 AND type = 'agent.turn' \
               GROUP BY g ORDER BY COUNT(*) DESC";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    let mut map = Map::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        map.insert(key, json!(count));
    }
    Ok(Value::Object(map))
}

/// `(total recovery actions, turns with at least one recovery)` - the
/// self-healing ladder (issue #129) folds a `{kind,tool,ok}` entry per action
/// into `attrs.recoveries` on the turn's own `agent.turn` event rather than
/// emitting a separate event type, so this sums `json_array_length` over that
/// column instead of grouping by a dedicated `events.type`.
async fn recovery_totals(state: &AppState, ws: &str) -> ApiResult<(i64, i64)> {
    let sql = "SELECT \
                 COALESCE(SUM(json_array_length(attrs, '$.recoveries')), 0), \
                 COALESCE(SUM(CASE WHEN json_array_length(attrs, '$.recoveries') > 0 THEN 1 ELSE 0 END), 0) \
               FROM events WHERE workspace_id = ?1 AND type = 'agent.turn'";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    Ok(match rows.next().await? {
        Some(row) => (row.get::<i64>(0)?, row.get::<i64>(1)?),
        None => (0, 0),
    })
}

/// Recovery action counts grouped by `kind` (`retry`/`arg_repair`/
/// `substitute`/`replan`, see `server/agent/recovery.js::recordRecovery`),
/// flattened out of every `agent.turn` row's `attrs.recoveries` array via
/// `json_each` - the same JSON1 extension `json_extract` already relies on
/// elsewhere in this file.
async fn recovery_by_kind(state: &AppState, ws: &str) -> ApiResult<Value> {
    let sql = "SELECT json_extract(je.value, '$.kind') AS kind, COUNT(*) \
               FROM events, json_each(events.attrs, '$.recoveries') AS je \
               WHERE events.workspace_id = ?1 AND events.type = 'agent.turn' \
               GROUP BY kind ORDER BY COUNT(*) DESC";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    let mut map = Map::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        map.insert(key, json!(count));
    }
    Ok(Value::Object(map))
}

/// Strategy optimizer leaderboard (issue #138's epsilon-greedy library, #156
/// wiring): every `agent.strategy.outcome` event carries `attrs.group` /
/// `attrs.variant` / `attrs.success` (see `server/agent/strategy.js`'s
/// `recordOutcome` - the exact event `type` string this reads). Flattened
/// across every decision group into one array, sorted the same way
/// `strategy.js::leaderboard()` sorts a single group (rate desc, then plays
/// desc) with `group` added first since this spans all of them at once.
async fn strategy_leaderboard(state: &AppState, ws: &str) -> ApiResult<Value> {
    let sql = "SELECT json_extract(attrs, '$.group') AS grp, \
                      json_extract(attrs, '$.variant') AS variant, \
                      COUNT(*) AS plays, \
                      COALESCE(SUM(json_extract(attrs, '$.success')), 0) AS successes, \
                      CAST(COALESCE(SUM(json_extract(attrs, '$.success')), 0) AS REAL) / COUNT(*) AS rate \
               FROM events \
               WHERE workspace_id = ?1 AND type = 'agent.strategy.outcome' \
               GROUP BY grp, variant \
               ORDER BY grp ASC, rate DESC, plays DESC";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(json!({
            "group": row.get::<String>(0)?,
            "variant": row.get::<String>(1)?,
            "plays": row.get::<i64>(2)?,
            "successes": row.get::<i64>(3)?,
            "rate": row.get::<f64>(4)?,
        }));
    }
    Ok(Value::Array(out))
}

/// Most recent per-node build events (issue #145): `build.node.completed` /
/// `build.node.failed` rows carry `run_id` + `attrs.node`/`attrs.tier` (see
/// `server/build/commit.js::emitBuildEvent`, `server/build/pipeline.js`).
/// Capped at 50 rows, newest first - a dashboard list, not a full audit dump
/// (`GET /api/event` already serves that).
async fn recent_build_nodes(state: &AppState, ws: &str) -> ApiResult<Value> {
    let sql = "SELECT run_id, json_extract(attrs, '$.node'), json_extract(attrs, '$.tier'), \
                      COALESCE(outcome, json_extract(attrs, '$.outcome')), type, ts \
               FROM events \
               WHERE workspace_id = ?1 AND type IN ('build.node.completed', 'build.node.failed', 'build.node.gated') \
               ORDER BY ts DESC LIMIT 50";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(json!({
            "run_id": row.get::<Option<String>>(0)?,
            "node": row.get::<Option<String>>(1)?,
            "tier": row.get::<Option<String>>(2)?,
            "outcome": row.get::<Option<String>>(3)?,
            "type": row.get::<String>(4)?,
            "ts": row.get::<i64>(5)?,
        }));
    }
    Ok(Value::Array(out))
}

/// Agent turns bucketed by calendar day (from `ts`, unix seconds - see
/// `crate::ids::now_secs`), with per-day token totals, so the dashboard can
/// chart both without pulling the raw event log client-side. Capped to the
/// most recent 30 days.
async fn turns_by_day(state: &AppState, ws: &str) -> ApiResult<Value> {
    let sql = "SELECT date(ts, 'unixepoch') AS day, COUNT(*), \
                      COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0) \
               FROM events WHERE workspace_id = ?1 AND type = 'agent.turn' \
               GROUP BY day ORDER BY day DESC LIMIT 30";
    let mut rows = state.conn.query(sql, libsql::params![ws]).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(json!({
            "day": row.get::<String>(0)?,
            "turns": row.get::<i64>(1)?,
            "tokens_in": row.get::<i64>(2)?,
            "tokens_out": row.get::<i64>(3)?,
        }));
    }
    out.reverse(); // chronological (oldest first) for a line chart
    Ok(Value::Array(out))
}
