//! Keyless AI lanes for the drain - every trait the drain's jobs need
//! (`PipelineStageRunner`, `Judge`, `Captioner`) implemented over the shared
//! `lifeos-agents` local agent-CLI router instead of a cloud API key. The
//! drain runs on the trusted Mac where those CLIs (Claude Code, Gemini CLI,
//! OpenCode, ...) already exist and are already authenticated, so no
//! `ANTHROPIC_API_KEY` is needed; the Haiku implementations remain as the
//! fallback when no CLI is on PATH.
//!
//! Agent/model defaults follow `LIFEOS_AGENT` / `LIFEOS_AGENT_MODEL`,
//! resolved per call inside `lifeos_agents::run` - changing the env and
//! restarting the drain is the whole "switch models" story here.

use async_trait::async_trait;
use lifeos_agents::{DetectedAgent, RunOptions};
use lifeos_ingest::Captioner;
use lifeos_memory::{HeuristicModel, MemoryError, MemoryModel};
use lifeos_pipelines::{Judge, PipelineStageRunner, StageResult, StageSpec};
use serde_json::{json, Value};
use std::sync::Arc;

const STAGE_TIMEOUT_SECS: u64 = 300;
const MEMORY_MODEL_TIMEOUT_SECS: u64 = 60;
const JUDGE_RUBRIC: &str = "You are an output quality judge. \
Score the following stage output 1-5 on whether it is complete, non-empty, and usable as-is \
(5 = ship it, 1 = empty or unusable). \
Respond with ONLY strict JSON: {\"score\": <1-5>, \"rationale\": \"<one sentence>\"}.";
const CAPTION_PROMPT: &str = "Read the image file at this path and describe it in one or two \
concise sentences, focused on what it shows so it can be found later by a text search. \
Respond with ONLY the description text.";

fn run_options() -> RunOptions {
    // agent_id/model None => LIFEOS_AGENT / LIFEOS_AGENT_MODEL, then the
    // registry preference order - the same resolution every lane uses.
    RunOptions { timeout_secs: STAGE_TIMEOUT_SECS, ..Default::default() }
}

/// Pipeline stage execution through a local agent CLI.
pub struct AgentCliStageRunner {
    pub agents: Arc<Vec<DetectedAgent>>,
}

fn build_stage_prompt(stage: &StageSpec, input: &Value, prior: &[Value]) -> String {
    let mut prompt = format!("You are the '{}' stage of an agent pipeline.\n", stage.agent);
    if let Some(skill) = stage.skill {
        prompt.push_str(&format!("Apply the '{skill}' skill.\n"));
    }
    if let Some(tool) = stage.tool {
        prompt.push_str(&format!(
            "(Reference tool for this stage: {tool}; not actually invoked by this runner.)\n"
        ));
    }
    prompt.push_str(&format!("Run input: {input}\n"));
    if !prior.is_empty() {
        prompt.push_str(&format!("Prior stage outputs: {}\n", Value::Array(prior.to_vec())));
    }
    prompt.push_str("Respond with the stage's output as plain text.");
    prompt
}

#[async_trait]
impl PipelineStageRunner for AgentCliStageRunner {
    async fn run_stage(
        &self,
        stage: &StageSpec,
        input: &Value,
        prior: &[Value],
    ) -> Result<StageResult, String> {
        let prompt = build_stage_prompt(stage, input, prior);
        let opts = run_options();
        let text = lifeos_agents::run(&self.agents, &opts, &prompt)
            .await
            .map_err(|e| format!("agent CLI stage failed: {e}"))?;
        if text.trim().is_empty() {
            return Err("agent CLI returned an empty stage output".to_string());
        }
        let model = lifeos_agents::default_agent_id(&self.agents)
            .map(|id| format!("agent-cli:{id}"))
            .unwrap_or_else(|| "agent-cli".to_string());
        // CLIs don't report token usage - recorded as 0, honestly unknown.
        Ok(StageResult { output: json!({ "text": text.trim() }), tokens_in: 0, tokens_out: 0, model })
    }
}

/// Eval-gate judging through a local agent CLI. Same rubric/JSON contract
/// as `HaikuJudge`, so gate behavior is identical either way.
pub struct AgentCliJudge {
    pub agents: Arc<Vec<DetectedAgent>>,
}

#[async_trait]
impl Judge for AgentCliJudge {
    async fn score(&self, content: &str) -> Result<(f64, String), String> {
        let prompt = format!("{JUDGE_RUBRIC}\n\nStage output to judge:\n{content}");
        let opts = run_options();
        let text = lifeos_agents::run(&self.agents, &opts, &prompt)
            .await
            .map_err(|e| format!("agent CLI judge failed: {e}"))?;
        parse_judge_json(&text)
    }
}

fn parse_judge_json(text: &str) -> Result<(f64, String), String> {
    // Agents sometimes wrap JSON in a code fence - strip a fence if present.
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

/// Image captioning through a local agent CLI that can read files (Claude
/// Code, Gemini CLI). The image bytes are written to a private temp file
/// and the agent is asked to describe the file at that path.
pub struct AgentCliCaptioner {
    pub agents: Arc<Vec<DetectedAgent>>,
}

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

#[async_trait]
impl Captioner for AgentCliCaptioner {
    async fn caption(&self, image_bytes: &[u8], mime: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join("lifeos-caption");
        std::fs::create_dir_all(&dir).map_err(|e| format!("caption temp dir failed: {e}"))?;
        let name = format!(
            "{}.{}",
            blake3::hash(image_bytes).to_hex(),
            ext_for_mime(mime)
        );
        let path = dir.join(name);
        std::fs::write(&path, image_bytes).map_err(|e| format!("caption temp write failed: {e}"))?;

        let prompt = format!("{CAPTION_PROMPT}\n\nImage file path: {}", path.display());
        let opts = run_options();
        let result = lifeos_agents::run(&self.agents, &opts, &prompt).await;
        let _ = std::fs::remove_file(&path); // best-effort cleanup either way

        let text = result.map_err(|e| format!("agent CLI caption failed: {e}"))?;
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return Err("agent CLI returned an empty caption".to_string());
        }
        Ok(trimmed)
    }
}

/// Task-keyed instruction the model is asked to follow (audit #6: consolidation
/// summaries stayed extractive-only despite comments promising a Haiku-backed
/// drop-in). Kept as a pure function so the wording is directly testable.
fn task_instruction(task: &str) -> &'static str {
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
/// `LIFEOS_MEMORY_MODEL=agent`, see docs/AI-MEMORY.md §5). `lifeos-memory`
/// itself stays free of the `lifeos-agents` dependency (it must remain a pure
/// event-sourcing engine testable without any CLI on PATH), so this impl
/// lives here in the consumer instead. A near-identical, intentionally
/// duplicated impl lives in `lifeos-api/src/routes/memory.rs`'s
/// `sleep_handler` - `lifeos-api` and `lifeos-drain` are separate binaries
/// with no third crate that already depends on both `lifeos-agents` and
/// `lifeos-memory` without adding a new cross-crate edge, so the ~40 lines
/// are kept in sync by hand rather than sharing a home neither crate should
/// depend on. See that file's comment for the twin.
///
/// ANY failure (no CLI detected, timeout, empty output) falls back to
/// `HeuristicModel`'s deterministic output for that call - a consolidation
/// cycle must never fail outright, and the caller's `ReplayCachedModel`
/// wrapper still caches whatever this produces, so replay stays
/// byte-identical either way.
pub struct AgentCliModel {
    pub agents: Arc<Vec<DetectedAgent>>,
}

#[async_trait]
impl MemoryModel for AgentCliModel {
    async fn complete(&self, task: &str, prompt: &str) -> Result<String, MemoryError> {
        let full_prompt = format!("{}\n\n{prompt}", task_instruction(task));
        let opts = RunOptions { timeout_secs: MEMORY_MODEL_TIMEOUT_SECS, ..Default::default() };
        match lifeos_agents::run(&self.agents, &opts, &full_prompt).await {
            Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
            _ => HeuristicModel.complete(task, prompt).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_json_parses_plain_and_fenced() {
        let (score, rationale) =
            parse_judge_json(r#"{"score": 4, "rationale": "solid"}"#).unwrap();
        assert!((score - 0.8).abs() < 1e-9);
        assert_eq!(rationale, "solid");

        let fenced = "```json\n{\"score\": 5, \"rationale\": \"ship it\"}\n```";
        let (score, _) = parse_judge_json(fenced).unwrap();
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn judge_score_is_clamped() {
        let (score, _) = parse_judge_json(r#"{"score": 9, "rationale": "x"}"#).unwrap();
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn stage_prompt_includes_agent_input_and_prior() {
        let stage = StageSpec {
            name: "draft",
            agent: "writer",
            tool: None,
            skill: Some("copywriting"),
            gate: None,
            gated: false,
        };
        let prompt = build_stage_prompt(&stage, &json!({"topic": "x"}), &[json!({"text": "prev"})]);
        assert!(prompt.contains("'writer' stage"));
        assert!(prompt.contains("copywriting"));
        assert!(prompt.contains("\"topic\""));
        assert!(prompt.contains("Prior stage outputs"));
    }

    #[tokio::test]
    async fn cli_lanes_fail_loudly_with_no_agents() {
        let agents = Arc::new(Vec::new());
        let stage = StageSpec { name: "x", agent: "x", tool: None, skill: None, gate: None, gated: false };
        let runner = AgentCliStageRunner { agents: agents.clone() };
        assert!(runner.run_stage(&stage, &json!({}), &[]).await.is_err());
        let judge = AgentCliJudge { agents: agents.clone() };
        assert!(judge.score("content").await.is_err());
        let captioner = AgentCliCaptioner { agents };
        assert!(captioner.caption(b"png", "image/png").await.is_err());
    }

    /// Same "no CLI on PATH" seam as `cli_lanes_fail_loudly_with_no_agents`
    /// (an empty `agents` vec makes `lifeos_agents::run` fail without
    /// spawning a real subprocess) - but `MemoryModel::complete` must never
    /// propagate that error: it should fall back to `HeuristicModel`'s
    /// deterministic output instead (audit #6, "never fail the sleep cycle").
    #[tokio::test]
    async fn agent_cli_model_falls_back_to_heuristic_when_no_agent_is_detected() {
        let model = AgentCliModel { agents: Arc::new(Vec::new()) };
        let prompt = "first fact\nsecond fact";

        let got = model.complete("summarize", prompt).await.unwrap();
        let expected = HeuristicModel.complete("summarize", prompt).await.unwrap();
        assert_eq!(got, expected, "falls back to the heuristic model's exact output");
    }

    #[test]
    fn task_instruction_is_task_specific_and_forbids_speculation() {
        assert!(task_instruction("summarize").contains("No speculation"));
        assert!(task_instruction("reflect").contains("reflection"));
        assert!(task_instruction("unknown-task").contains("No speculation"));
    }
}
