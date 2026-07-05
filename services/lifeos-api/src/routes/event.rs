//! `/api/event` - the append-only domain + harness run log.
//!
//! Only append (POST) and read (GET) exist. There is deliberately no update or
//! delete route anywhere in the API: history cannot be rewritten, even by the
//! owner token. Undefined methods on this path resolve to HTTP 405.

use crate::auth::resolve_workspace;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{collect, read_event, Event, COLS_EVENT};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateEvent {
    r#type: String,
    entity_id: Option<String>,
    actor: Option<String>,
    #[serde(default)]
    attrs: serde_json::Value,
    // Optional harness run-log fields (events doubles as the run log).
    run_id: Option<String>,
    tier: Option<String>,
    model: Option<String>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    cost: Option<f64>,
    latency_ms: Option<i64>,
    error: Option<String>,
    outcome: Option<String>,
    eval_score: Option<f64>,
    gated: Option<i64>,
    workspace_id: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateEvent>,
) -> ApiResult<Json<Event>> {
    if req.r#type.trim().is_empty() {
        return Err(ApiError::BadRequest("type is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let id = new_id("evt");
    let attrs_str = if req.attrs.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&req.attrs).unwrap_or_else(|_| "{}".into())
    };

    state
        .conn
        .execute(
            "INSERT INTO events \
             (id, workspace_id, ts, type, entity_id, actor, attrs, run_id, tier, model, \
              tokens_in, tokens_out, cost, latency_ms, error, outcome, eval_score, gated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                now_secs(),
                req.r#type,
                req.entity_id,
                req.actor.unwrap_or_else(|| "api".into()),
                attrs_str,
                req.run_id,
                req.tier,
                req.model,
                req.tokens_in,
                req.tokens_out,
                req.cost,
                req.latency_ms,
                req.error,
                req.outcome,
                req.eval_score,
                req.gated.unwrap_or(0)
            ],
        )
        .await?;

    let mut rows = state
        .conn
        .query(
            &format!("SELECT {COLS_EVENT} FROM events WHERE id = ?1"),
            libsql::params![id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| ApiError::Internal("event vanished".into()))?;
    Ok(Json(read_event(&row)?))
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    r#type: Option<String>,
    entity_id: Option<String>,
    // Filters by the harness run-log `run_id` column (issue #92: a pipeline
    // run's job id, so the frontend can poll all of one run's stage events
    // before it knows the `pipeline_run` entity id `process_pipeline_job`
    // creates internally).
    run_id: Option<String>,
    limit: Option<u32>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Event>>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;

    let mut sql = format!("SELECT {COLS_EVENT} FROM events WHERE workspace_id = ?1");
    let mut binds: Vec<String> = vec![workspace_id];
    let mut next = 2;
    for (col, val) in
        [("type", &params.r#type), ("entity_id", &params.entity_id), ("run_id", &params.run_id)]
    {
        if let Some(v) = val {
            sql.push_str(&format!(" AND {col} = ?{next}"));
            binds.push(v.clone());
            next += 1;
        }
    }
    let limit = params.limit.unwrap_or(200).min(2000);
    sql.push_str(&format!(" ORDER BY ts DESC LIMIT {limit}"));

    let rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    Ok(Json(collect(rows, read_event).await?))
}
