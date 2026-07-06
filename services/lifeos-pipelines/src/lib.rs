//! lifeos-pipelines: the agent DAG orchestrator (issue #92,
//! docs/PLATFORM-SYSTEMS.md §1.2).
//!
//! `lifeos-drain` claims `pipeline` jobs from `jobs` and calls
//! `process_pipeline_job` directly as a library, the same shape
//! `lifeos-ingest::process_ingest_job` already uses. This crate has no
//! dependency on `lifeos-api` (same standalone style as `lifeos-drain`/
//! `lifeos-ingest`): it reads/writes `entities`/`events` with its own small
//! SQL, mirroring `audit::emit`'s INSERT shape by hand - extended to the
//! harness run-log columns (`run_id/tier/model/tokens_in/tokens_out/cost/
//! latency_ms/error/outcome/eval_score/gated`) since this is the first
//! crate that actually needs them.
//!
//! Scope of #92: a real (not full-manifest-driven) static pipeline
//! registry, a DI-trait stage runner (no Rust "Claude Agent SDK" exists
//! anywhere in this workspace - the established pattern, same as
//! `lifeos-ingest/src/vision.rs::HaikuCaptioner`, is a direct `reqwest`
//! call to the Anthropic Messages API), per-stage `events`, and an
//! unconditional draft-and-halt gate for any stage marked `gated: true`
//! (matching every other "only ever drafts" gated write in this codebase -
//! `integrations.rs::draft_action`, `whatsapp.rs`, `slack.rs`, `drive.rs`,
//! `travel.rs`). The full LLM-as-judge Eval/Observe/Release system in
//! docs/HARNESS-LOOP.md §2-4 is a separate, larger, unbuilt system; the
//! `gate: "eval"` stage here uses a real but intentionally minimal
//! heuristic, not that system.
//!
//! Manifest-driven pipeline registration (reading a `pipelines: [...]`
//! array out of a JS module manifest, per docs/PLATFORM-SYSTEMS.md §1.2's
//! example) is a deferred gap: no module manifest declares one yet, and
//! building a JS->Rust manifest bridge for a single documented example
//! would be speculative. `pipeline_registry()` is a hardcoded Rust table
//! today, seeded with the one pipeline the docs actually specify.

use libsql::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use ulid::{Generator, Ulid};

pub mod anthropic;
pub mod eval_gate;
pub mod runner;
pub use eval_gate::{parse_judge_json, HaikuJudge, HeuristicJudge, Judge, JUDGE_RUBRIC};
pub use runner::{build_prompt, HaikuStageRunner, NoopStageRunner, PipelineStageRunner, StageResult};

static ID_GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());

fn new_id(prefix: &str) -> String {
    let ulid = ID_GENERATOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .generate()
        .unwrap_or_else(|_| Ulid::new());
    format!("{prefix}_{ulid}")
}

// --------------------------------------------------------------- spec

/// One stage in a pipeline DAG. Field names match
/// docs/PLATFORM-SYSTEMS.md §1.2's manifest example verbatim.
#[derive(Debug, Clone)]
pub struct StageSpec {
    pub name: &'static str,
    pub agent: &'static str,
    pub tool: Option<&'static str>,
    pub skill: Option<&'static str>,
    pub gate: Option<&'static str>,
    pub gated: bool,
}

#[derive(Debug, Clone)]
pub struct PipelineSpec {
    pub id: &'static str,
    pub stages: Vec<StageSpec>,
}

/// Static pipeline registry - see module doc for why this isn't
/// manifest-driven yet. Seeded with `post-from-topic`, the only pipeline
/// documented anywhere in the repo (docs/PLATFORM-SYSTEMS.md §1.2).
pub fn pipeline_registry() -> HashMap<&'static str, PipelineSpec> {
    let mut m = HashMap::new();
    m.insert(
        "post-from-topic",
        PipelineSpec {
            id: "post-from-topic",
            stages: vec![
                StageSpec {
                    name: "research",
                    agent: "research",
                    tool: Some("memvec.recall"),
                    skill: None,
                    gate: None,
                    gated: false,
                },
                StageSpec {
                    name: "draft",
                    agent: "draft",
                    tool: None,
                    skill: Some("copywriting"),
                    gate: None,
                    gated: false,
                },
                StageSpec {
                    name: "verify",
                    agent: "verify",
                    tool: None,
                    skill: None,
                    gate: Some("eval"),
                    gated: false,
                },
                StageSpec {
                    name: "publish",
                    agent: "publish",
                    tool: Some("social.draft"),
                    skill: None,
                    gate: None,
                    gated: true,
                },
            ],
        },
    );
    m
}

// --------------------------------------------------------------- events

/// Mirrors `routes/event.rs`'s full-column INSERT by hand (this crate has
/// no dependency on `lifeos-api`, same convention as `lifeos-drain`'s
/// `emit_event`/`lifeos-ingest`'s `emit_event`).
#[allow(clippy::too_many_arguments)]
async fn emit_run_event(
    conn: &Connection,
    workspace_id: &str,
    event_type: &str,
    entity_id: &str,
    run_id: &str,
    model: Option<&str>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    latency_ms: Option<i64>,
    error: Option<&str>,
    outcome: Option<&str>,
    eval_score: Option<f64>,
    gated: bool,
    attrs: &Value,
    now: i64,
) -> libsql::Result<()> {
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO events \
         (id, workspace_id, ts, type, entity_id, actor, attrs, run_id, tier, model, \
          tokens_in, tokens_out, latency_ms, error, outcome, eval_score, gated) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'lifeos-pipelines', ?6, ?7, 'mac', ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            new_id("evt"),
            workspace_id,
            now,
            event_type,
            entity_id,
            attrs_str,
            run_id,
            model,
            tokens_in,
            tokens_out,
            latency_ms,
            error,
            outcome,
            eval_score,
            gated as i64
        ],
    )
    .await?;
    Ok(())
}

// --------------------------------------------------------------- eval

/// See `eval_gate` module doc (issue #96) for the real judge/cache/sample
/// pipeline this threshold now gates - the length-only heuristic
/// (`eval_stage_output`, issue #92) lives on as `eval_gate::HeuristicJudge`,
/// the fallback for no-API-key / not-sampled / judge-error cases.
const EVAL_THRESHOLD: f64 = 0.3;

fn stage_output_text(output: &Value) -> String {
    output.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string()
}

// --------------------------------------------------------------- run

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PipelineJobPayload {
    pub pipeline: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineOutcome {
    Completed,
    AwaitingApproval { stage: String },
    Gated { stage: String, rationale: String },
}

#[allow(clippy::too_many_arguments)]
pub async fn process_pipeline_job(
    conn: &Connection,
    workspace_id: &str,
    run_id: &str,
    payload: PipelineJobPayload,
    runner: &(dyn PipelineStageRunner + Sync),
    judge: &(dyn Judge + Sync),
    sample_rate: f64,
    now: i64,
) -> Result<PipelineOutcome, String> {
    let registry = pipeline_registry();
    let spec = registry
        .get(payload.pipeline.as_str())
        .ok_or_else(|| format!("unknown pipeline '{}'", payload.pipeline))?;

    let run_entity_id = new_id("ent");
    let initial_attrs = json!({ "pipeline_id": spec.id, "input": payload.input, "status": "running", "run_id": run_id });
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, attrs, source, created_at, updated_at) \
         VALUES (?1, ?2, 'pipelines', 'pipeline_run', ?3, 'lifeos-pipelines', ?4, ?4)",
        params![
            run_entity_id.clone(),
            workspace_id,
            serde_json::to_string(&initial_attrs).unwrap_or_else(|_| "{}".into()),
            now
        ],
    )
    .await
    .map_err(|e| format!("failed to create pipeline_run entity: {e}"))?;

    let mut prior_outputs: Vec<Value> = Vec::new();

    for stage in &spec.stages {
        if stage.gated {
            let attrs = json!({ "stage": stage.name, "pending_input": prior_outputs.last().cloned().unwrap_or(Value::Null) });
            let approval_id = new_id("ent");
            conn.execute(
                "INSERT INTO entities (id, workspace_id, module, type, parent_id, status, attrs, source, created_at, updated_at) \
                 VALUES (?1, ?2, 'pipelines', 'pending_approval', ?3, 'pending_approval', ?4, 'lifeos-pipelines', ?5, ?5)",
                params![
                    approval_id,
                    workspace_id,
                    run_entity_id.clone(),
                    serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into()),
                    now
                ],
            )
            .await
            .map_err(|e| format!("failed to create pending_approval entity: {e}"))?;

            emit_run_event(
                conn,
                workspace_id,
                "pipeline.stage.gated",
                &run_entity_id,
                run_id,
                None,
                None,
                None,
                None,
                None,
                Some("awaiting_approval"),
                None,
                true,
                &attrs,
                now,
            )
            .await
            .map_err(|e| format!("failed to emit gated event: {e}"))?;

            set_run_status(conn, &run_entity_id, "awaiting_approval", now).await?;
            return Ok(PipelineOutcome::AwaitingApproval { stage: stage.name.to_string() });
        }

        let started = now;
        let result = runner.run_stage(stage, &payload.input, &prior_outputs).await;

        match result {
            Ok(stage_result) => {
                let latency_ms = Some((now - started).max(0));
                let mut eval_score = None;
                let mut gated_now = false;
                let mut rationale = String::new();
                if stage.gate == Some("eval") {
                    let text = stage_output_text(&stage_result.output);
                    let (score, r) = eval_gate::judge_stage_output(
                        conn,
                        workspace_id,
                        judge,
                        &HeuristicJudge,
                        &text,
                        sample_rate,
                        now,
                    )
                    .await;
                    eval_score = Some(score);
                    gated_now = score < EVAL_THRESHOLD;
                    rationale = r;
                }

                emit_run_event(
                    conn,
                    workspace_id,
                    if gated_now { "pipeline.stage.gated" } else { "pipeline.stage.completed" },
                    &run_entity_id,
                    run_id,
                    Some(&stage_result.model),
                    Some(stage_result.tokens_in),
                    Some(stage_result.tokens_out),
                    latency_ms,
                    None,
                    Some(if gated_now { "gated" } else { "completed" }),
                    eval_score,
                    gated_now,
                    &json!({ "stage": stage.name, "output": stage_result.output, "rationale": rationale }),
                    now,
                )
                .await
                .map_err(|e| format!("failed to emit stage event: {e}"))?;

                if gated_now {
                    set_run_status(conn, &run_entity_id, "gated", now).await?;
                    return Ok(PipelineOutcome::Gated { stage: stage.name.to_string(), rationale });
                }

                prior_outputs.push(stage_result.output);
            }
            Err(e) => {
                emit_run_event(
                    conn,
                    workspace_id,
                    "pipeline.stage.failed",
                    &run_entity_id,
                    run_id,
                    None,
                    None,
                    None,
                    None,
                    Some(&e),
                    Some("failed"),
                    None,
                    false,
                    &json!({ "stage": stage.name }),
                    now,
                )
                .await
                .map_err(|e| format!("failed to emit stage-failed event: {e}"))?;
                set_run_status(conn, &run_entity_id, "failed", now).await?;
                return Err(format!("stage '{}' failed: {e}", stage.name));
            }
        }
    }

    set_run_status(conn, &run_entity_id, "completed", now).await?;
    Ok(PipelineOutcome::Completed)
}

async fn set_run_status(conn: &Connection, run_entity_id: &str, status: &str, now: i64) -> Result<(), String> {
    conn.execute(
        "UPDATE entities SET attrs = json_set(attrs, '$.status', ?1), updated_at = ?2 WHERE id = ?3",
        params![status, now, run_entity_id],
    )
    .await
    .map_err(|e| format!("failed to update pipeline_run status: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use libsql::Builder;
    use std::sync::Mutex as StdMutex;

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
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER, type TEXT, \
                entity_id TEXT, actor TEXT, attrs TEXT, run_id TEXT, tier TEXT, model TEXT, \
                tokens_in INTEGER, tokens_out INTEGER, cost REAL, latency_ms INTEGER, \
                error TEXT, outcome TEXT, eval_score REAL, gated INTEGER DEFAULT 0)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    struct MockRunner {
        outputs: StdMutex<Vec<Result<StageResult, String>>>,
    }

    impl MockRunner {
        fn ok_sequence(texts: &[&str]) -> Self {
            let outputs = texts
                .iter()
                .map(|t| {
                    Ok(StageResult {
                        output: json!({ "text": t }),
                        tokens_in: 10,
                        tokens_out: 20,
                        model: "mock-model".to_string(),
                    })
                })
                .collect();
            Self { outputs: StdMutex::new(outputs) }
        }

        fn failing() -> Self {
            Self { outputs: StdMutex::new(vec![Err("mock failure".to_string())]) }
        }
    }

    #[async_trait]
    impl PipelineStageRunner for MockRunner {
        async fn run_stage(
            &self,
            _stage: &StageSpec,
            _input: &Value,
            _prior: &[Value],
        ) -> Result<StageResult, String> {
            self.outputs.lock().unwrap().remove(0)
        }
    }

    async fn count_events(conn: &Connection, run_entity_id: &str) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM events WHERE entity_id = ?1", params![run_entity_id])
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn unknown_pipeline_fails_loudly() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-unknown.db").await;
        let runner = MockRunner::ok_sequence(&[]);
        let payload = PipelineJobPayload { pipeline: "nonsense".into(), input: json!({}) };
        let result = process_pipeline_job(&conn, "ws1", "run1", payload, &runner, &HeuristicJudge, 1.0, 0).await;
        assert!(result.unwrap_err().contains("unknown pipeline"));
    }

    #[tokio::test]
    async fn run_entity_records_its_own_run_id_for_frontend_history_joins() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-runid.db").await;
        let runner = MockRunner::ok_sequence(&[
            "a long researched summary about the topic",
            "a long drafted post body about the topic",
            "a long verification pass confirming the draft",
        ]);
        let payload = PipelineJobPayload { pipeline: "post-from-topic".into(), input: json!({}) };
        process_pipeline_job(&conn, "ws1", "run_xyz", payload, &runner, &HeuristicJudge, 1.0, 0).await.unwrap();

        let mut rows = conn.query("SELECT attrs FROM entities WHERE type='pipeline_run'", ()).await.unwrap();
        let attrs_str: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let attrs: Value = serde_json::from_str(&attrs_str).unwrap();
        assert_eq!(attrs["run_id"], "run_xyz");
    }

    #[tokio::test]
    async fn full_run_halts_and_drafts_at_the_gated_publish_stage() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-gated.db").await;
        // research, draft, verify all produce healthy (long) text so the eval gate passes.
        let runner = MockRunner::ok_sequence(&[
            "a long researched summary about the topic",
            "a long drafted post body about the topic",
            "a long verification pass confirming the draft",
        ]);
        let payload = PipelineJobPayload { pipeline: "post-from-topic".into(), input: json!({"topic": "rust"}) };
        let outcome = process_pipeline_job(&conn, "ws1", "run1", payload, &runner, &HeuristicJudge, 1.0, 0).await.unwrap();
        assert_eq!(outcome, PipelineOutcome::AwaitingApproval { stage: "publish".to_string() });

        let mut rows = conn.query("SELECT attrs FROM entities WHERE type='pipeline_run'", ()).await.unwrap();
        let attrs_str: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let attrs: Value = serde_json::from_str(&attrs_str).unwrap();
        assert_eq!(attrs["status"], "awaiting_approval");

        let mut rows = conn.query("SELECT id FROM entities WHERE type='pipeline_run'", ()).await.unwrap();
        let run_entity_id: String = rows.next().await.unwrap().unwrap().get(0).unwrap();

        // 3 completed stages + 1 gated event = 4.
        assert_eq!(count_events(&conn, &run_entity_id).await, 4);

        let mut approvals =
            conn.query("SELECT COUNT(*) FROM entities WHERE type='pending_approval'", ()).await.unwrap();
        let n: i64 = approvals.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn eval_gated_verify_stage_halts_before_publish_runs() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-evalgate.db").await;
        // verify's output is empty text -> eval score 0.0, below threshold.
        let runner =
            MockRunner::ok_sequence(&["a long researched summary", "a long drafted post body", ""]);
        let payload = PipelineJobPayload { pipeline: "post-from-topic".into(), input: json!({}) };
        let outcome = process_pipeline_job(&conn, "ws1", "run1", payload, &runner, &HeuristicJudge, 1.0, 0).await.unwrap();
        assert_eq!(outcome, PipelineOutcome::Gated { stage: "verify".to_string(), rationale: "output is empty".to_string() });

        // publish must never have been reached: only 3 events (research, draft, verify-gated).
        let mut rows = conn.query("SELECT id FROM entities WHERE type='pipeline_run'", ()).await.unwrap();
        let run_entity_id: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count_events(&conn, &run_entity_id).await, 3);

        // No pending_approval draft should exist - the run halted before the gated stage.
        let mut approvals =
            conn.query("SELECT COUNT(*) FROM entities WHERE type='pending_approval'", ()).await.unwrap();
        let n: i64 = approvals.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(n, 0);
    }

    struct StubLowJudge;
    #[async_trait]
    impl Judge for StubLowJudge {
        async fn score(&self, _content: &str) -> Result<(f64, String), String> {
            Ok((0.1, "reads like a placeholder, not a real draft".to_string()))
        }
    }

    #[tokio::test]
    async fn real_judge_gate_carries_its_rationale_through_the_outcome() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-judgegate.db").await;
        // verify's text is non-empty (passes the heuristic) but StubLowJudge scores it low.
        let runner = MockRunner::ok_sequence(&[
            "a long researched summary",
            "a long drafted post body",
            "a long verification pass confirming the draft",
        ]);
        let payload = PipelineJobPayload { pipeline: "post-from-topic".into(), input: json!({}) };
        let outcome =
            process_pipeline_job(&conn, "ws1", "run1", payload, &runner, &StubLowJudge, 1.0, 0)
                .await
                .unwrap();
        assert_eq!(
            outcome,
            PipelineOutcome::Gated {
                stage: "verify".to_string(),
                rationale: "reads like a placeholder, not a real draft".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn stage_runner_error_fails_the_whole_job() {
        let conn = fresh_conn("/tmp/lifeos-pipelines-test-fail.db").await;
        let runner = MockRunner::failing();
        let payload = PipelineJobPayload { pipeline: "post-from-topic".into(), input: json!({}) };
        let result = process_pipeline_job(&conn, "ws1", "run1", payload, &runner, &HeuristicJudge, 1.0, 0).await;
        assert!(result.unwrap_err().contains("mock failure"));

        let mut rows = conn.query("SELECT attrs FROM entities WHERE type='pipeline_run'", ()).await.unwrap();
        let attrs_str: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let attrs: Value = serde_json::from_str(&attrs_str).unwrap();
        assert_eq!(attrs["status"], "failed");
    }
}
