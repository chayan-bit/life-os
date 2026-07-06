//! One shared Anthropic Messages API client for the whole crate (audit
//! finding 30 + 51). Before this, `eval_gate::HaikuJudge`,
//! `runner::HaikuStageRunner`, and `lifeos-ingest`'s captioner each
//! hand-rolled the same request/headers/error/parse against
//! `https://api.anthropic.com/v1/messages` with a separately-pinned model id.
//! This collapses the pipelines callers onto one `complete()` and one
//! env-overridable model constant so the request contract lives in a single
//! place (the captioner lives in another crate and is left to its owner).

use serde_json::{json, Value};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The single Haiku model id for the whole workspace (audit finding 51).
/// Previously pinned as four separate constants with two different values;
/// now one default with a `LIFEOS_HAIKU_MODEL` env override so a model bump
/// is one env var, not a code sweep.
pub const DEFAULT_HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";

/// Pure resolver so the env-override precedence is directly unit-testable
/// without mutating process env in a parallel test run.
fn resolve_model(env_value: Option<String>) -> String {
    env_value
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HAIKU_MODEL.to_string())
}

/// The Haiku model id, `LIFEOS_HAIKU_MODEL` if set and non-empty, else the
/// current default (`claude-haiku-4-5-20251001`).
pub fn haiku_model() -> String {
    resolve_model(std::env::var("LIFEOS_HAIKU_MODEL").ok())
}

/// One completion from the Messages API: the first text block plus reported
/// token usage (0/0 when the response omits `usage`).
#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

/// A thin owner of `{model, api_key}` that speaks the Anthropic Messages API.
/// `complete` is the one place the request body, headers, error path, and
/// response parse live for the crate's Haiku callers.
pub struct AnthropicClient {
    api_key: String,
    model: String,
}

impl AnthropicClient {
    /// Uses the env-resolved Haiku model (`haiku_model`).
    pub fn new(api_key: String) -> Self {
        Self { api_key, model: haiku_model() }
    }

    /// The resolved model id this client sends, for honest `events.model`
    /// stamping by callers.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// One Messages API call. `system` is optional (the stage runner sends
    /// none, the judge sends its rubric); the caller decides how to trim /
    /// validate / parse the returned text.
    pub async fn complete(
        &self,
        system: Option<&str>,
        user: &str,
        max_tokens: u32,
    ) -> Result<Completion, String> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": [{ "role": "user", "content": user }],
        });
        if let Some(sys) = system {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("system".to_string(), Value::String(sys.to_string()));
            }
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("anthropic request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("anthropic api error {status}: {text}"));
        }

        let parsed: Value = resp
            .json()
            .await
            .map_err(|e| format!("anthropic response parse failed: {e}"))?;
        let text = parsed
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|block| block.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "anthropic response had no text block".to_string())?;

        let usage = parsed.get("usage");
        let tokens_in = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);
        let tokens_out = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);

        Ok(Completion { text, tokens_in, tokens_out })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_defaults_when_unset_or_empty() {
        assert_eq!(resolve_model(None), DEFAULT_HAIKU_MODEL);
        assert_eq!(resolve_model(Some(String::new())), DEFAULT_HAIKU_MODEL);
    }

    #[test]
    fn resolve_model_honors_a_non_empty_override() {
        assert_eq!(resolve_model(Some("claude-haiku-4-5".to_string())), "claude-haiku-4-5");
    }

    #[test]
    fn default_haiku_model_is_the_current_pinned_id() {
        assert_eq!(DEFAULT_HAIKU_MODEL, "claude-haiku-4-5-20251001");
    }
}
