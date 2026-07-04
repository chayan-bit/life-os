// Replay inspector for an `agent.turn` event (issue #125,
// docs/AGENT-CORE.md §7). Read-only: reconstructs what a turn did from its
// single flight-recorder row - never re-executes any tool call. The
// formatting logic (`formatTurnReplay`) is exported separately from the CLI
// wrapper so it is unit-testable on a fixture row without a live server.
//
// Usage: node server/scripts/replayTurn.js <run_id>
const DEFAULT_API_BASE = process.env.LIFEOS_API_URL || "http://127.0.0.1:8080";
const DEFAULT_WORKSPACE_ID = process.env.LIFEOS_WORKSPACE_ID || "default-personal-workspace";

// Fetches the one `agent.turn` event for `runId`, or null if none exists.
// `fetchFn`/`apiBase`/`workspaceId` are injectable for tests.
export async function fetchTurnEvent(runId, { fetchFn = fetch, apiBase = DEFAULT_API_BASE, workspaceId = DEFAULT_WORKSPACE_ID } = {}) {
  const url = `${apiBase}/api/event?run_id=${encodeURIComponent(runId)}&type=agent.turn`;
  const res = await fetchFn(url, { headers: { "X-Workspace-Id": workspaceId } });
  if (!res.ok) return null;
  const rows = await res.json();
  return Array.isArray(rows) && rows.length > 0 ? rows[0] : null;
}

function formatToolCall(call, index) {
  const status = call.ok ? "ok" : "FAILED";
  return `  ${index + 1}. ${call.tool}  [${call.decision}]  ${call.ms}ms  ${status}`;
}

function formatPlanStage(stage, index) {
  return `  ${index + 1}. ${stage.name}${stage.tool ? ` (${stage.tool})` : ""} - ${stage.description}`;
}

// Pure formatter: turns one `agent.turn` event row into a human-readable,
// step-by-step reconstruction. No I/O, no tool execution - a straight
// projection of the event's own fields/attrs (docs/AGENT-CORE.md §7).
export function formatTurnReplay(event) {
  const attrs = event.attrs && typeof event.attrs === "object" ? event.attrs : {};
  const plan = attrs.plan;
  const toolCalls = attrs.tool_calls ?? [];
  const lines = [
    `run_id       : ${event.run_id ?? "(none)"}`,
    `goal         : ${attrs.goal ?? "(none)"}`,
    `outcome      : ${event.outcome ?? "(unknown)"}`,
    `model        : ${event.model ?? "(none)"}`,
    `tokens       : in ${event.tokens_in ?? 0}  out ${event.tokens_out ?? 0}`,
    `latency      : ${event.latency_ms ?? 0}ms`,
    `refined      : ${Boolean(attrs.refined)}`,
    `gated        : ${Boolean(event.gated)}`,
    `error        : ${event.error ?? "(none)"}`,
    "",
    plan && plan.length > 0 ? "plan:" : "plan: (no plan - direct execution)",
    ...(plan ?? []).map(formatPlanStage),
    "",
    toolCalls.length > 0 ? "tool calls:" : "tool calls: (none)",
    ...toolCalls.map(formatToolCall),
  ];
  return lines.join("\n");
}

async function main() {
  const runId = process.argv[2];
  if (!runId) {
    console.error("Usage: node server/scripts/replayTurn.js <run_id>");
    process.exit(1);
  }
  const event = await fetchTurnEvent(runId);
  if (!event) {
    console.error(`replayTurn: no agent.turn event found for run_id '${runId}'`);
    process.exit(1);
  }
  console.log(formatTurnReplay(event));
}

// Only run the CLI when invoked directly (not when imported by tests).
if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
