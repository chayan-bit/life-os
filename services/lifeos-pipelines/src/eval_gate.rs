//! LLM-as-judge Eval+Gate (issue #96, docs/HARNESS-LOOP.md §2).
//!
//! Upgrades the `gate: "eval"` stage from a length heuristic
//! (`HeuristicJudge`, kept as the always-available fallback) to a real
//! Haiku judge call, content-cached (BLAKE3 hash -> one `entities` row,
//! "zero new tables") and sampled (deterministic hash-based sampling, not
//! `rand`, so tests stay reproducible). The Anthropic request itself goes
//! through the crate's shared `anthropic::AnthropicClient` (audit finding
//! 30) rather than a hand-rolled `reqwest` call.

use crate::anthropic::AnthropicClient;
use async_trait::async_trait;
use libsql::{params, Connection};
use serde_json::Value;

/// The judge rubric, `pub` so `lifeos-drain`'s agent-CLI judge reuses this
/// single copy instead of duplicating a near-identical string (audit
/// finding 31).
pub const JUDGE_RUBRIC: &str = "You are a quality judge for an AI pipeline stage's output. \
Score 1-5 on whether it is complete, non-empty, and usable as-is (5 = ship it, 1 = empty or unusable). \
Respond with ONLY strict JSON: {\"score\": <1-5>, \"rationale\": \"<one sentence>\"}.";

/// Produces a 0.0-1.0 quality score + one-sentence rationale for a stage's
/// output text.
#[async_trait]
pub trait Judge: Send + Sync {
    async fn score(&self, content: &str) -> Result<(f64, String), String>;
}

/// The original `eval_stage_output` heuristic (issue #92), demoted from
/// "the gate" to "the fallback": used when no real judge is configured, a
/// sampled-out call, or a judge call that errored. Behavior is unchanged
/// from #92 in the no-`ANTHROPIC_API_KEY` case.
pub struct HeuristicJudge;

#[async_trait]
impl Judge for HeuristicJudge {
    async fn score(&self, content: &str) -> Result<(f64, String), String> {
        let text_len = content.trim().len();
        let (score, rationale) = if text_len == 0 {
            (0.0, "output is empty".to_string())
        } else if text_len < 10 {
            (0.2, "output is too short to be usable".to_string())
        } else {
            (1.0, "output passes the length heuristic".to_string())
        };
        Ok((score, rationale))
    }
}

/// Real judging via the Anthropic Messages API (Haiku), through the shared
/// `AnthropicClient`.
pub struct HaikuJudge {
    pub api_key: String,
}

#[async_trait]
impl Judge for HaikuJudge {
    async fn score(&self, content: &str) -> Result<(f64, String), String> {
        let client = AnthropicClient::new(self.api_key.clone());
        let completion = client.complete(Some(JUDGE_RUBRIC), content, 200).await?;
        parse_judge_json(&completion.text)
    }
}

/// Parse a judge model's reply (`{"score": 1-5, "rationale": "..."}`) into a
/// normalized 0.0-1.0 score + rationale. Tolerant of a ```json code fence,
/// which agent CLIs sometimes add and a raw API call never does - so it is
/// the one parser both the API judge here and `lifeos-drain`'s agent-CLI
/// judge share (audit finding 31). `pub` for that cross-crate reuse.
pub fn parse_judge_json(text: &str) -> Result<(f64, String), String> {
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let judged: Value = serde_json::from_str(cleaned)
        .map_err(|e| format!("judge response was not valid JSON: {e} (text: {text})"))?;
    let raw_score = judged
        .get("score")
        .and_then(|s| s.as_f64())
        .ok_or_else(|| "judge response missing numeric score".to_string())?;
    let rationale = judged
        .get("rationale")
        .and_then(|r| r.as_str())
        .unwrap_or("no rationale given")
        .to_string();
    Ok(((raw_score / 5.0).clamp(0.0, 1.0), rationale))
}

/// BLAKE3 content hash used both as the cache key and the sampling seed.
pub fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

/// Deterministic sampling: no `rand`, no wall-clock - the same content
/// always makes the same sample/no-sample decision, so tests are
/// reproducible and re-judging the identical output twice can't flip
/// between the cheap and expensive path.
pub fn should_sample(hash_hex: &str, rate: f64) -> bool {
    let first_byte = u8::from_str_radix(&hash_hex[0..2], 16).unwrap_or(255);
    (first_byte as f64 / 255.0) < rate
}

async fn get_cached(
    conn: &Connection,
    workspace_id: &str,
    hash: &str,
) -> Option<(f64, String)> {
    let id = format!("eval_cache_{hash}");
    let mut rows = conn
        .query(
            "SELECT attrs FROM entities WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let attrs_str: String = row.get(0).ok()?;
    let attrs: Value = serde_json::from_str(&attrs_str).ok()?;
    let score = attrs.get("score")?.as_f64()?;
    let rationale = attrs.get("rationale")?.as_str()?.to_string();
    Some((score, rationale))
}

async fn set_cached(
    conn: &Connection,
    workspace_id: &str,
    hash: &str,
    score: f64,
    rationale: &str,
    now: i64,
) {
    let id = format!("eval_cache_{hash}");
    let attrs = serde_json::json!({ "score": score, "rationale": rationale, "hash": hash });
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    let _ = conn
        .execute(
            "INSERT INTO entities (id, workspace_id, module, type, attrs, source, created_at, updated_at) \
             VALUES (?1, ?2, 'harness', 'eval_cache', ?3, 'lifeos-pipelines', ?4, ?4) \
             ON CONFLICT(id) DO UPDATE SET attrs = excluded.attrs, updated_at = excluded.updated_at",
            params![id, workspace_id, attrs_str, now],
        )
        .await;
}

/// The gate entry point: cache hit -> cached result; else sampled -> real
/// judge (falls back to the heuristic on any judge `Err`); else the
/// heuristic directly. Always returns a usable score - a judging failure
/// must never fail the pipeline run.
pub async fn judge_stage_output(
    conn: &Connection,
    workspace_id: &str,
    judge: &(dyn Judge + Sync),
    heuristic: &HeuristicJudge,
    content: &str,
    sample_rate: f64,
    now: i64,
) -> (f64, String) {
    let hash = content_hash(content);

    if let Some(cached) = get_cached(conn, workspace_id, &hash).await {
        return cached;
    }

    let (score, rationale) = if should_sample(&hash, sample_rate) {
        match judge.score(content).await {
            Ok(result) => result,
            Err(_) => heuristic
                .score(content)
                .await
                .unwrap_or((0.0, "judge unavailable".to_string())),
        }
    } else {
        heuristic
            .score(content)
            .await
            .unwrap_or((0.0, "judge unavailable".to_string()))
    };

    set_cached(conn, workspace_id, &hash, score, &rationale, now).await;
    (score, rationale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Builder;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn fresh_conn(path: &str) -> Connection {
        let _ = std::fs::remove_file(path);
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE entities (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT, type TEXT, \
                parent_id TEXT, title TEXT, status TEXT, attrs TEXT NOT NULL DEFAULT '{}', \
                source TEXT, blob_ref TEXT, created_at INTEGER, updated_at INTEGER)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    #[tokio::test]
    async fn heuristic_judge_scores_empty_short_and_normal_text() {
        let h = HeuristicJudge;
        assert_eq!(h.score("").await.unwrap().0, 0.0);
        assert_eq!(h.score("   ").await.unwrap().0, 0.0);
        assert_eq!(h.score("hi").await.unwrap().0, 0.2);
        assert_eq!(h.score("a fully fleshed out draft").await.unwrap().0, 1.0);
    }

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash("same text"), content_hash("same text"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }

    #[test]
    fn parse_judge_json_reads_plain_fenced_and_clamps() {
        let (score, rationale) = parse_judge_json(r#"{"score": 4, "rationale": "solid"}"#).unwrap();
        assert!((score - 0.8).abs() < 1e-9);
        assert_eq!(rationale, "solid");

        let fenced = "```json\n{\"score\": 5, \"rationale\": \"ship it\"}\n```";
        assert!((parse_judge_json(fenced).unwrap().0 - 1.0).abs() < 1e-9);

        // Out-of-range score is clamped into 0.0-1.0.
        assert!((parse_judge_json(r#"{"score": 9, "rationale": "x"}"#).unwrap().0 - 1.0).abs() < 1e-9);

        assert!(parse_judge_json("not json").is_err());
    }

    #[test]
    fn should_sample_is_deterministic_and_rate_bounded() {
        let hash = content_hash("some pipeline output");
        assert!(should_sample(&hash, 1.0));
        assert!(!should_sample(&hash, 0.0));
        assert_eq!(should_sample(&hash, 0.5), should_sample(&hash, 0.5));
    }

    #[tokio::test]
    async fn cache_roundtrip() {
        let conn = fresh_conn("/tmp/e96-eval-cache-test.db").await;
        let hash = content_hash("cache me");
        assert!(get_cached(&conn, "ws", &hash).await.is_none());
        set_cached(&conn, "ws", &hash, 0.7, "decent", 100).await;
        let cached = get_cached(&conn, "ws", &hash).await.unwrap();
        assert_eq!(cached, (0.7, "decent".to_string()));
    }

    struct AlwaysErrJudge;
    #[async_trait]
    impl Judge for AlwaysErrJudge {
        async fn score(&self, _content: &str) -> Result<(f64, String), String> {
            Err("judge down".to_string())
        }
    }

    struct FixedScoreJudge {
        score: f64,
        rationale: &'static str,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Judge for FixedScoreJudge {
        async fn score(&self, _content: &str) -> Result<(f64, String), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok((self.score, self.rationale.to_string()))
        }
    }

    #[tokio::test]
    async fn judge_stage_output_falls_back_to_heuristic_on_judge_error() {
        let conn = fresh_conn("/tmp/e96-eval-gate-errfallback.db").await;
        let heuristic = HeuristicJudge;
        let (score, _) = judge_stage_output(
            &conn,
            "ws",
            &AlwaysErrJudge,
            &heuristic,
            "a fully fleshed out draft",
            1.0,
            100,
        )
        .await;
        assert_eq!(score, 1.0);
    }

    #[tokio::test]
    async fn judge_stage_output_uses_real_judge_when_sampled() {
        let conn = fresh_conn("/tmp/e96-eval-gate-sampled.db").await;
        let heuristic = HeuristicJudge;
        let judge = FixedScoreJudge { score: 0.1, rationale: "too thin", calls: AtomicUsize::new(0) };
        let (score, rationale) =
            judge_stage_output(&conn, "ws", &judge, &heuristic, "some output", 1.0, 100).await;
        assert_eq!(score, 0.1);
        assert_eq!(rationale, "too thin");
        assert_eq!(judge.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn judge_stage_output_second_call_hits_cache_not_judge() {
        let conn = fresh_conn("/tmp/e96-eval-gate-cachehit.db").await;
        let heuristic = HeuristicJudge;
        let judge = FixedScoreJudge { score: 0.9, rationale: "great", calls: AtomicUsize::new(0) };
        let _ = judge_stage_output(&conn, "ws", &judge, &heuristic, "repeat me", 1.0, 100).await;
        let (score, rationale) =
            judge_stage_output(&conn, "ws", &judge, &heuristic, "repeat me", 1.0, 200).await;
        assert_eq!(score, 0.9);
        assert_eq!(rationale, "great");
        assert_eq!(judge.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn judge_stage_output_not_sampled_uses_heuristic_without_calling_judge() {
        let conn = fresh_conn("/tmp/e96-eval-gate-notsampled.db").await;
        let heuristic = HeuristicJudge;
        let judge = FixedScoreJudge { score: 0.9, rationale: "great", calls: AtomicUsize::new(0) };
        let (score, _) =
            judge_stage_output(&conn, "ws", &judge, &heuristic, "a fully fleshed out draft", 0.0, 100).await;
        assert_eq!(score, 1.0);
        assert_eq!(judge.calls.load(Ordering::SeqCst), 0);
    }
}
