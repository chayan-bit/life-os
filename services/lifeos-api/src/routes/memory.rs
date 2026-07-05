//! `/api/memory/*` - the cognitive-memory surface (issues #111-#119,
//! docs/AI-MEMORY.md). Thin HTTP over the `lifeos-memory` engine:
//!
//! - `POST /api/memory/recall`   activation-scored recall (+ point-in-time)
//! - `POST /api/memory/context`  compiled, token-budgeted working memory
//! - `POST /api/memory/ingest`   append an observation event from ANY source
//!   (terminal hook, script, bot relay, UI)
//! - `POST /api/memory/sleep`    run one consolidation cycle now
//! - `POST /api/memory/rebuild`  wipe read models + replay from `events`
//! - `POST /api/memory/tier`     cold-tier sweep to the primary storage backend
//! - `GET  /api/memory/rules`    active procedural rules (system-prompt block)
//! - `GET  /api/memory/inspect`  recent memory ledger events (inspector UI)
//!
//! Every recall/skip/abstention is itself appended to `events` (the
//! AGENT-CONTROL action ledger), which is what the inspector renders - recall
//! is legible, not magic. Memory collection needs no special capture path:
//! all tiers already write `events`, and the projector folds them all.

use crate::auth::resolve_workspace;
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use lifeos_memory::{
    ask_network, compile_context, list_network_map, recall, rules_for_prompt, run_sleep_cycle,
    BudgetSpec, HeuristicModel, HeuristicPolicyLearner, HeuristicSummarizer, NoopVectorSearcher,
    RecallOutcome, RecallParams, ReplayCachedModel, Turn,
};
use serde::Deserialize;
use serde_json::{json, Value};

fn internal(e: lifeos_memory::MemoryError) -> ApiError {
    ApiError::Internal(format!("memory engine: {e}"))
}

const MEMORY_MODEL_TIMEOUT_SECS: u64 = 60;

/// Task-keyed instruction the model is asked to follow - kept identical to
/// `lifeos_drain::ai::AgentCliModel`'s wording (audit #6) so a cycle run from
/// either binary produces the same shape of summary.
fn memory_model_task_instruction(task: &str) -> &'static str {
    match task {
        "summarize" => {
            "Summarize these events into 2-4 factual sentences. No speculation, no invented \
             detail - only what the events actually say."
        }
        "reflect" => {
            "Write a short day-level reflection (2-4 factual sentences) synthesizing these \
             summaries. No speculation, no invented detail."
        }
        _ => "Summarize the following into 2-4 factual sentences. No speculation, no invented detail.",
    }
}

/// `lifeos_memory::MemoryModel` backed by a local agent CLI (env-gated by
/// `LIFEOS_MEMORY_MODEL=agent`, docs/AI-MEMORY.md §5). Intentionally
/// duplicated (not shared) from `lifeos_drain::ai::AgentCliModel`: keeping
/// `lifeos-memory` free of the `lifeos-agents` dependency rules out putting
/// the impl there, and `lifeos-api`/`lifeos-drain` are separate binaries with
/// no third crate that already depends on both without adding a new
/// cross-crate edge - so this ~40-line impl is kept in sync by hand. See that
/// file's doc comment for the twin.
///
/// ANY failure (no CLI detected, timeout, empty output) falls back to
/// `HeuristicModel`'s deterministic output for that call, so an on-demand
/// sleep cycle never fails outright.
struct AgentCliModel {
    agents: std::sync::Arc<Vec<crate::agents::DetectedAgent>>,
    cwd: Option<std::path::PathBuf>,
}

#[async_trait::async_trait]
impl lifeos_memory::MemoryModel for AgentCliModel {
    async fn complete(&self, task: &str, prompt: &str) -> Result<String, lifeos_memory::MemoryError> {
        let full_prompt = format!("{}\n\n{prompt}", memory_model_task_instruction(task));
        let opts = lifeos_agents::RunOptions {
            cwd: self.cwd.clone(),
            timeout_secs: MEMORY_MODEL_TIMEOUT_SECS,
            ..Default::default()
        };
        match lifeos_agents::run(&self.agents, &opts, &full_prompt).await {
            Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
            _ => HeuristicModel.complete(task, prompt).await,
        }
    }
}

#[derive(Deserialize)]
pub struct RecallRequest {
    query: String,
    workspace_id: Option<String>,
    top_k: Option<usize>,
    /// Point-in-time recall: "what was true at this unix ts?"
    as_of: Option<i64>,
    /// Override the multi-hop auto-detector.
    expand_graph: Option<bool>,
    /// Disable the self-RAG gate (forced inspection).
    #[serde(default)]
    no_gate: bool,
}

fn recall_params(req: &RecallRequest) -> RecallParams {
    RecallParams {
        top_k: req.top_k.unwrap_or(8).clamp(1, 50),
        as_of: req.as_of,
        expand_graph: req.expand_graph,
        use_gate: !req.no_gate,
        ..Default::default()
    }
}

/// Restore the vector lane behind LIFEOS_MEMVEC: real memvec searcher when
/// configured, else the graceful Noop (lexical-only) - the pre-#1 behavior.
fn vector_searcher(state: &AppState) -> Box<dyn lifeos_memory::VectorSearcher> {
    match crate::memvec_search::SubprocessMemvec::from_env(&state.config.derived_db_path) {
        Some(runner) => Box::new(crate::memvec_search::MemvecSearcher::new(Box::new(runner))),
        None => Box::new(NoopVectorSearcher),
    }
}

/// Re-read a node's current content + tiered state (after promote-on-access
/// restored it in the DB) so the recall RESPONSE carries the full text, not the
/// pre-promote stub.
async fn read_node_content(
    state: &AppState,
    workspace_id: &str,
    id: &str,
) -> ApiResult<Option<(String, Option<String>)>> {
    let mut rows = state
        .conn
        .query(
            "SELECT content, tiered_ref FROM memory_nodes WHERE workspace_id = ?1 AND id = ?2",
            libsql::params![workspace_id, id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some((row.get::<String>(0)?, row.get::<Option<String>>(1)?))),
        None => Ok(None),
    }
}

/// Run recall and append the ledger event that makes it inspectable.
async fn recall_logged(
    state: &AppState,
    workspace_id: &str,
    query: &str,
    params: &RecallParams,
) -> ApiResult<(RecallOutcome, Value)> {
    // Incremental projection keeps recall fresh without a per-write hook.
    lifeos_memory::project_workspace(&state.conn, workspace_id).await.map_err(internal)?;
    // Best-effort: embed any not-yet-embedded nodes so the vector lane has
    // material to search (no-op when LIFEOS_MEMVEC is unset).
    let _ = crate::memvec_search::embed_new_nodes(
        &state.conn,
        &state.config.derived_db_path,
        workspace_id,
    )
    .await;
    let searcher = vector_searcher(state);
    let mut outcome =
        recall(&state.conn, workspace_id, query, now_secs(), params, searcher.as_ref())
            .await
            .map_err(internal)?;

    // Promote-on-access (issue #117): recalled cold memories come back warm.
    if let RecallOutcome::Recalled { memories, .. } = &mut outcome {
        let tiered: Vec<String> = memories
            .iter()
            .filter(|m| m.tiered_ref.is_some())
            .map(|m| m.id.clone())
            .collect();
        if !tiered.is_empty() {
            for backend in crate::storage::read_backends(state, workspace_id).await? {
                if lifeos_memory::promote_nodes(&state.conn, workspace_id, backend.as_ref(), &tiered)
                    .await
                    .is_ok()
                {
                    break;
                }
            }
            // Reflect the just-restored full content in THIS response: recall
            // scored off the stubbed row, so the in-memory outcome still holds
            // the stub until we re-read the promoted node.
            for m in memories.iter_mut().filter(|m| m.tiered_ref.is_some()) {
                if let Some((content, tiered_ref)) =
                    read_node_content(state, workspace_id, &m.id).await?
                {
                    m.content = content;
                    m.tiered_ref = tiered_ref;
                }
            }
        }
    }

    let outcome_json = serde_json::to_value(&outcome)
        .map_err(|e| ApiError::Internal(format!("serialize outcome: {e}")))?;
    let (event_type, ledger_attrs) = match &outcome {
        RecallOutcome::Skipped { .. } => ("memory.recall.skipped", json!({ "query": query })),
        RecallOutcome::Abstained { top_activation, threshold } => (
            "memory.recall.abstained",
            json!({ "query": query, "top_activation": top_activation, "threshold": threshold }),
        ),
        RecallOutcome::Recalled { memories, expanded_graph } => (
            "memory.recalled",
            json!({
                "query": query,
                "expanded_graph": expanded_graph,
                "memories": memories.iter().map(|m| json!({
                    "id": m.id,
                    "content": m.content,
                    "source_event_ids": m.source_event_ids,
                    "breakdown": m.breakdown,
                })).collect::<Vec<_>>(),
            }),
        ),
    };
    crate::audit::emit(&state.conn, workspace_id, event_type, None, "agent", &ledger_attrs).await?;
    Ok((outcome, outcome_json))
}

pub async fn recall_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RecallRequest>,
) -> ApiResult<Json<Value>> {
    if req.query.trim().is_empty() {
        return Err(ApiError::BadRequest("query is required".into()));
    }
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let (_, outcome_json) = recall_logged(&state, &ws, &req.query, &recall_params(&req)).await?;
    Ok(Json(outcome_json))
}

#[derive(Deserialize)]
pub struct ContextRequest {
    query: String,
    workspace_id: Option<String>,
    #[serde(default)]
    recent_turns: Vec<Turn>,
    budget_tokens: Option<usize>,
    top_k: Option<usize>,
}

/// Compiled working memory for any agent tier (bot, harness, in-app agent):
/// procedural rules + activation-ranked facts + recent turns, deterministic
/// and within budget (issue #116).
pub async fn context_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ContextRequest>,
) -> ApiResult<Json<Value>> {
    if req.query.trim().is_empty() {
        return Err(ApiError::BadRequest("query is required".into()));
    }
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let params = RecallParams {
        top_k: req.top_k.unwrap_or(8).clamp(1, 50),
        ..Default::default()
    };
    let (outcome, outcome_json) = recall_logged(&state, &ws, &req.query, &params).await?;
    let memories = match outcome {
        RecallOutcome::Recalled { memories, .. } => memories,
        _ => Vec::new(),
    };
    let spec = BudgetSpec {
        total_tokens: req.budget_tokens.unwrap_or(2000).clamp(200, 32_000),
        ..Default::default()
    };
    let rules_budget = (spec.total_tokens as f64 * spec.rules_share) as usize;
    let rules = rules_for_prompt(&state.conn, &ws, rules_budget).await.map_err(internal)?;
    let compiled = compile_context(&req.recent_turns, &memories, &rules, &spec);
    Ok(Json(json!({ "context": compiled, "recall": outcome_json })))
}

#[derive(Deserialize)]
pub struct IngestRequest {
    content: String,
    /// Where this observation came from: 'terminal', 'telegram', 'web', a
    /// script name, ... - recorded as the event actor.
    source: Option<String>,
    /// Domain event type; defaults to a plain observation.
    #[serde(rename = "type")]
    event_type: Option<String>,
    entity_id: Option<String>,
    workspace_id: Option<String>,
}

/// Universal capture: anything, from anywhere, becomes an `events` row and is
/// projected into memory. This is how sources without their own event-writing
/// path (terminal hooks, cron scripts, external relays) feed the same brain
/// as the bot and the platform.
pub async fn ingest_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> ApiResult<Json<Value>> {
    if req.content.trim().is_empty() {
        return Err(ApiError::BadRequest("content is required".into()));
    }
    let event_type = req.event_type.unwrap_or_else(|| "observation.captured".to_string());
    if event_type.starts_with("memory.") {
        return Err(ApiError::BadRequest("memory.* events are reserved for the engine".into()));
    }
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !crate::db::workspace_exists(&state.conn, &ws).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{ws}'")));
    }
    let actor = req.source.unwrap_or_else(|| "user".to_string());
    let event_id = crate::audit::emit(
        &state.conn,
        &ws,
        &event_type,
        req.entity_id.as_deref(),
        &actor,
        &json!({ "text": req.content }),
    )
    .await?;
    let stats = lifeos_memory::project_workspace(&state.conn, &ws).await.map_err(internal)?;
    Ok(Json(json!({ "event_id": event_id, "projected": stats })))
}

#[derive(Deserialize)]
pub struct WorkspaceOnly {
    workspace_id: Option<String>,
}

/// One consolidation ("sleep") cycle, on demand. The model goes through the
/// BLAKE3 replay cache so a later rebuild replays its outputs verbatim.
pub async fn sleep_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WorkspaceOnly>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let now = now_secs();
    // `LIFEOS_MEMORY_MODEL=agent` routes through the local agent-CLI router
    // (audit #6); anything else (including unset) keeps the deterministic
    // extractive heuristic, unchanged from before.
    let inner_model: Box<dyn lifeos_memory::MemoryModel> =
        if std::env::var("LIFEOS_MEMORY_MODEL").as_deref() == Ok("agent") {
            Box::new(AgentCliModel {
                agents: state.agents.clone(),
                cwd: state.config.agent_cwd.clone().map(std::path::PathBuf::from),
            })
        } else {
            Box::new(HeuristicModel)
        };
    let model = ReplayCachedModel::new(inner_model.as_ref(), &state.conn, now);
    // Give the cycle the workspace's primary backend so its decay sweep can
    // actually tier cold memories out (issue #117 auto-tiering, audit #3), not
    // just count them. FTS keeps full text, so tiered nodes stay recallable.
    let backend = crate::storage::primary_backend(&state, &ws).await?;
    let report = run_sleep_cycle(
        &state.conn, &ws, &model, &HeuristicPolicyLearner, &HeuristicSummarizer, now,
        Some(backend.as_ref()),
    )
    .await
    .map_err(internal)?;
    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

/// Wipe the read models and replay the whole log (the §11 invariant path).
pub async fn rebuild_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WorkspaceOnly>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let stats = lifeos_memory::rebuild_workspace(&state.conn, &ws).await.map_err(internal)?;
    Ok(Json(serde_json::to_value(stats).unwrap_or_default()))
}

/// Cold-tier sweep: bundle cold node content to the workspace's primary
/// storage backend (issue #117). Storage-internal, so not human-gated.
pub async fn tier_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WorkspaceOnly>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let now = now_secs();
    let cold = lifeos_memory::find_cold_nodes(&state.conn, &ws, now, 30 * 86400, 0.4, 1)
        .await
        .map_err(internal)?;
    if cold.is_empty() {
        return Ok(Json(json!({ "tiered": 0, "blob_ref": null })));
    }
    let backend = crate::storage::primary_backend(&state, &ws).await?;
    let report = lifeos_memory::tier_out_cold(&state.conn, &ws, backend.as_ref(), &cold)
        .await
        .map_err(internal)?;
    crate::audit::emit(
        &state.conn,
        &ws,
        "memory.tiered",
        None,
        "harness",
        &json!({ "tiered": report.tiered, "blob_ref": report.blob_ref }),
    )
    .await?;
    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct RulesParams {
    workspace_id: Option<String>,
    budget: Option<usize>,
}

pub async fn rules_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<RulesParams>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let rules = rules_for_prompt(&state.conn, &ws, params.budget.unwrap_or(1000).clamp(50, 8000))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "rules": rules })))
}

#[derive(Deserialize)]
pub struct InspectParams {
    workspace_id: Option<String>,
    limit: Option<u32>,
}

/// The inspector feed (issue #119): recent memory ledger events - recalls
/// with full score breakdowns + provenance, abstentions, gate skips,
/// consolidation runs - straight from the append-only `events` log.
pub async fn inspect_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<InspectParams>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let limit = params.limit.unwrap_or(50).clamp(1, 500);
    let mut rows = state
        .conn
        .query(
            "SELECT id, ts, type, attrs FROM events \
             WHERE workspace_id = ?1 AND type LIKE 'memory.%' \
             ORDER BY ts DESC, id DESC LIMIT ?2",
            libsql::params![ws.clone(), limit],
        )
        .await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        let attrs_raw: Option<String> = row.get(3)?;
        entries.push(json!({
            "id": row.get::<String>(0)?,
            "ts": row.get::<i64>(1)?,
            "type": row.get::<String>(2)?,
            "attrs": attrs_raw
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null),
        }));
    }

    // Summary counts give the inspector its header stats.
    let mut counts = state
        .conn
        .query(
            "SELECT \
               (SELECT COUNT(*) FROM memory_nodes WHERE workspace_id = ?1 AND t_invalid IS NULL), \
               (SELECT COUNT(*) FROM memory_nodes WHERE workspace_id = ?1 AND t_invalid IS NOT NULL), \
               (SELECT COUNT(*) FROM memory_summaries WHERE workspace_id = ?1), \
               (SELECT COUNT(*) FROM memory_rules WHERE workspace_id = ?1 AND status = 'active')",
            libsql::params![ws],
        )
        .await?;
    let stats = match counts.next().await? {
        Some(r) => json!({
            "current_nodes": r.get::<i64>(0)?,
            "superseded_nodes": r.get::<i64>(1)?,
            "summaries": r.get::<i64>(2)?,
            "active_rules": r.get::<i64>(3)?,
        }),
        None => Value::Null,
    };
    Ok(Json(json!({ "entries": entries, "stats": stats })))
}

/// GraphRAG global lens (issue #139, docs/AGENT-CORE.md §13): the community
/// map as last rebuilt by the sleep cycle - "how is my world connected".
pub async fn network_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<WorkspaceOnly>,
) -> ApiResult<Json<Value>> {
    let ws = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let communities = list_network_map(&state.conn, &ws).await.map_err(internal)?;
    Ok(Json(json!({
        "communities": communities.into_iter().map(|c| json!({
            "id": c.id,
            "summary": c.summary,
            "size": c.size,
            "member_ids": c.member_ids,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct NetworkAskRequest {
    question: String,
    workspace_id: Option<String>,
    top_k: Option<usize>,
}

/// Map-reduce-lite over the community map: rank clusters by relevance to a
/// thematic question and return the grounded top-k. Never calls a model -
/// the agent composes the final answer from these community summaries.
pub async fn network_ask_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<NetworkAskRequest>,
) -> ApiResult<Json<Value>> {
    if req.question.trim().is_empty() {
        return Err(ApiError::BadRequest("question is required".into()));
    }
    let ws = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let top_k = req.top_k.unwrap_or(3).clamp(1, 20);
    let hits = ask_network(&state.conn, &ws, &req.question, top_k).await.map_err(internal)?;
    Ok(Json(json!({ "hits": hits })))
}
