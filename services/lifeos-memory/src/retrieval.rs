//! Activation-scored retrieval (issue #113, docs/AI-MEMORY.md §4).
//!
//! Not flat top-k: every candidate is scored
//!   A(m) = relevance · recency · importance · frequency
//! where relevance is an RRF fusion of the FTS5 (BM25), LIKE-fallback,
//! entity-match and (optional) vector lanes; recency is ACT-R exponential
//! decay; frequency is log base-level activation from access_count; and
//! importance was scored at write time. The hot path makes NO LLM call -
//! query reformulation is the caller's (optional, cached) concern.
//!
//! Bi-temporality (§7): current recall only sees `t_invalid IS NULL`; a
//! point-in-time recall (`as_of`) sees what was true then.

use crate::error::MemoryError;
use crate::gate::{is_multi_hop, needs_memory};
use async_trait::async_trait;
use libsql::{params, Connection};
use std::collections::HashMap;

const RRF_K: f64 = 60.0;
/// RRF scores are tiny (max 1/(k+1)); normalize so a rank-1 single-lane hit
/// has relevance ~1.0 and thresholds stay human-readable.
const RRF_NORM: f64 = RRF_K + 1.0;
/// Relevance haircut per graph hop for spreading-activation neighbors.
const HOP_DAMPING: f64 = 0.5;

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub id: String,
    pub rank: usize,
}

/// Semantic ANN lane. vec0/sqlite-vec is owned by memvec.py (not loadable
/// from the Rust libSQL build), so the searcher is injected: `lifeos-api` wires
/// a subprocess-backed `MemvecSearcher` (shared with `/api/search`, scoped to a
/// `mem:<ws>` vector label) when LIFEOS_MEMVEC is set, tests wire fakes, and
/// `NoopVectorSearcher` degrades recall gracefully to lexical-only when it is
/// unset or the subprocess fails.
#[async_trait]
pub trait VectorSearcher: Send + Sync {
    async fn search(
        &self,
        workspace_id: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<VectorHit>, MemoryError>;
}

pub struct NoopVectorSearcher;

#[async_trait]
impl VectorSearcher for NoopVectorSearcher {
    async fn search(&self, _ws: &str, _q: &str, _k: usize) -> Result<Vec<VectorHit>, MemoryError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivationBreakdown {
    pub relevance: f64,
    pub recency: f64,
    pub importance: f64,
    pub frequency: f64,
    pub activation: f64,
    /// Set when this memory entered via spreading activation rather than a
    /// direct lexical/vector hit.
    pub via_graph_hops: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecalledMemory {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub ts: i64,
    pub confidence: f64,
    pub access_count: i64,
    pub source_event_ids: Vec<String>,
    pub tiered_ref: Option<String>,
    pub breakdown: ActivationBreakdown,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RecallOutcome {
    /// Self-RAG gate decided this turn doesn't need long-term memory.
    Skipped { reason: String },
    /// Candidates existed but none was reliable - the caller should say
    /// "I don't know" instead of confabulating.
    Abstained { top_activation: f64, threshold: f64 },
    Recalled { memories: Vec<RecalledMemory>, expanded_graph: bool },
}

#[derive(Debug, Clone)]
pub struct RecallParams {
    pub top_k: usize,
    /// ACT-R decay factor per hour (recency = decay^Δhours).
    pub decay_per_hour: f64,
    pub abstention_threshold: f64,
    /// Point-in-time recall: what was true at this unix ts (None = now).
    pub as_of: Option<i64>,
    /// None = auto (multi-hop detector); Some overrides.
    pub expand_graph: Option<bool>,
    /// Apply the self-RAG gate (callers doing forced inspection disable it).
    pub use_gate: bool,
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            top_k: 8,
            decay_per_hour: 0.995,
            abstention_threshold: 0.05,
            as_of: None,
            expand_graph: None,
            use_gate: true,
        }
    }
}

pub async fn recall(
    conn: &Connection,
    workspace_id: &str,
    query: &str,
    now: i64,
    params_in: &RecallParams,
    vector: &dyn VectorSearcher,
) -> Result<RecallOutcome, MemoryError> {
    if params_in.use_gate && !needs_memory(query) {
        return Ok(RecallOutcome::Skipped { reason: "self-rag gate: turn needs no memory".into() });
    }

    let pool = (params_in.top_k * 4).max(24);
    // Each lane yields (id, rank). Ranks are DENSE and tie-aware: candidates a
    // lane cannot distinguish share a rank, so RRF relevance is identical for
    // them and the other activation factors (recency/importance/frequency)
    // decide - never incidental row order.
    let mut lanes: Vec<Vec<(String, usize)>> = Vec::new();
    lanes.push(fts_lane(conn, workspace_id, query, pool).await);
    lanes.push(like_lane(conn, workspace_id, query, pool).await?);
    lanes.push(entity_lane(conn, workspace_id, query, pool).await?);
    // Vector lane is best-effort by design - errors just drop the lane.
    if let Ok(hits) = vector.search(workspace_id, query, pool).await {
        lanes.push(hits.into_iter().map(|h| (h.id, h.rank)).collect());
    }

    // RRF fusion -> relevance per candidate id.
    let mut relevance: HashMap<String, f64> = HashMap::new();
    for lane in &lanes {
        for (id, rank) in lane {
            *relevance.entry(id.clone()).or_insert(0.0) += RRF_NORM / (RRF_K + 1.0 + *rank as f64);
        }
    }

    // Spreading activation (issue #114): only when the query is multi-hop
    // (or the caller forces it), expand 1-2 hops from the strongest seeds and
    // admit neighbors with hop-damped relevance.
    let expand = params_in.expand_graph.unwrap_or_else(|| is_multi_hop(query));
    let mut via_hops: HashMap<String, usize> = HashMap::new();
    if expand && !relevance.is_empty() {
        let mut seeds: Vec<(String, f64)> =
            relevance.iter().map(|(k, v)| (k.clone(), *v)).collect();
        seeds.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
        let seed_ids: Vec<String> = seeds.iter().take(5).map(|(id, _)| id.clone()).collect();
        let max_rel = seeds.first().map(|(_, v)| *v).unwrap_or(0.0);
        for (id, hops) in crate::graph::expand_seeds(conn, workspace_id, &seed_ids, 2).await? {
            let boosted = max_rel * HOP_DAMPING.powi(hops as i32);
            let entry = relevance.entry(id.clone()).or_insert(0.0);
            if boosted > *entry {
                *entry = boosted;
                via_hops.insert(id, hops);
            }
        }
    }

    if relevance.is_empty() {
        return Ok(RecallOutcome::Abstained {
            top_activation: 0.0,
            threshold: params_in.abstention_threshold,
        });
    }

    let mut scored =
        score_candidates(conn, workspace_id, &relevance, &via_hops, now, params_in).await?;
    // Deterministic order: activation desc, then id asc as tiebreak.
    scored.sort_by(|a, b| {
        b.breakdown
            .activation
            .partial_cmp(&a.breakdown.activation)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    scored.truncate(params_in.top_k);

    let top = scored.first().map(|m| m.breakdown.activation).unwrap_or(0.0);
    if top < params_in.abstention_threshold {
        return Ok(RecallOutcome::Abstained {
            top_activation: top,
            threshold: params_in.abstention_threshold,
        });
    }

    // ACT-R base-level learning: recall strengthens (current-truth mode only;
    // point-in-time inspection shouldn't distort activation history).
    if params_in.as_of.is_none() {
        for m in &scored {
            conn.execute(
                "UPDATE memory_nodes SET access_count = access_count + 1, last_accessed = ?1 \
                 WHERE workspace_id = ?2 AND id = ?3",
                params![now, workspace_id, m.id.clone()],
            )
            .await?;
        }
    }

    Ok(RecallOutcome::Recalled { memories: scored, expanded_graph: expand })
}

/// Hydrate candidate ids into scored memories, applying bi-temporal validity.
async fn score_candidates(
    conn: &Connection,
    ws: &str,
    relevance: &HashMap<String, f64>,
    via_hops: &HashMap<String, usize>,
    now: i64,
    p: &RecallParams,
) -> Result<Vec<RecalledMemory>, MemoryError> {
    let mut out = Vec::new();
    for (id, rel) in relevance {
        let mut rows = conn
            .query(
                "SELECT kind, content, importance, access_count, confidence, \
                        source_event_ids, ts, t_invalid, tiered_ref \
                 FROM memory_nodes WHERE workspace_id = ?1 AND id = ?2",
                params![ws, id.clone()],
            )
            .await?;
        let Some(row) = rows.next().await? else { continue };
        let ts: i64 = row.get(6)?;
        let t_invalid: Option<i64> = row.get(7)?;
        let valid = match p.as_of {
            // Point-in-time: existed then, and not yet invalidated then.
            Some(as_of) => ts <= as_of && t_invalid.map(|t| t > as_of).unwrap_or(true),
            // Current truth: never invalidated.
            None => t_invalid.is_none(),
        };
        if !valid {
            continue;
        }

        let importance: f64 = row.get(2)?;
        let access_count: i64 = row.get(3)?;
        let reference = p.as_of.unwrap_or(now);
        let hours = ((reference - ts).max(0)) as f64 / 3600.0;
        let recency = p.decay_per_hour.powf(hours);
        let frequency = 1.0 + (1.0 + access_count as f64).ln();
        let activation = rel * recency * importance.max(0.05) * frequency;

        let sources: String = row.get(5)?;
        out.push(RecalledMemory {
            id: id.clone(),
            kind: row.get(0)?,
            content: row.get(1)?,
            ts,
            confidence: row.get(4)?,
            access_count,
            source_event_ids: serde_json::from_str(&sources).unwrap_or_default(),
            tiered_ref: row.get(8)?,
            breakdown: ActivationBreakdown {
                relevance: *rel,
                recency,
                importance,
                frequency,
                activation,
                via_graph_hops: via_hops.get(id).copied(),
            },
        });
    }
    Ok(out)
}

/// Same safe FTS5 MATCH construction as routes/search.rs.
fn build_fts_query(raw: &str) -> String {
    let terms: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    terms.join(" OR ")
}

/// BM25 lane over the derived FTS index. Best-effort: the derived DB may not
/// be attached (drain), in which case this lane is empty and the LIKE lane
/// carries lexical recall. Dense tie-aware ranks over the bm25 score.
async fn fts_lane(conn: &Connection, ws: &str, query: &str, limit: usize) -> Vec<(String, usize)> {
    let match_query = build_fts_query(query);
    if match_query.is_empty() {
        return Vec::new();
    }
    let sql = "SELECT i.id, bm25(memory_fts) FROM memory_fts \
               JOIN memory_idx i ON i.rowid = memory_fts.rowid \
               WHERE memory_fts MATCH ?1 AND i.workspace_id = ?2 \
               ORDER BY bm25(memory_fts) LIMIT ?3";
    let Ok(mut rows) = conn.query(sql, params![match_query, ws, limit as i64]).await else {
        return Vec::new();
    };
    let mut scored: Vec<(String, f64)> = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let (Ok(id), Ok(score)) = (row.get::<String>(0), row.get::<f64>(1)) {
            scored.push((id, score));
        }
    }
    let mut out = Vec::new();
    let mut rank = 0;
    let mut last_score: Option<f64> = None;
    for (i, (id, score)) in scored.into_iter().enumerate() {
        if last_score.map(|s| (s - score).abs() > 1e-9).unwrap_or(false) {
            rank = i;
        }
        last_score = Some(score);
        out.push((id, rank));
    }
    out
}

fn query_tokens(query: &str) -> Vec<String> {
    let mut tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .collect();
    tokens.dedup();
    tokens
}

/// Convert (id -> match count) hits into dense tie-aware (id, rank) pairs:
/// equal match counts share a rank.
fn dense_ranks(hits: HashMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut ranked: Vec<(String, usize)> = hits.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = Vec::new();
    let mut rank = 0;
    let mut last_count: Option<usize> = None;
    for (i, (id, count)) in ranked.into_iter().take(limit).enumerate() {
        if last_count.map(|c| c != count).unwrap_or(false) {
            rank = i;
        }
        last_count = Some(count);
        out.push((id, rank));
    }
    out
}

/// Lexical fallback over memory_nodes.content, ranked by token-match count.
async fn like_lane(
    conn: &Connection,
    ws: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, usize)>, MemoryError> {
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits: HashMap<String, usize> = HashMap::new();
    for token in tokens.iter().take(8) {
        let pattern = format!("%{token}%");
        let mut rows = conn
            .query(
                "SELECT id FROM memory_nodes \
                 WHERE workspace_id = ?1 AND content LIKE ?2 COLLATE NOCASE LIMIT ?3",
                params![ws, pattern, limit as i64],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            *hits.entry(row.get::<String>(0)?).or_insert(0) += 1;
        }
    }
    Ok(dense_ranks(hits, limit))
}

/// Entity-match lane: memories linked (via 'about' edges) to entities whose
/// title matches a query token.
async fn entity_lane(
    conn: &Connection,
    ws: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, usize)>, MemoryError> {
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits: HashMap<String, usize> = HashMap::new();
    for token in tokens.iter().take(8) {
        let pattern = format!("%{token}%");
        let mut rows = conn
            .query(
                "SELECT e.from_id FROM memory_edges e \
                 JOIN entities ent ON ent.id = e.to_id AND ent.workspace_id = e.workspace_id \
                 WHERE e.workspace_id = ?1 AND e.t_invalid IS NULL \
                   AND ent.title LIKE ?2 COLLATE NOCASE LIMIT ?3",
                params![ws, pattern, limit as i64],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            *hits.entry(row.get::<String>(0)?).or_insert(0) += 1;
        }
    }
    Ok(dense_ranks(hits, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::project_workspace;
    use crate::testutil::{seed_entity, seed_event, test_conn_with_derived};
    use serde_json::json;

    async fn setup() -> Connection {
        let conn = test_conn_with_derived().await;
        seed_entity(&conn, "ws_1", "ent_rel", "trading", "RELIANCE swing").await;
        seed_event(
            &conn, "ws_1", "evt_trade", 1_000_000, "trade.closed", Some("ent_rel"), "user",
            json!({"pnl": 4200, "note": "booked profit on reliance breakout"}), None,
        )
        .await;
        seed_event(
            &conn, "ws_1", "evt_study", 1_003_600, "study.review", None, "bot",
            json!({"topic": "order flow imbalance basics"}), None,
        )
        .await;
        project_workspace(&conn, "ws_1").await.unwrap();
        conn
    }

    #[tokio::test]
    async fn recall_scores_and_returns_provenance() {
        let conn = setup().await;
        let out = recall(
            &conn, "ws_1", "what happened with the reliance trade?", 1_010_000,
            &RecallParams::default(), &NoopVectorSearcher,
        )
        .await
        .unwrap();
        let RecallOutcome::Recalled { memories, .. } = out else {
            panic!("expected recall, got {out:?}")
        };
        assert!(!memories.is_empty());
        let top = &memories[0];
        assert!(top.content.contains("reliance breakout"));
        assert_eq!(top.source_event_ids, vec!["evt_trade".to_string()]);
        assert!(top.breakdown.activation > 0.0);
        assert!(top.breakdown.recency <= 1.0 && top.breakdown.recency > 0.9);

        // Access bookkeeping happened (ACT-R base-level learning).
        let mut rows = conn
            .query(
                "SELECT access_count FROM memory_nodes WHERE id = ?1",
                params![top.id.clone()],
            )
            .await
            .unwrap();
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn gate_skips_and_unknown_abstains() {
        let conn = setup().await;
        let skipped = recall(
            &conn, "ws_1", "thanks!", 1_010_000, &RecallParams::default(), &NoopVectorSearcher,
        )
        .await
        .unwrap();
        assert!(matches!(skipped, RecallOutcome::Skipped { .. }));

        let unknown = recall(
            &conn, "ws_1", "zzqxv nonexistent frobnicator", 1_010_000,
            &RecallParams::default(), &NoopVectorSearcher,
        )
        .await
        .unwrap();
        assert!(
            matches!(unknown, RecallOutcome::Abstained { .. }),
            "unknown fact must abstain, not confabulate: {unknown:?}"
        );
    }

    #[tokio::test]
    async fn workspace_isolation_holds() {
        let conn = setup().await;
        let out = recall(
            &conn, "ws_2", "reliance trade breakout", 1_010_000,
            &RecallParams::default(), &NoopVectorSearcher,
        )
        .await
        .unwrap();
        assert!(
            !matches!(out, RecallOutcome::Recalled { .. }),
            "ws_2 must recall nothing of ws_1: {out:?}"
        );
    }

    #[tokio::test]
    async fn recency_decays_and_frequency_boosts() {
        let conn = test_conn_with_derived().await;
        seed_event(
            &conn, "ws_1", "evt_old", 0, "note.captured", None, "user",
            json!({"text": "gamma exposure note"}), None,
        )
        .await;
        seed_event(
            &conn, "ws_1", "evt_new", 720 * 3600, "note.captured", None, "user",
            json!({"text": "gamma exposure note"}), None,
        )
        .await;
        project_workspace(&conn, "ws_1").await.unwrap();
        let out = recall(
            &conn, "ws_1", "gamma exposure", 720 * 3600 + 60,
            &RecallParams { abstention_threshold: 0.0, ..Default::default() },
            &NoopVectorSearcher,
        )
        .await
        .unwrap();
        let RecallOutcome::Recalled { memories, .. } = out else { panic!() };
        let newer = memories.iter().find(|m| m.ts == 720 * 3600).unwrap();
        let older = memories.iter().find(|m| m.ts == 0).unwrap();
        assert!(
            newer.breakdown.activation > older.breakdown.activation,
            "identical content: the recent memory must outrank the stale one"
        );
    }
}
