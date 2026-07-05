// The general plan -> execute -> verify agent turn (docs/AGENT-CORE.md §3, §7).
// Orchestrates: gate -> world snapshot -> plan? -> execute -> verify/refine ->
// persist. Invents no new authority - it sequences already-gated actions and
// records one append-only `events('agent.turn')` row per turn.
import { query as defaultQuery } from "@anthropic-ai/claude-agent-sdk";
import { checkGate } from "./gate.js";
import { buildWorldSnapshot } from "./worldSnapshot.js";
import { needsPlanning, generatePlan, persistPlan, updatePlanStatus } from "./planner.js";
import { runExecute } from "./executor.js";
import { critique } from "./critic.js";
import { createHttpFn } from "./http.js";
import { REGISTRY } from "./actionRegistry.js";
import { indexTools, retrieveTools } from "./toolRag.js";
import { fetchMemoryContext, ingestTurnOutcome } from "./memoryContext.js";
import { emptyUsage } from "./usage.js";

const newRunId = () => `run_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

// The harness run-log's tier stamp (docs/HARNESS-LOOP.md §1, §3): the agent
// loop only ever runs on the trusted Mac harness process today, matching
// `lifeos-pipelines::emit_run_event`'s own hardcoded 'mac' tier.
const TIER = "mac";

// A "deliberate" turn (had a plan, actually finished) is the natural
// `gate:"eval"` boundary the sampled Haiku judge can score (issue #125,
// docs/AGENT-CORE.md §7). Stamped as `attrs.stage` - the exact field
// `GET /api/metrics`'s `events_by_phase` lens already reads
// (`json_extract(attrs, '$.stage')`) - so agent turns slot into the same
// phase breakdown pipeline stages do, with no Rust change required.
// Scoring itself stays out of scope here (wiring only, see docs note):
// today only `lifeos-pipelines::process_pipeline_job`'s inline stage runner
// calls the judge; a future consumer can select on
// `type='agent.turn' AND json_extract(attrs,'$.stage')='eval'`.
function evalStage(plan, outcome) {
  return plan && outcome === "completed" ? "eval" : null;
}

// Folds a stage result's { tokensIn, tokensOut } into an accumulator without
// mutating either argument.
function addUsage(acc, stageResult) {
  return {
    tokensIn: acc.tokensIn + (stageResult.tokensIn ?? 0),
    tokensOut: acc.tokensOut + (stageResult.tokensOut ?? 0),
  };
}

// Appends the single agent.turn flight-recorder row (docs/AGENT-CORE.md §7).
// Stamps the same run-log lens fields `lifeos-pipelines::emit_run_event`
// uses (tier/tokens_in/tokens_out/gated/error) so `harness observe` /
// `GET /api/metrics` count agent turns without any Rust change - the
// metrics SQL already aggregates generically over `events`, not by type.
async function persistTurn(ctx, record) {
  try {
    await ctx.httpFn("POST", "/api/event", {
      type: "agent.turn",
      actor: "agent",
      run_id: ctx.runId,
      model: record.model ?? null,
      tier: TIER,
      tokens_in: record.tokensIn ?? null,
      tokens_out: record.tokensOut ?? null,
      latency_ms: record.latency_ms,
      outcome: record.outcome,
      error: record.error ?? null,
      gated: record.outcome === "awaiting_approval" ? 1 : 0,
      attrs: {
        ...record,
        stage: evalStage(record.plan, record.outcome),
        tools_offered: ctx.toolsOffered ?? null,
        toolrag_fallback: ctx.toolragFallback ?? null,
        memory_injected: ctx.memoryInjected ?? false,
      },
      workspace_id: ctx.workspaceId,
    });
  } catch {
    // Best-effort trace; a persistence failure must not mask the turn result.
  }
}

async function escalate(ctx, type, attrs) {
  try {
    await ctx.httpFn("POST", "/api/event", { type, actor: "agent", attrs, workspace_id: ctx.workspaceId });
  } catch {
    // Escalation is best-effort.
  }
}

// runAgentTurn(prompt, workspaceId, opts) - the loop entry point.
export async function runAgentTurn(prompt, workspaceId, opts = {}) {
  const started = Date.now();
  const runId = opts.runId ?? newRunId();
  const ctx = {
    runId,
    workspaceId,
    model: opts.model ?? null,
    queryFn: opts.queryFn ?? defaultQuery,
    httpFn: opts.httpFn ?? createHttpFn(workspaceId, opts.apiBase),
    ledger: [],
    pendingApprovals: [],
    stepCount: 0,
    stepBudgetExhausted: false,
    nowSecs: opts.nowSecs,
  };

  // 1. Gate - fail closed before any model call.
  const gate = await checkGate(ctx);
  if (!gate.ok) {
    const text = gate.reason === "kill_switch" ? "Agent paused: the kill switch is on for this workspace." : null;
    return { success: false, runId, outcome: gate.reason, error: gate.reason, ...(text ? { text } : {}) };
  }

  try {
    // 2. Context assembly - world snapshot + lifeos-memory activation recall
    // (docs/AGENT-CORE.md §5, #124). The memory block is the compiler's own
    // token-budgeted output, appended verbatim - never a re-query, never a
    // raw dump.
    const worldSnapshot = await buildWorldSnapshot(ctx);
    const memory = await fetchMemoryContext(ctx.httpFn, ctx.workspaceId, prompt);
    ctx.memoryInjected = Boolean(memory.block);
    const context = [worldSnapshot, memory.block].filter(Boolean).join("\n\n");

    // 3. Plan (conditional). Tracks tokensIn/tokensOut separately (not just
    // the combined `tokens`) for the run-log lens's tokens_in/tokens_out
    // columns (issue #125).
    let plan = null;
    let planEntityId = null;
    let tokens = 0;
    let usage = emptyUsage();
    if (needsPlanning(prompt)) {
      const planned = await generatePlan(prompt, context, ctx);
      plan = planned.plan;
      tokens += planned.tokens;
      usage = addUsage(usage, planned);
      planEntityId = await persistPlan(prompt, plan, ctx);
    }

    // 3b. Tool-RAG: index the registry lazily (fire-and-forget-ish - never
    // fails the turn, indexTools already catches internally) then retrieve
    // the top-K relevant tools + core set for this turn's execute stage.
    try {
      await indexTools(REGISTRY, { httpFn: ctx.httpFn, workspaceId: ctx.workspaceId, ...opts.toolRag });
    } catch {
      // Defense-in-depth only; indexTools does not throw.
    }
    const retrieval = await retrieveTools(prompt, REGISTRY, opts.toolRag);
    ctx.toolNames = retrieval.tools;
    ctx.toolsOffered = retrieval.tools.length;
    ctx.toolragFallback = retrieval.fallback;

    // 4. Execute (bounded).
    const exec = await runExecute(prompt, context, plan, ctx);
    tokens += exec.tokens;
    usage = addUsage(usage, exec);
    let text = exec.text;
    let refined = false;

    // Step-budget runaway: stop and escalate rather than loop or verify.
    if (ctx.stepBudgetExhausted) {
      await escalate(ctx, "agent.step_budget_exhausted", { run_id: runId, ledger: ctx.ledger });
      const outcome = "step_budget_exhausted";
      await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started });
      return { success: false, runId, outcome, text, error: outcome };
    }

    // 5. Verify + one bounded refine round.
    if (ctx.pendingApprovals.length === 0) {
      const verdict = await critique(prompt, text, ctx);
      tokens += verdict.tokens;
      usage = addUsage(usage, verdict);
      if (!verdict.critique.ok && verdict.critique.fixable) {
        const redo = await runExecute(prompt, context, plan, ctx, verdict.critique.issue);
        tokens += redo.tokens;
        usage = addUsage(usage, redo);
        if (redo.text) text = redo.text;
        refined = true;
      }
    }

    const outcome = ctx.pendingApprovals.length > 0 ? "awaiting_approval" : "completed";
    await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started });

    return {
      success: true,
      runId,
      outcome,
      text,
      ...(ctx.pendingApprovals.length > 0 ? { pendingApprovals: ctx.pendingApprovals } : {}),
    };
  } catch (error) {
    await persistTurn(ctx, {
      run_id: runId,
      goal: prompt,
      plan: null,
      tool_calls: ctx.ledger,
      model: ctx.model,
      tokens: 0,
      tokensIn: 0,
      tokensOut: 0,
      latency_ms: Date.now() - started,
      refined: false,
      outcome: "failed",
      error: error.message,
    });
    // Write-back (issue #124): even a crashed turn produced ledger work worth
    // folding into the next sleep cycle. Best-effort - never rethrows.
    await ingestTurnOutcome(ctx.httpFn, ctx.workspaceId, prompt, "failed", "");
    return { success: false, runId, outcome: "failed", error: error.message };
  }
}

// Persists the plan status + the agent.turn row, then writes the turn's
// outcome back to memory (issue #124) best-effort so the next sleep cycle
// (consolidate.rs) can fold it - no new subsystem, `events` stays the path.
async function finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started }) {
  const planStatus = outcome === "completed" ? "completed" : outcome === "awaiting_approval" ? "awaiting_approval" : "failed";
  if (plan) await updatePlanStatus(planEntityId, plan, prompt, planStatus, ctx);
  await persistTurn(ctx, {
    run_id: ctx.runId,
    goal: prompt,
    plan: plan ? plan.stages : null,
    tool_calls: ctx.ledger,
    model: ctx.model,
    tokens,
    tokensIn: usage?.tokensIn ?? 0,
    tokensOut: usage?.tokensOut ?? 0,
    latency_ms: Date.now() - started,
    refined,
    outcome,
    error: outcome === "step_budget_exhausted" ? outcome : null,
    result_preview: (text || "").slice(0, 500),
  });
  await ingestTurnOutcome(ctx.httpFn, ctx.workspaceId, prompt, outcome, text);
}
