//! GraphRAG global queries (issue #139, docs/AGENT-CORE.md §13,
//! docs/AI-MEMORY.md): community detection + cluster summaries over the
//! whole entity/memory graph. `graph.rs` already does local 1-2-hop
//! spreading activation for a specific query; this module adds the GLOBAL
//! lens - "how is my world connected / which parts touch X" - by clustering
//! the same `memory_edges` graph with deterministic label propagation and
//! summarizing each cluster.
//!
//! Like every other memory read model, `memory_communities` is DERIVED and
//! rebuildable: it is wiped and reinserted per workspace on every sleep
//! cycle (`consolidate::run_sleep_cycle`), never mutated in place.
//!
//! Summarization sits behind the `CommunitySummarizer` trait, mirroring the
//! `MemoryModel`/`PolicyLearner` seams elsewhere in this crate: the
//! heuristic default below does no network I/O, so a Haiku-backed
//! summarizer is a drop-in replacement with no architectural change.
//! `ask_network` itself never calls a model - it returns grounded,
//! deterministically-ranked community material; the calling agent composes
//! the final answer.

use crate::error::MemoryError;
use libsql::{params, Connection};
use std::collections::{BTreeMap, HashSet};

/// Deterministic upper bound on label-propagation rounds - no convergence
/// check beyond "labels stopped changing" is needed, but a hard cap keeps
/// the pass bounded even on a pathological graph.
const MAX_ITERATIONS: usize = 10;
/// Singleton "communities" (a node with no same-label neighbors) carry no
/// cross-entity signal worth summarizing.
const MIN_COMMUNITY_SIZE: usize = 2;
/// Snippet titles are truncated to this many chars so summaries stay compact.
const SNIPPET_MAX_CHARS: usize = 80;
/// Only tokens at least this long count toward relevance scoring - filters
/// stopword-ish noise the same way `consolidate::word_set` does.
const MIN_TERM_CHARS: usize = 3;

/// One detected cluster: a stable id plus the graph-vertex ids (memory node
/// or entity ids) that belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Community {
    pub id: String,
    pub member_entity_ids: Vec<String>,
}

/// A compact, human-readable stand-in for one member, fed to the summarizer.
#[derive(Debug, Clone)]
pub struct MemberSnippet {
    pub id: String,
    pub title: String,
}

/// The summarization seam (mirrors `PolicyLearner`/`MemoryModel`). Pure and
/// synchronous by contract - a network-backed impl wraps its own blocking or
/// pre-fetches, keeping this trait boundary simple and testable.
pub trait CommunitySummarizer: Send + Sync {
    fn summarize(&self, members: &[MemberSnippet]) -> Result<String, MemoryError>;
}

/// Deterministic, LLM-free default: member titles, sorted, concatenated into
/// one compact line. A real summarizer slots in behind the same trait later.
pub struct HeuristicSummarizer;

impl CommunitySummarizer for HeuristicSummarizer {
    fn summarize(&self, members: &[MemberSnippet]) -> Result<String, MemoryError> {
        let mut titles: Vec<String> = members.iter().map(|m| m.title.clone()).collect();
        titles.sort();
        Ok(format!("Cluster of {} items: {}", members.len(), titles.join("; ")))
    }
}

/// One row of the network map, as persisted.
#[derive(Debug, Clone)]
pub struct NetworkCommunity {
    pub id: String,
    pub summary: String,
    pub size: usize,
    pub member_ids: Vec<String>,
}

/// One ranked result from `ask_network` - grounded material, not an answer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkAnswerHit {
    pub community_id: String,
    pub summary: String,
    pub members: Vec<String>,
    pub score: f64,
}

fn short_hash(input: &str) -> String {
    blake3::hash(input.as_bytes()).to_hex()[..24].to_string()
}

/// Deterministic community id from its (sorted) member set, so an unchanged
/// cluster reproduces the same row across rebuilds.
fn community_id(ws: &str, members: &[String]) -> String {
    let mut sorted = members.to_vec();
    sorted.sort();
    format!("mc_{}", short_hash(&format!("{ws}|{}", sorted.join(","))))
}

/// Load the undirected adjacency of currently-true (`t_invalid IS NULL`)
/// `memory_edges` for the workspace. A `BTreeMap` keeps both the vertex set
/// and each neighbor list in a fixed, sorted order - the basis for
/// deterministic label propagation (docs/AGENT-CORE.md §13: "no randomness").
async fn load_adjacency(
    conn: &Connection,
    ws: &str,
) -> Result<BTreeMap<String, Vec<String>>, MemoryError> {
    let mut rows = conn
        .query(
            "SELECT from_id, to_id FROM memory_edges \
             WHERE workspace_id = ?1 AND t_invalid IS NULL ORDER BY id",
            params![ws],
        )
        .await?;
    let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let from: String = row.get(0)?;
        let to: String = row.get(1)?;
        adjacency.entry(from.clone()).or_default().push(to.clone());
        adjacency.entry(to.clone()).or_default().push(from.clone());
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
        neighbors.dedup();
    }
    Ok(adjacency)
}

/// Synchronous, pure label propagation over a pre-loaded adjacency map.
/// Deterministic by construction: nodes are visited in sorted-id order every
/// round, and ties for the most common neighbor label break on the smallest
/// label id - no randomness, no dependency on HashMap iteration order.
fn label_propagation(adjacency: &BTreeMap<String, Vec<String>>, ws: &str) -> Vec<Community> {
    let nodes: Vec<String> = adjacency.keys().cloned().collect();
    let mut labels: BTreeMap<String, String> =
        nodes.iter().map(|n| (n.clone(), n.clone())).collect();

    for _ in 0..MAX_ITERATIONS {
        let mut changed = false;
        for node in &nodes {
            let neighbors = &adjacency[node];
            if neighbors.is_empty() {
                continue;
            }
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for neighbor in neighbors {
                *counts.entry(labels[neighbor].clone()).or_insert(0) += 1;
            }
            let max_count = *counts.values().max().unwrap_or(&0);
            let best_label = counts
                .into_iter()
                .filter(|(_, count)| *count == max_count)
                .map(|(label, _)| label)
                .min()
                .expect("neighbors is non-empty, so counts is non-empty");
            if labels[node] != best_label {
                labels.insert(node.clone(), best_label);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in &nodes {
        groups.entry(labels[node].clone()).or_default().push(node.clone());
    }

    let mut communities: Vec<Community> = groups
        .into_values()
        .filter(|members| members.len() >= MIN_COMMUNITY_SIZE)
        .map(|members| Community { id: community_id(ws, &members), member_entity_ids: members })
        .collect();
    communities.sort_by(|a, b| a.id.cmp(&b.id));
    communities
}

/// Run label propagation over the workspace's current entity/memory graph.
pub async fn detect_communities(conn: &Connection, ws: &str) -> Result<Vec<Community>, MemoryError> {
    let adjacency = load_adjacency(conn, ws).await?;
    Ok(label_propagation(&adjacency, ws))
}

async fn memory_node_title(conn: &Connection, ws: &str, id: &str) -> Result<Option<String>, MemoryError> {
    let mut rows = conn
        .query(
            "SELECT content FROM memory_nodes WHERE workspace_id = ?1 AND id = ?2",
            params![ws, id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// `module` (the entity's domain, e.g. 'trading') plus its title/type - the
/// "top entity titles/types" snippet the design calls for, so a thematic
/// question naming the domain ("trading") has literal vocabulary to match
/// against, not just whatever the title happens to say.
async fn entity_title(conn: &Connection, ws: &str, id: &str) -> Result<Option<String>, MemoryError> {
    let mut rows = conn
        .query(
            "SELECT module, COALESCE(title, type) FROM entities WHERE workspace_id = ?1 AND id = ?2",
            params![ws, id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => {
            let module: String = row.get(0)?;
            let title: String = row.get(1)?;
            Some(format!("{module} {title}"))
        }
        None => None,
    })
}

/// A compact snippet for one graph vertex: memory nodes contribute their
/// content, entities their title/type, anything else falls back to its id.
async fn member_snippet(conn: &Connection, ws: &str, member_id: &str) -> Result<MemberSnippet, MemoryError> {
    let title = match memory_node_title(conn, ws, member_id).await? {
        Some(t) => t,
        None => entity_title(conn, ws, member_id).await?.unwrap_or_else(|| member_id.to_string()),
    };
    let truncated: String = title.chars().take(SNIPPET_MAX_CHARS).collect();
    Ok(MemberSnippet { id: member_id.to_string(), title: truncated })
}

/// Rebuild the workspace's `memory_communities` read model: detect clusters,
/// summarize each, delete the old rows, and reinsert - the same
/// delete+reinsert cadence the other derived memory tables follow. Returns
/// the number of communities written.
pub async fn rebuild_communities(
    conn: &Connection,
    ws: &str,
    summarizer: &dyn CommunitySummarizer,
    now: i64,
) -> Result<usize, MemoryError> {
    let communities = detect_communities(conn, ws).await?;

    conn.execute("DELETE FROM memory_communities WHERE workspace_id = ?1", params![ws]).await?;

    for community in &communities {
        let mut snippets = Vec::with_capacity(community.member_entity_ids.len());
        for member_id in &community.member_entity_ids {
            snippets.push(member_snippet(conn, ws, member_id).await?);
        }
        let summary = summarizer.summarize(&snippets)?;
        let member_ids_json = serde_json::to_string(&community.member_entity_ids)
            .map_err(|e| MemoryError::Other(format!("serialize community members: {e}")))?;
        conn.execute(
            "INSERT INTO memory_communities (id, workspace_id, member_ids, summary, size, built_ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                community.id.clone(),
                ws,
                member_ids_json,
                summary,
                community.member_entity_ids.len() as i64,
                now
            ],
        )
        .await?;
    }
    Ok(communities.len())
}

/// All current communities + summaries for the workspace, id order (stable).
pub async fn list_network_map(conn: &Connection, ws: &str) -> Result<Vec<NetworkCommunity>, MemoryError> {
    let mut rows = conn
        .query(
            "SELECT id, summary, size, member_ids FROM memory_communities \
             WHERE workspace_id = ?1 ORDER BY id",
            params![ws],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let member_ids_json: String = row.get(3)?;
        out.push(NetworkCommunity {
            id: row.get(0)?,
            summary: row.get(1)?,
            size: row.get::<i64>(2)? as usize,
            member_ids: serde_json::from_str(&member_ids_json).unwrap_or_default(),
        });
    }
    Ok(out)
}

fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_TERM_CHARS)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Naive, deterministic term-overlap score of a question against one
/// community's summary + member ids - enough to rank clusters without a
/// model in the loop (map-reduce lite: the agent does the actual reduce).
fn relevance_score(question_terms: &HashSet<String>, community: &NetworkCommunity) -> f64 {
    if question_terms.is_empty() {
        return 0.0;
    }
    let haystack = format!("{} {}", community.summary, community.member_ids.join(" "));
    let haystack_terms = word_set(&haystack);
    if haystack_terms.is_empty() {
        return 0.0;
    }
    question_terms.intersection(&haystack_terms).count() as f64 / question_terms.len() as f64
}

/// Map-reduce lite over the network map: rank communities by relevance to
/// `question`, return the top `k` as grounded material. This function never
/// calls a model - the caller (agent) composes the final answer from the
/// returned summaries.
pub async fn ask_network(
    conn: &Connection,
    ws: &str,
    question: &str,
    k: usize,
) -> Result<Vec<NetworkAnswerHit>, MemoryError> {
    let communities = list_network_map(conn, ws).await?;
    let terms = word_set(question);

    let mut scored: Vec<(f64, NetworkCommunity)> = communities
        .into_iter()
        .map(|c| {
            let score = relevance_score(&terms, &c);
            (score, c)
        })
        .collect();
    // Deterministic order: highest score first, id as the tiebreak.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.id.cmp(&b.1.id))
    });

    Ok(scored
        .into_iter()
        .take(k)
        .map(|(score, c)| NetworkAnswerHit {
            community_id: c.id,
            summary: c.summary,
            members: c.member_ids,
            score,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::project_workspace;
    use crate::testutil::{seed_entity, seed_event, test_conn};
    use serde_json::json;

    /// Two obvious clusters bridged by a shared entity: trading events hang
    /// off `ent_trading`, learning events off `ent_learning`, and one bridge
    /// event links the two subjects. Label propagation should still resolve
    /// two communities, not one, because the bridge is a single thin edge.
    async fn seed_two_clusters(conn: &libsql::Connection) {
        seed_entity(conn, "ws_1", "ent_trading", "trading", "Banknifty swing desk").await;
        seed_entity(conn, "ws_1", "ent_learning", "learning", "Market microstructure").await;

        for i in 0..3 {
            seed_event(
                conn, "ws_1", &format!("evt_trade_{i}"), 100 + i, "trade.closed",
                Some("ent_trading"), "user", json!({"note": format!("trade {i} closed")}), None,
            )
            .await;
        }
        for i in 0..3 {
            seed_event(
                conn, "ws_1", &format!("evt_study_{i}"), 200 + i, "study.review",
                Some("ent_learning"), "user", json!({"note": format!("reviewed topic {i}")}), None,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn label_propagation_finds_two_clusters_and_ignores_singletons() {
        let conn = test_conn().await;
        seed_two_clusters(&conn).await;
        // A lone, unrelated event with no shared entity - a singleton in the
        // graph, must never surface as its own "community".
        seed_event(&conn, "ws_1", "evt_lonely", 999, "note.captured", None, "user",
            json!({"text": "an isolated thought"}), None)
            .await;
        project_workspace(&conn, "ws_1").await.unwrap();

        let communities = detect_communities(&conn, "ws_1").await.unwrap();
        assert_eq!(communities.len(), 2, "trading cluster + learning cluster, bridge excluded (no shared entity)");
        for c in &communities {
            assert!(c.member_entity_ids.len() >= MIN_COMMUNITY_SIZE);
        }

        // Deterministic across repeated runs on the same graph.
        let again = detect_communities(&conn, "ws_1").await.unwrap();
        assert_eq!(communities, again);
    }

    #[tokio::test]
    async fn heuristic_summarizer_is_deterministic_regardless_of_input_order() {
        let a = vec![
            MemberSnippet { id: "1".into(), title: "beta".into() },
            MemberSnippet { id: "2".into(), title: "alpha".into() },
        ];
        let b = vec![
            MemberSnippet { id: "2".into(), title: "alpha".into() },
            MemberSnippet { id: "1".into(), title: "beta".into() },
        ];
        let summarizer = HeuristicSummarizer;
        assert_eq!(summarizer.summarize(&a).unwrap(), summarizer.summarize(&b).unwrap());
        assert!(summarizer.summarize(&a).unwrap().starts_with("Cluster of 2 items: alpha; beta"));
    }

    #[tokio::test]
    async fn rebuild_communities_persists_and_is_idempotent() {
        let conn = test_conn().await;
        seed_two_clusters(&conn).await;
        project_workspace(&conn, "ws_1").await.unwrap();

        let n = rebuild_communities(&conn, "ws_1", &HeuristicSummarizer, 1000).await.unwrap();
        assert_eq!(n, 2);
        let map = list_network_map(&conn, "ws_1").await.unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.iter().all(|c| !c.summary.is_empty() && c.size >= MIN_COMMUNITY_SIZE));

        // Re-running (delete+reinsert) on an unchanged graph reproduces the
        // same rows - the rebuildable-read-model invariant this crate holds
        // everywhere else.
        rebuild_communities(&conn, "ws_1", &HeuristicSummarizer, 2000).await.unwrap();
        let map_again = list_network_map(&conn, "ws_1").await.unwrap();
        let ids: Vec<&str> = map.iter().map(|c| c.id.as_str()).collect();
        let ids_again: Vec<&str> = map_again.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ids_again, "same graph -> same community ids across rebuilds");
    }

    #[tokio::test]
    async fn ask_network_ranks_the_trading_cluster_first_for_a_trading_question() {
        let conn = test_conn().await;
        seed_two_clusters(&conn).await;
        project_workspace(&conn, "ws_1").await.unwrap();
        rebuild_communities(&conn, "ws_1", &HeuristicSummarizer, 1000).await.unwrap();

        let hits = ask_network(&conn, "ws_1", "which parts of my world touch trading?", 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].summary.to_lowercase().contains("trade"),
            "top hit must be the trading cluster: {}",
            hits[0].summary
        );
    }

    #[tokio::test]
    async fn workspace_isolation_holds_for_the_network_map() {
        let conn = test_conn().await;
        seed_two_clusters(&conn).await;
        project_workspace(&conn, "ws_1").await.unwrap();
        rebuild_communities(&conn, "ws_1", &HeuristicSummarizer, 1000).await.unwrap();

        assert!(list_network_map(&conn, "ws_2").await.unwrap().is_empty());
        assert!(ask_network(&conn, "ws_2", "trading", 5).await.unwrap().is_empty());
    }
}
