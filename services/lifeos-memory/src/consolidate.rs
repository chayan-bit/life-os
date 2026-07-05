//! Consolidation "sleep" jobs (issue #115, docs/AI-MEMORY.md §5).
//!
//! One cycle = segment_episodes -> consolidate (summaries, multi-resolution)
//! -> score_surprise/importance -> supersede_detector -> decay_sweep -> rule
//! learning. Every output is a new provenance-linked EVENT (actor 'harness'),
//! never a direct mutation of the read models - the projector folds those
//! events in afterwards, which is exactly why a full rebuild replays
//! consolidation's work byte-identically (via the LLM replay cache) instead
//! of re-running it.

use crate::communities::{rebuild_communities, CommunitySummarizer};
use crate::error::MemoryError;
use crate::model::MemoryModel;
use crate::procedural::{PolicyLearner, RuleDelta};
use crate::project::{fetch_events_after, node_id, project_workspace, EventRecord};
use crate::redact::{flatten_redacted, is_secret_event_type};
use libsql::{params, Connection};
use std::collections::HashSet;

/// A new episode starts after this much silence (cognitive boundary, ES-Mem).
const EPISODE_GAP_SECS: i64 = 1800;
/// Events younger than this are left for the next cycle - the episode they
/// belong to may still be open.
const SETTLE_SECS: i64 = 900;
/// Token-overlap below this marks a memory as surprising (outlier) ->
/// high-importance, slow-decay (§5).
const SURPRISE_OVERLAP: f64 = 0.1;
const SURPRISE_IMPORTANCE: f64 = 0.8;
/// Enough episode summaries in one cycle roll up into a day-level reflection.
const DAY_ROLLUP_MIN_EPISODES: usize = 3;
/// Rule aging (issue #128, docs/AGENT-CORE.md §6): a rule sitting active this
/// long without earning confidence is stale advice.
const RULE_TTL_DAYS: i64 = 45;
/// Below this confidence, a stale rule is retired rather than kept forever.
const RULE_RETIRE_CONFIDENCE: f64 = 0.6;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SleepReport {
    pub events_consumed: usize,
    pub episodes: usize,
    pub summaries: usize,
    pub surprises: usize,
    pub supersedes: usize,
    pub rules_added: usize,
    pub rules_retired: usize,
    pub cold_candidates: usize,
    pub communities: usize,
}

/// How many events are waiting past the consolidation cursor - drain uses
/// this (with a threshold) to decide when a sleep cycle is worth enqueuing.
pub async fn unconsolidated_importance(
    conn: &Connection,
    workspace_id: &str,
) -> Result<i64, MemoryError> {
    let (ts, id) = read_cursor(conn, workspace_id).await?;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM events \
             WHERE workspace_id = ?1 AND (ts > ?2 OR (ts = ?2 AND id > ?3)) \
               AND type NOT LIKE 'memory.%'",
            params![workspace_id, ts, id],
        )
        .await?;
    Ok(rows.next().await?.map(|r| r.get::<i64>(0)).transpose()?.unwrap_or(0))
}

/// Run one full sleep cycle. `model` should be wrapped in ReplayCachedModel
/// by the caller so summaries are content-addressed and replayable.
/// `summarizer` drives the GraphRAG community-summary rebuild (issue #139,
/// docs/AGENT-CORE.md §13) that runs at the end of the cycle, same seam
/// shape as `learner`.
pub async fn run_sleep_cycle(
    conn: &Connection,
    workspace_id: &str,
    model: &dyn MemoryModel,
    learner: &dyn PolicyLearner,
    summarizer: &dyn CommunitySummarizer,
    now: i64,
) -> Result<SleepReport, MemoryError> {
    // Make sure raw events are projected before we reason about their nodes.
    project_workspace(conn, workspace_id).await?;

    let mut report = SleepReport::default();
    let (cur_ts, cur_id) = read_cursor(conn, workspace_id).await?;

    // Collect settled, non-memory, non-secret events past the cursor.
    let mut consumable: Vec<EventRecord> = Vec::new();
    let (mut scan_ts, mut scan_id) = (cur_ts, cur_id.clone());
    'outer: loop {
        let batch = fetch_events_after(conn, workspace_id, scan_ts, &scan_id, 500).await?;
        if batch.is_empty() {
            break;
        }
        for ev in batch {
            if ev.ts > now - SETTLE_SECS {
                break 'outer; // the tail may still be an open episode
            }
            scan_ts = ev.ts;
            scan_id = ev.id.clone();
            if !ev.event_type.starts_with("memory.") && !is_secret_event_type(&ev.event_type) {
                consumable.push(ev);
            }
        }
    }
    if consumable.is_empty() {
        return Ok(report);
    }
    report.events_consumed = consumable.len();

    // --- segment_episodes: cut at cognitive boundaries (silence gaps).
    let mut episodes: Vec<Vec<&EventRecord>> = Vec::new();
    for ev in &consumable {
        match episodes.last_mut() {
            Some(ep) if ev.ts - ep.last().unwrap().ts <= EPISODE_GAP_SECS => ep.push(ev),
            _ => episodes.push(vec![ev]),
        }
    }
    report.episodes = episodes.len();

    // --- consolidate: episode -> summary (through the replay-cached model).
    let mut episode_summaries: Vec<String> = Vec::new();
    let mut all_sources: Vec<String> = Vec::new();
    for ep in &episodes {
        let ids: Vec<String> = ep.iter().map(|e| e.id.clone()).collect();
        let last_id = ids.last().cloned();
        emit(
            conn, workspace_id, "memory.episode.segmented", None, last_id.as_deref(),
            &serde_json::json!({ "event_ids": ids, "start": ep[0].ts, "end": ep.last().unwrap().ts }),
            now,
        )
        .await?;

        let prompt = ep.iter().map(|e| event_text(e)).collect::<Vec<_>>().join("\n");
        let content = model.complete("summarize", &prompt).await?;
        if content.trim().is_empty() {
            continue;
        }
        let confidence = (0.4 + 0.05 * ep.len() as f64).min(0.9);
        emit(
            conn, workspace_id, "memory.summary.created", None, last_id.as_deref(),
            &serde_json::json!({
                "level": "episode", "content": content, "confidence": confidence,
                "source_event_ids": ids,
            }),
            now,
        )
        .await?;
        report.summaries += 1;
        episode_summaries.push(content);
        all_sources.extend(ids);
    }

    // --- multi-resolution rollup: enough episodes -> a day-level reflection.
    if episode_summaries.len() >= DAY_ROLLUP_MIN_EPISODES {
        let content = model.complete("reflect", &episode_summaries.join("\n")).await?;
        if !content.trim().is_empty() {
            emit(
                conn, workspace_id, "memory.summary.created", None, None,
                &serde_json::json!({
                    "level": "day", "content": content, "confidence": 0.5,
                    "source_event_ids": all_sources,
                }),
                now,
            )
            .await?;
            report.summaries += 1;
        }
    }

    // --- score_surprise: low-overlap outliers become high-importance.
    report.surprises = score_surprise(conn, workspace_id, &consumable, now).await?;

    // --- supersede_detector: '*.updated' events invalidate the previous
    //     memory about the same entity (bi-temporal, §7).
    for ev in &consumable {
        if !ev.event_type.ends_with(".updated") {
            continue;
        }
        let Some(entity_id) = &ev.entity_id else { continue };
        if let Some(old_id) = previous_event_for_entity(conn, workspace_id, entity_id, ev).await? {
            emit(
                conn, workspace_id, "memory.supersede.detected", Some(entity_id), Some(&ev.id),
                // t_invalid = when the NEW fact became true, so point-in-time
                // queries between the two facts still resolve the old one.
                &serde_json::json!({ "old_event_id": old_id, "new_event_id": ev.id, "t_invalid": ev.ts }),
                now,
            )
            .await?;
            report.supersedes += 1;
        }
    }

    // --- rule learning (procedural memory, §8) - via the PolicyLearner seam.
    for delta in learner.derive_rule_deltas(&consumable) {
        match delta {
            RuleDelta::Add { rule, confidence, source_event_ids } => {
                emit(
                    conn, workspace_id, "memory.rule.added", None,
                    source_event_ids.first().map(|s| s.as_str()),
                    &serde_json::json!({
                        "rule": rule, "confidence": confidence,
                        "source_event_ids": source_event_ids,
                    }),
                    now,
                )
                .await?;
                report.rules_added += 1;
            }
            RuleDelta::Retire { rule_id, source_event_id } => {
                emit(
                    conn, workspace_id, "memory.rule.retired", None, Some(&source_event_id),
                    &serde_json::json!({ "rule_id": rule_id }),
                    now,
                )
                .await?;
                report.rules_retired += 1;
            }
        }
    }

    // --- rule aging (issue #128): retire stale, low-confidence rules so
    //     procedural advice doesn't accrete forever. Emits proper
    //     memory.rule.retired events through the same event-append path the
    //     learner uses above - never a direct UPDATE of memory_rules outside
    //     the projector.
    report.rules_retired += retire_stale_rules(conn, workspace_id, now).await?;

    // --- decay_sweep (no LLM): surface cold-tier candidates + a ledger entry.
    let cold = crate::tier::find_cold_nodes(conn, workspace_id, now, 30 * 86400, 0.4, 1).await?;
    report.cold_candidates = cold.len();
    emit(
        conn, workspace_id, "memory.decay.swept", None, None,
        &serde_json::json!({ "events_consumed": report.events_consumed, "cold_candidates": cold.len() }),
        now,
    )
    .await?;

    write_cursor(conn, workspace_id, scan_ts, &scan_id, now).await?;
    // Fold everything this cycle emitted into the read models.
    project_workspace(conn, workspace_id).await?;

    // --- GraphRAG global lens (issue #139, §13): rebuild the community map
    //     from the now-current graph. Derived + rebuildable like everything
    //     else here, so a delete+reinsert every cycle is the whole contract.
    report.communities = rebuild_communities(conn, workspace_id, summarizer, now).await?;

    Ok(report)
}

/// Retires active rules older than `RULE_TTL_DAYS` that never earned
/// `RULE_RETIRE_CONFIDENCE` or better, one `memory.rule.retired` event per
/// rule (actor 'harness', folded by the projector at the end of this cycle -
/// `apply_rule_retired` in project.rs is the only writer of
/// `memory_rules.status`). Returns the count retired.
async fn retire_stale_rules(conn: &Connection, ws: &str, now: i64) -> Result<usize, MemoryError> {
    let cutoff = now - RULE_TTL_DAYS * 86400;
    let mut rows = conn
        .query(
            "SELECT id FROM memory_rules \
             WHERE workspace_id = ?1 AND status = 'active' \
               AND created_ts < ?2 AND confidence < ?3",
            params![ws, cutoff, RULE_RETIRE_CONFIDENCE],
        )
        .await?;
    let mut stale_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        stale_ids.push(row.get(0)?);
    }

    for rule_id in &stale_ids {
        emit(
            conn, ws, "memory.rule.retired", None, None,
            &serde_json::json!({ "rule_id": rule_id, "reason": "stale" }),
            now,
        )
        .await?;
    }
    Ok(stale_ids.len())
}

fn event_text(ev: &EventRecord) -> String {
    let mut flattened = String::new();
    flatten_redacted(&ev.attrs, &mut flattened);
    format!("[{}] {}: {}", ev.actor.as_deref().unwrap_or("unknown"), ev.event_type, flattened)
}

fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Flag consumed events whose projected node barely overlaps anything older -
/// surprising/contradicting memories decay slowly (§5).
async fn score_surprise(
    conn: &Connection,
    ws: &str,
    consumed: &[EventRecord],
    now: i64,
) -> Result<usize, MemoryError> {
    // Prior memory vocabulary: the most recent 100 nodes strictly older than
    // the batch. Deterministic bound keeps this cheap.
    let batch_start = consumed.first().map(|e| e.ts).unwrap_or(0);
    let mut rows = conn
        .query(
            "SELECT content FROM memory_nodes \
             WHERE workspace_id = ?1 AND ts < ?2 ORDER BY ts DESC, id ASC LIMIT 100",
            params![ws, batch_start],
        )
        .await?;
    let mut prior: Vec<HashSet<String>> = Vec::new();
    while let Some(row) = rows.next().await? {
        prior.push(word_set(&row.get::<String>(0)?));
    }
    if prior.is_empty() {
        return Ok(0);
    }

    let mut flagged = 0;
    for ev in consumed {
        let words = word_set(&event_text(ev));
        if words.is_empty() {
            continue;
        }
        let max_overlap = prior
            .iter()
            .map(|p| {
                let inter = words.intersection(p).count() as f64;
                let union = (words.len() + p.len()) as f64 - inter;
                if union == 0.0 { 0.0 } else { inter / union }
            })
            .fold(0.0_f64, f64::max);
        if max_overlap < SURPRISE_OVERLAP {
            let nid = node_id(ws, &ev.id);
            emit(
                conn, ws, "memory.importance.scored", None, Some(&ev.id),
                &serde_json::json!({ "node_id": nid, "importance": SURPRISE_IMPORTANCE, "reason": "surprise" }),
                now,
            )
            .await?;
            emit(
                conn, ws, "memory.surprise.flagged", None, Some(&ev.id),
                &serde_json::json!({ "node_id": nid, "max_overlap": max_overlap }),
                now,
            )
            .await?;
            flagged += 1;
        }
    }
    Ok(flagged)
}

/// The most recent non-memory event about the same entity strictly before
/// `ev` in the (ts, id) replay order.
async fn previous_event_for_entity(
    conn: &Connection,
    ws: &str,
    entity_id: &str,
    ev: &EventRecord,
) -> Result<Option<String>, MemoryError> {
    let mut rows = conn
        .query(
            "SELECT id FROM events \
             WHERE workspace_id = ?1 AND entity_id = ?2 \
               AND (ts < ?3 OR (ts = ?3 AND id < ?4)) \
               AND type NOT LIKE 'memory.%' \
             ORDER BY ts DESC, id DESC LIMIT 1",
            params![ws, entity_id, ev.ts, ev.id.clone()],
        )
        .await?;
    Ok(rows.next().await?.map(|r| r.get(0)).transpose()?)
}

async fn read_cursor(conn: &Connection, ws: &str) -> Result<(i64, String), MemoryError> {
    let mut rows = conn
        .query(
            "SELECT consolidated_ts, consolidated_id FROM memory_cursors WHERE workspace_id = ?1",
            params![ws],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok((row.get(0)?, row.get(1)?)),
        None => Ok((0, String::new())),
    }
}

async fn write_cursor(
    conn: &Connection,
    ws: &str,
    ts: i64,
    id: &str,
    now: i64,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memory_cursors (workspace_id, consolidated_ts, consolidated_id, updated_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(workspace_id) DO UPDATE SET \
            consolidated_ts = excluded.consolidated_ts, \
            consolidated_id = excluded.consolidated_id, updated_at = excluded.updated_at",
        params![ws, ts, id, now],
    )
    .await?;
    Ok(())
}

/// Append one consolidation event (actor 'harness'), with a causal pointer
/// back to the event that triggered it - the ledger sees everything memory
/// does.
async fn emit(
    conn: &Connection,
    ws: &str,
    event_type: &str,
    entity_id: Option<&str>,
    caused_by: Option<&str>,
    attrs: &serde_json::Value,
    now: i64,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs, caused_by_event_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'harness', ?6, ?7)",
        params![
            format!("evt_{}", ulid::Ulid::new()),
            ws,
            now,
            event_type,
            entity_id,
            attrs.to_string(),
            caused_by
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::communities::HeuristicSummarizer;
    use crate::model::{HeuristicModel, ReplayCachedModel};
    use crate::procedural::HeuristicPolicyLearner;
    use crate::testutil::{seed_entity, seed_event, test_conn};
    use serde_json::json;

    const NOW: i64 = 1_000_000;

    #[tokio::test]
    async fn sleep_cycle_segments_summarizes_with_provenance_and_is_incremental() {
        let conn = test_conn().await;
        // Episode 1: three close events. Episode 2: one event much later.
        for (i, ts) in [(0, 1000), (1, 1300), (2, 1600)] {
            seed_event(
                &conn, "ws_1", &format!("evt_a{i}"), ts, "study.review", None, "user",
                json!({"topic": format!("chapter {i}")}), None,
            )
            .await;
        }
        seed_event(
            &conn, "ws_1", "evt_b0", 9000, "task.completed", None, "bot",
            json!({"note": "shipped the parser"}), None,
        )
        .await;

        let model = ReplayCachedModel::new(&HeuristicModel, &conn, NOW);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW).await.unwrap();
        assert_eq!(report.episodes, 2);
        assert_eq!(report.summaries, 2, "one episode summary each, no day rollup yet");

        // Every summary traces back to its source events (auditable, §5).
        let mut rows = conn
            .query(
                "SELECT source_event_ids, confidence FROM memory_summaries \
                 WHERE workspace_id = 'ws_1' AND level = 'episode' ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut n = 0;
        while let Some(row) = rows.next().await.unwrap() {
            let sources: String = row.get(0).unwrap();
            assert!(sources.contains("evt_a0") || sources.contains("evt_b0"));
            assert!(row.get::<f64>(1).unwrap() > 0.0);
            n += 1;
        }
        assert_eq!(n, 2);

        // Second cycle: cursor advanced, nothing new to consolidate.
        let again =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW).await.unwrap();
        assert_eq!(again.events_consumed, 0);
        assert_eq!(again.summaries, 0);
    }

    #[tokio::test]
    async fn supersede_detector_invalidates_the_old_fact_bi_temporally() {
        let conn = test_conn().await;
        seed_entity(&conn, "ws_1", "ent_profile", "profile", "Home city").await;
        seed_event(
            &conn, "ws_1", "evt_delhi", 1000, "profile.updated", Some("ent_profile"), "user",
            json!({"city": "Delhi"}), None,
        )
        .await;
        seed_event(
            &conn, "ws_1", "evt_blr", 5000, "profile.updated", Some("ent_profile"), "user",
            json!({"city": "Bangalore"}), None,
        )
        .await;

        let model = ReplayCachedModel::new(&HeuristicModel, &conn, NOW);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW).await.unwrap();
        assert!(report.supersedes >= 1);

        // Old fact: invalidated with a pointer, never deleted.
        let old_node = node_id("ws_1", "evt_delhi");
        let mut rows = conn
            .query(
                "SELECT t_invalid, superseded_by_event_id, content FROM memory_nodes WHERE id = ?1",
                params![old_node],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("old fact still exists");
        assert!(row.get::<Option<i64>>(0).unwrap().is_some(), "no longer current truth");
        assert!(row.get::<Option<String>>(1).unwrap().is_some(), "supersede pointer set");
        assert!(row.get::<String>(2).unwrap().contains("Delhi"));

        // New fact stays current.
        let new_node = node_id("ws_1", "evt_blr");
        let mut rows = conn
            .query("SELECT t_invalid FROM memory_nodes WHERE id = ?1", params![new_node])
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().unwrap().get::<Option<i64>>(0).unwrap().is_none());
    }

    #[tokio::test]
    async fn feedback_becomes_a_persistent_rule() {
        let conn = test_conn().await;
        seed_event(
            &conn, "ws_1", "evt_fb", 1000, "feedback.given", None, "user",
            json!({"feedback": "when asked about a trade, include the market regime"}), None,
        )
        .await;
        let model = ReplayCachedModel::new(&HeuristicModel, &conn, NOW);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW).await.unwrap();
        assert_eq!(report.rules_added, 1);

        let rules = crate::procedural::rules_for_prompt(&conn, "ws_1", 1000).await.unwrap();
        assert_eq!(rules, vec!["when asked about a trade, include the market regime".to_string()]);
    }

    #[tokio::test]
    async fn stale_low_confidence_rule_is_retired_by_sleep() {
        let conn = test_conn().await;
        // A rule added long ago (ts=0) with confidence below the retire
        // threshold - this is exactly the "stale advice" case rule aging
        // exists to age out.
        seed_event(
            &conn, "ws_1", "evt_old_rule", 0, "memory.rule.added", None, "harness",
            json!({"rule": "old stale advice", "confidence": 0.5}), None,
        )
        .await;
        project_workspace(&conn, "ws_1").await.unwrap();

        // A fresh event so the cycle isn't a no-op (run_sleep_cycle returns
        // early when there is nothing new to consume).
        seed_event(
            &conn, "ws_1", "evt_new", NOW - SETTLE_SECS - 100, "task.completed", None, "user",
            json!({"note": "ship it"}), None,
        )
        .await;

        // Sleep well past the 45-day TTL relative to the rule's creation.
        let later = NOW + (RULE_TTL_DAYS + 1) * 86400;
        let model = ReplayCachedModel::new(&HeuristicModel, &conn, later);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, later).await.unwrap();
        assert_eq!(report.rules_retired, 1);

        let rules = crate::procedural::rules_for_prompt(&conn, "ws_1", 1000).await.unwrap();
        assert!(!rules.contains(&"old stale advice".to_string()));
    }

    #[tokio::test]
    async fn fresh_or_high_confidence_rules_survive_sleep() {
        let conn = test_conn().await;
        let later = NOW + (RULE_TTL_DAYS + 1) * 86400;

        // Old (ts=0) but high-confidence - survives on confidence alone.
        seed_event(
            &conn, "ws_1", "evt_trusted_rule", 0, "memory.rule.added", None, "harness",
            json!({"rule": "trusted advice", "confidence": 0.9}), None,
        )
        .await;
        // Low-confidence but recently added relative to `later` - survives
        // on freshness alone.
        seed_event(
            &conn, "ws_1", "evt_fresh_rule", later - 1000, "memory.rule.added", None, "harness",
            json!({"rule": "fresh advice", "confidence": 0.5}), None,
        )
        .await;
        project_workspace(&conn, "ws_1").await.unwrap();

        seed_event(
            &conn, "ws_1", "evt_new", NOW - SETTLE_SECS - 100, "task.completed", None, "user",
            json!({"note": "ship it"}), None,
        )
        .await;

        let model = ReplayCachedModel::new(&HeuristicModel, &conn, later);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, later).await.unwrap();
        assert_eq!(report.rules_retired, 0);

        let rules = crate::procedural::rules_for_prompt(&conn, "ws_1", 1000).await.unwrap();
        assert!(rules.contains(&"trusted advice".to_string()));
        assert!(rules.contains(&"fresh advice".to_string()));
    }

    /// Issue #139: the GraphRAG community map is rebuilt as part of the same
    /// sleep cycle that does everything else - derived state, same cadence.
    #[tokio::test]
    async fn sleep_cycle_rebuilds_the_community_map_idempotently() {
        let conn = test_conn().await;
        seed_entity(&conn, "ws_1", "ent_trading", "trading", "Banknifty desk").await;
        seed_entity(&conn, "ws_1", "ent_learning", "learning", "Market microstructure").await;
        for (i, ts) in [(0, 1000), (1, 1100), (2, 1200)] {
            seed_event(
                &conn, "ws_1", &format!("evt_trade_{i}"), ts, "trade.closed", Some("ent_trading"),
                "user", json!({"note": format!("trade {i}")}), None,
            )
            .await;
        }
        for (i, ts) in [(0, 2000), (1, 2100), (2, 2200)] {
            seed_event(
                &conn, "ws_1", &format!("evt_study_{i}"), ts, "study.review", Some("ent_learning"),
                "user", json!({"note": format!("topic {i}")}), None,
            )
            .await;
        }

        let model = ReplayCachedModel::new(&HeuristicModel, &conn, NOW);
        let report =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW)
                .await
                .unwrap();
        assert_eq!(report.communities, 2, "trading cluster + learning cluster");

        let map = crate::communities::list_network_map(&conn, "ws_1").await.unwrap();
        assert_eq!(map.len(), 2);
        let ids_before: Vec<String> = map.iter().map(|c| c.id.clone()).collect();

        // A later cycle with no new events still returns early (no new
        // communities work is queued) - the map stays exactly as it was.
        let again =
            run_sleep_cycle(&conn, "ws_1", &model, &HeuristicPolicyLearner, &HeuristicSummarizer, NOW)
                .await
                .unwrap();
        assert_eq!(again.communities, 0, "no new events -> early return, no rebuild queued");
        let map_after = crate::communities::list_network_map(&conn, "ws_1").await.unwrap();
        let ids_after: Vec<String> = map_after.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids_before, ids_after, "unchanged graph -> identical community ids");
    }
}
