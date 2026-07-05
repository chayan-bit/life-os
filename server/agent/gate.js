// Fail-closed pre-flight gate (docs/AGENT-CORE.md §3 step 1, §11). Runs BEFORE
// any model call: honors a per-workspace kill switch and a daily token budget,
// and fails closed (refuses) on ANY error evaluating the gate - the same
// discipline as broker-guard. No model tokens are ever spent past a refusal.
// Both refusal paths (kill switch, budget) escalate an append-only event
// best-effort, so a paused/exhausted agent is visible in Observe, not silent.

// Named constants - no magic numbers.
export const DEFAULT_DAILY_TOKEN_BUDGET = 2_000_000;
const SECONDS_PER_DAY = 86_400;
const EVENT_FETCH_LIMIT = 2000;

const agentConfigId = (workspaceId) => `agent_config_${workspaceId}`;

const startOfTodayUtc = (nowSecs) => Math.floor(nowSecs / SECONDS_PER_DAY) * SECONDS_PER_DAY;

// Events store attrs as an object or a JSON string depending on the read path;
// parse defensively so a malformed row never crashes the gate.
function readAttrs(row) {
  const raw = row?.attrs;
  if (raw && typeof raw === "object") return raw;
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw);
    } catch {
      return {};
    }
  }
  return {};
}

// Sums attrs.tokens over today's 'agent.turn' events for the workspace.
function sumTodaysTokens(events, nowSecs) {
  const since = startOfTodayUtc(nowSecs);
  let total = 0;
  for (const ev of events || []) {
    if (typeof ev?.ts === "number" && ev.ts < since) continue;
    const tokens = Number(readAttrs(ev).tokens);
    if (Number.isFinite(tokens)) total += tokens;
  }
  return total;
}

// Reads { killSwitch, dailyTokenBudget } from the agent config entity, applying
// defaults when the row is absent (a 404 is "no config yet", not an error).
async function readGateConfig(httpFn, workspaceId) {
  const res = await httpFn("GET", `/api/entity/${agentConfigId(workspaceId)}`);
  if (!res || res.status === 404 || !res.ok || !res.data) {
    return { killSwitch: false, dailyTokenBudget: DEFAULT_DAILY_TOKEN_BUDGET };
  }
  const attrs = readAttrs(res.data);
  return {
    killSwitch: attrs.killSwitch === true,
    dailyTokenBudget: Number.isFinite(Number(attrs.dailyTokenBudget))
      ? Number(attrs.dailyTokenBudget)
      : DEFAULT_DAILY_TOKEN_BUDGET,
  };
}

// Returns { ok: true } to proceed, or { ok: false, reason } to refuse.
// Any thrown fetch error -> fail closed with reason 'gate_unavailable'.
export async function checkGate(ctx) {
  const { httpFn, workspaceId, nowSecs = Math.floor(Date.now() / 1000) } = ctx;
  let config;
  try {
    config = await readGateConfig(httpFn, workspaceId);
  } catch {
    return { ok: false, reason: "gate_unavailable" };
  }

  if (config.killSwitch) {
    // Escalate: same append-only visibility as agent.budget_exhausted below,
    // so a paused agent shows up in Observe rather than silently going quiet.
    try {
      await httpFn("POST", "/api/event", {
        type: "agent.paused",
        actor: "agent",
        attrs: { reason: "kill_switch" },
        workspace_id: workspaceId,
      });
    } catch {
      // Escalation is best-effort; the refusal below still stands.
    }
    return { ok: false, reason: "kill_switch" };
  }

  let spent;
  try {
    const res = await httpFn("GET", `/api/event?type=agent.turn&limit=${EVENT_FETCH_LIMIT}`);
    const events = res?.ok ? res.data : [];
    spent = sumTodaysTokens(events, nowSecs);
  } catch {
    return { ok: false, reason: "gate_unavailable" };
  }

  if (spent >= config.dailyTokenBudget) {
    // Escalate: an append-only marker the harness Observe lens can surface.
    try {
      await httpFn("POST", "/api/event", {
        type: "agent.budget_exhausted",
        actor: "agent",
        attrs: { spent, budget: config.dailyTokenBudget },
        workspace_id: workspaceId,
      });
    } catch {
      // Escalation is best-effort; the refusal below still stands.
    }
    return { ok: false, reason: "budget_exhausted" };
  }

  return { ok: true };
}
