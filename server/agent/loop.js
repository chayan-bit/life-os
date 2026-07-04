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

const newRunId = () => `run_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

// Appends the single agent.turn flight-recorder row (docs/AGENT-CORE.md §7).
async function persistTurn(ctx, record) {
  try {
    await ctx.httpFn("POST", "/api/event", {
      type: "agent.turn",
      actor: "agent",
      run_id: ctx.runId,
      model: record.model ?? null,
      tokens_in: null,
      tokens_out: null,
      latency_ms: record.latency_ms,
      outcome: record.outcome,
      attrs: record,
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
    return { success: false, runId, outcome: gate.reason, error: gate.reason };
  }

  try {
    // 2. Context assembly.
    const worldSnapshot = await buildWorldSnapshot(ctx);

    // 3. Plan (conditional).
    let plan = null;
    let planEntityId = null;
    let tokens = 0;
    if (needsPlanning(prompt)) {
      const planned = await generatePlan(prompt, worldSnapshot, ctx);
      plan = planned.plan;
      tokens += planned.tokens;
      planEntityId = await persistPlan(prompt, plan, ctx);
    }

    // 4. Execute (bounded).
    const exec = await runExecute(prompt, worldSnapshot, plan, ctx);
    tokens += exec.tokens;
    let text = exec.text;
    let refined = false;

    // Step-budget runaway: stop and escalate rather than loop or verify.
    if (ctx.stepBudgetExhausted) {
      await escalate(ctx, "agent.step_budget_exhausted", { run_id: runId, ledger: ctx.ledger });
      const outcome = "step_budget_exhausted";
      await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, text, refined, started });
      return { success: false, runId, outcome, text, error: outcome };
    }

    // 5. Verify + one bounded refine round.
    if (ctx.pendingApprovals.length === 0) {
      const verdict = await critique(prompt, text, ctx);
      tokens += verdict.tokens;
      if (!verdict.critique.ok && verdict.critique.fixable) {
        const redo = await runExecute(prompt, worldSnapshot, plan, ctx, verdict.critique.issue);
        tokens += redo.tokens;
        if (redo.text) text = redo.text;
        refined = true;
      }
    }

    const outcome = ctx.pendingApprovals.length > 0 ? "awaiting_approval" : "completed";
    await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, text, refined, started });

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
      latency_ms: Date.now() - started,
      refined: false,
      outcome: "failed",
    });
    return { success: false, runId, outcome: "failed", error: error.message };
  }
}

// Persists the plan status + the agent.turn row.
async function finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, text, refined, started }) {
  const planStatus = outcome === "completed" ? "completed" : outcome === "awaiting_approval" ? "awaiting_approval" : "failed";
  if (plan) await updatePlanStatus(planEntityId, plan, prompt, planStatus, ctx);
  await persistTurn(ctx, {
    run_id: ctx.runId,
    goal: prompt,
    plan: plan ? plan.stages : null,
    tool_calls: ctx.ledger,
    model: ctx.model,
    tokens,
    latency_ms: Date.now() - started,
    refined,
    outcome,
    result_preview: (text || "").slice(0, 500),
  });
}
