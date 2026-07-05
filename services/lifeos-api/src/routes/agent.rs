//! `POST /api/agent` - the general plan -> execute -> verify loop
//! (issue #122, docs/AGENT-CORE.md §14). Shells to the JS agent runtime
//! (`server/agent/run.js`) the same way `lifeos-drain` shells `node
//! scaffold.js`, then parses the last stdout line as the JSON `runAgentTurn`
//! returns. Unlike the drain, this path adds a hard wall-clock timeout so a
//! runaway subprocess can never pin the API.
//!
//! `/api/llm` (the cheap single-shot completion path) is untouched; this is the
//! loop-mode superset. The Rust side stays the trust anchor: it only builds the
//! args and parses one line - every capability gate lives in the JS runtime and
//! the existing `entity/edge/event/draft/pipeline` routes it calls back into.

use crate::auth::resolve_workspace;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// Hard ceiling on a single agent turn's subprocess (the JS loop has its own
/// step + spend budgets; this is the outermost wall-clock backstop).
const AGENT_TURN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
pub struct AgentRequest {
    prompt: String,
    workspace_id: Option<String>,
}

pub async fn agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AgentRequest>,
) -> ApiResult<Json<Value>> {
    if req.prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt is required".into()));
    }
    let workspace_id =
        resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let run = tokio::process::Command::new("node")
        .arg("agent/run.js")
        .arg(&req.prompt)
        .arg(&workspace_id)
        .current_dir(&state.config.server_dir)
        .output();

    let output = match tokio::time::timeout(AGENT_TURN_TIMEOUT, run).await {
        Err(_) => return Err(ApiError::Upstream("agent turn timed out".into())),
        Ok(Err(e)) => return Err(ApiError::Upstream(format!("failed to spawn agent runtime: {e}"))),
        Ok(Ok(output)) => output,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = parse_agent_output(&stdout, &stderr).map_err(ApiError::Upstream)?;
    Ok(Json(parsed))
}

/// Parses the LAST non-empty stdout line as the JSON `runAgentTurn` returns.
/// Pure + `#[cfg(test)]`-covered so the line contract is verified without
/// spawning a real Node process (same DI discipline as `ScaffoldJsBuilder`).
fn parse_agent_output(stdout: &str, stderr: &str) -> Result<Value, String> {
    let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty());
    let Some(last_line) = last_line else {
        return Err(format!("agent runtime produced no output (stderr: {stderr})"));
    };
    serde_json::from_str(last_line)
        .map_err(|e| format!("agent runtime output was not valid JSON: {e} (line: {last_line})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_last_nonempty_stdout_line() {
        let stdout = "some log noise\n\n{\"success\":true,\"runId\":\"run_1\",\"outcome\":\"completed\"}\n";
        let v = parse_agent_output(stdout, "").unwrap();
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["outcome"], serde_json::json!("completed"));
    }

    #[test]
    fn errors_when_stdout_is_empty() {
        let err = parse_agent_output("   \n\n", "boom on stderr").unwrap_err();
        assert!(err.contains("no output"));
        assert!(err.contains("boom on stderr"));
    }

    #[test]
    fn errors_when_last_line_is_not_json() {
        let err = parse_agent_output("not json at all\n", "").unwrap_err();
        assert!(err.contains("not valid JSON"));
    }
}
