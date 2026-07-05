//! Shared `server/memvec.py` subprocess seam - the ONE place that shells the
//! semantic half of hybrid recall, used by both `/api/search` (entity vectors)
//! and the memory engine's vector lane (`mn_*` node vectors).
//!
//! vec0/sqlite-vec is NOT loadable from the Rust libSQL build, so `memvec.py`
//! owns the `entity_vec` index and we talk to it over a bounded, best-effort
//! subprocess. Every failure mode (env unset, python missing, non-zero exit,
//! timeout, garbage output) degrades to "no vector hits" - it NEVER fails the
//! caller, so recall/search always fall back to lexical-only.
//!
//! Memory-node vectors are kept separate from entity vectors by labelling them
//! with a `mem:<ws>` workspace (mirroring how #123 partitioned Tool-RAG with a
//! `_system_tools` label): `/api/search` queries plain `<ws>` and only ever
//! sees entity ids; the memory lane queries `mem:<ws>` and only ever sees
//! `mn_*` ids. The two never bleed into each other's results.

use crate::ids::now_secs;
use async_trait::async_trait;
use libsql::Connection;
use lifeos_memory::{MemoryError, VectorHit, VectorSearcher};
use std::time::Duration;

/// Same bound `/api/search` used before this was factored out.
const VECTOR_TIMEOUT: Duration = Duration::from_secs(20);
/// Each embed reloads MiniLM in a fresh python process, so keep the per-call
/// batch small; the marker table means the backlog only ever shrinks.
const EMBED_BATCH_MAX: usize = 16;

/// Memory vectors live under this workspace label so they never mix with
/// entity vectors in the shared `entity_vec` index.
pub fn mem_label(workspace_id: &str) -> String {
    format!("mem:{workspace_id}")
}

/// Runs `python3 <memvec> <subcommand> --db <derived> <args...>`, bounded by a
/// timeout. Returns stdout on a clean exit, `None` on ANY failure. Injected so
/// the parsing/wiring is unit-testable without spawning python.
#[async_trait]
pub trait MemvecRunner: Send + Sync {
    async fn run(&self, subcommand: &str, args: &[&str]) -> Option<String>;
}

/// The real subprocess runner. `from_env` returns `None` (the graceful-degrade
/// signal) when `LIFEOS_MEMVEC` is unset/empty - identical env-gating to the
/// original `/api/search` path.
pub struct SubprocessMemvec {
    memvec_path: String,
    derived_db: String,
    timeout: Duration,
}

impl SubprocessMemvec {
    pub fn from_env(derived_db: &str) -> Option<Self> {
        let memvec_path = std::env::var("LIFEOS_MEMVEC").ok().filter(|s| !s.is_empty())?;
        Some(Self { memvec_path, derived_db: derived_db.to_string(), timeout: VECTOR_TIMEOUT })
    }
}

#[async_trait]
impl MemvecRunner for SubprocessMemvec {
    async fn run(&self, subcommand: &str, args: &[&str]) -> Option<String> {
        // Arg order mirrors the original /api/search invocation exactly:
        // `python3 memvec.py <subcommand> --db <derived> <args...>`.
        let mut cmd = tokio::process::Command::new("python3");
        cmd.arg(&self.memvec_path)
            .arg(subcommand)
            .args(["--db", &self.derived_db])
            .args(args)
            .stdin(std::process::Stdio::null());
        let output = tokio::time::timeout(self.timeout, cmd.output()).await.ok()?.ok()?;
        if !output.status.success() {
            tracing::warn!("memvec {subcommand} failed (degraded to lexical-only)");
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Parse memvec `query` stdout (`id<TAB>distance` per line, best first) into
/// its ranked id list, dropping blank/garbage lines.
pub fn parse_ranked_ids(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.split('\t').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Best-first semantic neighbour ids for a workspace label. `None` = the lane
/// was unavailable (degrade to lexical-only); `Some(vec![])` = it ran and found
/// nothing.
pub async fn query_ids(
    runner: &dyn MemvecRunner,
    workspace_label: &str,
    text: &str,
    k: u32,
) -> Option<Vec<String>> {
    let k = k.to_string();
    let stdout = runner
        .run("query", &["--workspace", workspace_label, "--k", &k, "--text", text])
        .await?;
    Some(parse_ranked_ids(&stdout))
}

/// The memory engine's semantic lane: a [`VectorSearcher`] backed by the memvec
/// subprocess, scoped to the `mem:<ws>` label so it only ever returns `mn_*`
/// ids. Degrades to empty (lexical-only) on any failure - never errors.
pub struct MemvecSearcher {
    runner: Box<dyn MemvecRunner>,
}

impl MemvecSearcher {
    pub fn new(runner: Box<dyn MemvecRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl VectorSearcher for MemvecSearcher {
    async fn search(
        &self,
        workspace_id: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<VectorHit>, MemoryError> {
        let label = mem_label(workspace_id);
        let Some(ids) = query_ids(self.runner.as_ref(), &label, query, k as u32).await else {
            return Ok(Vec::new()); // graceful degrade -> lexical-only
        };
        Ok(ids.into_iter().enumerate().map(|(rank, id)| VectorHit { id, rank }).collect())
    }
}

/// Best-effort: embed memory nodes that aren't in the vector index yet, so the
/// lane has material to search. No-op when `LIFEOS_MEMVEC` is unset. Bounded
/// per call; embedding state is tracked in the derived DB (`d.memory_embedded`)
/// and, because node ids are content-deterministic, an embedding stays valid
/// across rebuilds and never needs invalidating. Requires the derived schema
/// `d` to be attached (always true in the API); returns `Ok(0)` when the lane
/// is disabled, so callers can `let _ =` it safely.
pub async fn embed_new_nodes(
    conn: &Connection,
    derived_db: &str,
    workspace_id: &str,
) -> Result<usize, libsql::Error> {
    let Some(runner) = SubprocessMemvec::from_env(derived_db) else { return Ok(0) };

    let mut rows = conn
        .query(
            "SELECT n.id, n.content FROM memory_nodes n \
             LEFT JOIN d.memory_embedded e ON e.id = n.id \
             WHERE n.workspace_id = ?1 AND n.tiered_ref IS NULL AND e.id IS NULL \
             ORDER BY n.ts DESC LIMIT ?2",
            libsql::params![workspace_id, EMBED_BATCH_MAX as i64],
        )
        .await?;
    let mut pending: Vec<(String, String)> = Vec::new();
    while let Some(row) = rows.next().await? {
        pending.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }

    let label = mem_label(workspace_id);
    let now = now_secs();
    let mut embedded = 0;
    for (id, content) in pending {
        if runner.run("embed", &["--workspace", &label, "--id", &id, "--text", &content]).await.is_some() {
            conn.execute(
                "INSERT OR IGNORE INTO d.memory_embedded (id, workspace_id, embedded_ts) \
                 VALUES (?1, ?2, ?3)",
                libsql::params![id, workspace_id, now],
            )
            .await?;
            embedded += 1;
        }
    }
    Ok(embedded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A runner whose stdout is scripted, recording the args it was asked to
    /// run - the subprocess seam other suites mock the same way.
    struct FakeRunner {
        stdout: Option<String>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn ok(stdout: &str) -> Self {
            Self { stdout: Some(stdout.to_string()), calls: Mutex::new(Vec::new()) }
        }
        fn degraded() -> Self {
            Self { stdout: None, calls: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait]
    impl MemvecRunner for FakeRunner {
        async fn run(&self, subcommand: &str, args: &[&str]) -> Option<String> {
            self.calls
                .lock()
                .unwrap()
                .push((subcommand.to_string(), args.iter().map(|s| s.to_string()).collect()));
            self.stdout.clone()
        }
    }

    #[test]
    fn parses_id_tab_distance_lines_and_ignores_noise() {
        let ids = parse_ranked_ids("mn_a\t0.10\nmn_b\t0.22\n\n   \nmn_c\t0.31\n");
        assert_eq!(ids, vec!["mn_a", "mn_b", "mn_c"]);
    }

    #[tokio::test]
    async fn searcher_maps_lines_to_dense_ranks_under_the_mem_label() {
        let runner = FakeRunner::ok("mn_a\t0.10\nmn_b\t0.22\n");
        let searcher = MemvecSearcher::new(Box::new(runner));
        let hits = searcher.search("ws_1", "gamma exposure", 8).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "mn_a");
        assert_eq!(hits[0].rank, 0);
        assert_eq!(hits[1].id, "mn_b");
        assert_eq!(hits[1].rank, 1);
    }

    #[tokio::test]
    async fn searcher_degrades_to_empty_without_erroring() {
        let searcher = MemvecSearcher::new(Box::new(FakeRunner::degraded()));
        let hits = searcher.search("ws_1", "anything", 8).await.unwrap();
        assert!(hits.is_empty(), "a failed subprocess must yield no hits, not an error");
    }

    #[tokio::test]
    async fn query_ids_uses_the_given_workspace_label() {
        let runner = FakeRunner::ok("mn_a\t0.1\n");
        let out = query_ids(&runner, &mem_label("ws_1"), "q", 5).await.unwrap();
        assert_eq!(out, vec!["mn_a"]);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "query");
        assert!(calls[0].1.contains(&"mem:ws_1".to_string()), "{:?}", calls[0].1);
    }
}
