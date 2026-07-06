//! Per-stage agent execution (issue #92). Same DI-trait shape as
//! `lifeos-ingest/src/vision.rs::Captioner`: a `NoopStageRunner` fails
//! loudly (a pipeline stage is not optional - unlike OCR in the ingest
//! crate, there is no safe "degrade to empty" for an agent stage) and a
//! `HaikuStageRunner` runs the stage through the crate's shared
//! `anthropic::AnthropicClient` (audit finding 30).

use crate::anthropic::AnthropicClient;
use crate::StageSpec;
use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct StageResult {
    pub output: Value,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub model: String,
}

/// Runs one DAG stage and returns its output. `input` is the pipeline run's
/// original input; `prior` is every earlier stage's output, in order.
#[async_trait]
pub trait PipelineStageRunner: Send + Sync {
    async fn run_stage(&self, stage: &StageSpec, input: &Value, prior: &[Value]) -> Result<StageResult, String>;
}

/// Used when `ANTHROPIC_API_KEY` is unset. Fails loudly: a pipeline job
/// that can't actually run its stages must fail, not silently "complete"
/// with fabricated output.
pub struct NoopStageRunner;

#[async_trait]
impl PipelineStageRunner for NoopStageRunner {
    async fn run_stage(&self, _stage: &StageSpec, _input: &Value, _prior: &[Value]) -> Result<StageResult, String> {
        Err("no pipeline stage runner configured (ANTHROPIC_API_KEY unset)".to_string())
    }
}

/// Real stage execution via the Anthropic Messages API (Haiku), through the
/// shared `AnthropicClient`.
pub struct HaikuStageRunner {
    pub api_key: String,
}

/// Builds a stage's prompt. `pub` so `lifeos-drain`'s agent-CLI stage runner
/// reuses this single copy instead of duplicating it verbatim (audit
/// finding 31).
pub fn build_prompt(stage: &StageSpec, input: &Value, prior: &[Value]) -> String {
    let mut prompt = format!("You are the '{}' stage of an agent pipeline.\n", stage.agent);
    if let Some(skill) = stage.skill {
        prompt.push_str(&format!("Apply the '{skill}' skill.\n"));
    }
    if let Some(tool) = stage.tool {
        prompt.push_str(&format!("(Reference tool for this stage: {tool}; not actually invoked by this runner.)\n"));
    }
    prompt.push_str(&format!("Run input: {input}\n"));
    if !prior.is_empty() {
        prompt.push_str(&format!("Prior stage outputs: {}\n", Value::Array(prior.to_vec())));
    }
    prompt.push_str("Respond with the stage's output as plain text.");
    prompt
}

#[async_trait]
impl PipelineStageRunner for HaikuStageRunner {
    async fn run_stage(&self, stage: &StageSpec, input: &Value, prior: &[Value]) -> Result<StageResult, String> {
        let prompt = build_prompt(stage, input, prior);
        let client = AnthropicClient::new(self.api_key.clone());
        let completion = client.complete(None, &prompt, 1024).await?;

        let text = completion.text.trim().to_string();
        if text.is_empty() {
            return Err("anthropic response had no stage output text".to_string());
        }

        Ok(StageResult {
            output: json!({ "text": text }),
            tokens_in: completion.tokens_in,
            tokens_out: completion.tokens_out,
            model: client.model().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_stage_runner_fails_loudly() {
        let stage = StageSpec { name: "x", agent: "x", tool: None, skill: None, gate: None, gated: false };
        let result = NoopStageRunner.run_stage(&stage, &json!({}), &[]).await;
        assert!(result.unwrap_err().contains("ANTHROPIC_API_KEY"));
    }
}
