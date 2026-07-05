//! `/api/search` - hybrid recall over the derived DB.
//!
//! Lexical (FTS5 over `d.entities_fts`) is always available and owned by this
//! service. Semantic neighbours come from `server/memvec.py` (sqlite-vec vec0,
//! MiniLM-384) when `LIFEOS_MEMVEC` points at it - vec0 is not loadable from the
//! Rust libSQL build, so that half is a best-effort subprocess. The two ranked
//! lists are fused with reciprocal-rank fusion (RRF_K=60, as memory-recall uses)
//! and degrade gracefully to lexical-only when memvec is absent or fails.

use crate::auth::resolve_workspace;
use crate::error::ApiResult;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Reciprocal-rank-fusion constant (Cormack et al.; same as memory-recall).
const RRF_K: f64 = 60.0;

#[derive(Deserialize)]
pub struct SearchParams {
    q: String,
    module: Option<String>,
    limit: Option<u32>,
    workspace_id: Option<String>,
}

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id =
        resolve_workspace(&headers, &state.config.jwt_secret, params.workspace_id.as_deref());
    let limit = params.limit.unwrap_or(20).clamp(1, 100);

    let match_query = build_fts_query(&params.q);
    if match_query.is_empty() {
        return Ok(Json(json!({ "query": params.q, "results": [], "mode": "empty" })));
    }

    // Pull more than `limit` from each source so fusion has material to work with.
    let pool = (limit * 3).min(100);
    let lexical = lexical_ids(&state, &workspace_id, &match_query, pool).await?;
    let semantic = semantic_ids(&state, &workspace_id, &params.q, pool).await;
    let used_vectors = semantic.is_some();
    let semantic = semantic.unwrap_or_default();

    let fused = rrf_fuse(&[lexical, semantic]);
    let top: Vec<(String, f64)> = take_top(fused, limit as usize);
    let results = hydrate(&state, &workspace_id, &top, params.module.as_deref()).await?;

    Ok(Json(json!({
        "query": params.q,
        "mode": if used_vectors { "hybrid" } else { "lexical" },
        "results": results,
    })))
}

/// Turn free text into a safe FTS5 MATCH expression: alnum tokens, each quoted
/// (so FTS5 special chars in user input can't break the query), OR-joined.
fn build_fts_query(raw: &str) -> String {
    let terms: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        // Count characters, not bytes: `len()` would drop a legitimate
        // single-character query while letting one multibyte char (e.g. CJK)
        // through, skewing recall. `chars().count()` keeps the >=2 rule correct
        // across scripts.
        .filter(|t| t.chars().count() >= 2)
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    terms.join(" OR ")
}

async fn lexical_ids(
    state: &AppState,
    workspace_id: &str,
    match_query: &str,
    limit: u32,
) -> ApiResult<Vec<String>> {
    // FTS5's MATCH and bm25() require the bare table name (no alias). The
    // unqualified `entities_fts`/`entities_idx` resolve to the attached `d`.
    let sql = "SELECT i.id FROM entities_fts \
               JOIN entities_idx i ON i.rowid = entities_fts.rowid \
               WHERE entities_fts MATCH ?1 AND i.workspace_id = ?2 \
               ORDER BY bm25(entities_fts) LIMIT ?3";
    let mut rows = state
        .conn
        .query(sql, libsql::params![match_query, workspace_id, limit])
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}

/// Best-effort semantic neighbours via the shared memvec subprocess seam
/// (`crate::memvec_search`). Entity vectors use the plain `<ws>` label (memory
/// vectors are partitioned under `mem:<ws>`, so they never surface here).
/// `None` means the lane was unavailable (no LIFEOS_MEMVEC, missing deps, or a
/// failure) - the caller then runs lexical-only; `Some(vec![])` means it ran
/// and found nothing.
async fn semantic_ids(
    state: &AppState,
    workspace_id: &str,
    query: &str,
    limit: u32,
) -> Option<Vec<String>> {
    let runner = crate::memvec_search::SubprocessMemvec::from_env(&state.config.derived_db_path)?;
    crate::memvec_search::query_ids(&runner, workspace_id, query, limit).await
}

/// Reciprocal-rank fusion across several ranked id lists (best-first).
fn rrf_fuse(lists: &[Vec<String>]) -> HashMap<String, f64> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    scores
}

fn take_top(scores: HashMap<String, f64>, n: usize) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = scores.into_iter().collect();
    // Sort by score desc, then id asc for a stable, deterministic order.
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    v.truncate(n);
    v
}

/// Fetch full entity rows for the fused ids, preserving fused order, optionally
/// filtering by module. Workspace-scoped.
async fn hydrate(
    state: &AppState,
    workspace_id: &str,
    ranked: &[(String, f64)],
    module: Option<&str>,
) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    for (id, score) in ranked {
        let mut rows = state
            .conn
            .query(
                "SELECT id, module, type, title, status, attrs FROM main.entities \
                 WHERE id = ?1 AND workspace_id = ?2",
                libsql::params![id.clone(), workspace_id],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let module_val: String = row.get(1)?;
            if let Some(m) = module {
                if module_val != m {
                    continue;
                }
            }
            let attrs_raw: String = row.get(5)?;
            out.push(json!({
                "id": row.get::<String>(0)?,
                "module": module_val,
                "type": row.get::<String>(2)?,
                "title": row.get::<Option<String>>(3)?,
                "status": row.get::<Option<String>>(4)?,
                "attrs": serde_json::from_str::<Value>(&attrs_raw).unwrap_or(Value::Null),
                "score": score,
            }));
        }
    }
    Ok(out)
}
