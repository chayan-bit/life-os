// The general plan -> execute -> verify agent turn (docs/AGENT-CORE.md §3, §7).
// Orchestrates: gate -> world snapshot -> plan? -> execute -> verify/refine ->
// persist. Invents no new authority - it sequences already-gated actions and
// records one append-only `events('agent.turn')` row per turn.
import { query as defaultQuery } from "@anthropic-ai/claude-agent-sdk";
import { checkGate } from "./gate.js";
import { buildWorldSnapshot } from "./worldSnapshot.js";
import { needsPlanning, generatePlan, persistPlan, updatePlanStatus, PLANNER_PROMPT_GROUP } from "./planner.js";
import { runExecute } from "./executor.js";
import { critique } from "./critic.js";
import { createHttpFn } from "./http.js";
import { REGISTRY } from "./actionRegistry.js";
import { indexTools, retrieveTools } from "./toolRag.js";
import { fetchMemoryContext, fetchRecentTurns, ingestTurnOutcome } from "./memoryContext.js";
import { correctiveRetrieve, isQuestionTurn, buildAbstentionResponse } from "./correctiveRag.js";
import { fetchActiveManual } from "./manual.js";
import { distillLesson } from "./reflect.js";
import { emptyUsage } from "./usage.js";
import { isCacheMode, looksActiony, probe as cacheProbe, store as cacheStore } from "./llmCache.js";
import { hasBudget, recordRecovery, wrapQueryFnWithBreaker, RECOVERY_BUDGET } from "./recovery.js";
import { recordOutcome } from "./strategy.js";

// A genuinely low-confidence answer to a weak/no-context question turn
// abstains rather than fabricating (docs/AGENT-CORE.md §12) - the single
// cheapest defense against hallucinated actuation.
const ABSTAIN_THRESHOLD = 0.4;

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
  // Dry-run (issue #140, docs/AGENT-CORE.md §13): no turn trace is written -
  // the eval runner reads the in-memory ledger/result directly instead.
  if (ctx.dryRun) return;
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
        rag: ctx.rag ?? null,
      },
      workspace_id: ctx.workspaceId,
    });
  } catch {
    // Best-effort trace; a persistence failure must not mask the turn result.
  }
}

async function escalate(ctx, type, attrs) {
  if (ctx.dryRun) return;
  try {
    await ctx.httpFn("POST", "/api/event", { type, actor: "agent", attrs, workspace_id: ctx.workspaceId });
  } catch {
    // Escalation is best-effort.
  }
}

// Ladder step 5+6 (docs/AGENT-CORE.md §9): recovery is exhausted (or the
// breaker is open, or a replan already happened this turn) - return the
// partial result honestly rather than pretending nothing ran, and emit an
// append-only escalation event so it surfaces downstream (Telegram
// consumption is out of scope here, per the issue).
async function degradeAndEscalate(ctx, { prompt, plan, planEntityId, started, error }) {
  const completedTools = ctx.ledger.filter((entry) => entry.ok).map((entry) => entry.tool);
  const reason = error?.message ?? "unknown error";
  const text = completedTools.length
    ? `Partially completed (${completedTools.join(", ")}) before this failed: ${reason}.`
    : `Could not complete the request: ${reason}.`;
  const outcome = "degraded";

  await escalate(ctx, "agent.escalation", {
    run_id: ctx.runId,
    reason,
    ledger: ctx.ledger,
    breaker_open: Boolean(ctx.breakerOpen),
    replanned: Boolean(ctx.replanned),
  });
  await finalize(ctx, {
    plan,
    planEntityId,
    prompt,
    outcome,
    tokens: 0,
    usage: emptyUsage(),
    text,
    refined: false,
    started,
    error: reason,
  });
  return { success: false, runId: ctx.runId, outcome, text, error: reason };
}

// Ladder step 4 (docs/AGENT-CORE.md §9): one replan, only when the breaker
// isn't open, no replan has happened yet this turn, budget remains, and a
// plan existed to revise. The failure context carries the completed-work
// ledger so the replanned execute pass doesn't repeat successful steps -
// `ctx.stepCount` already threads the remaining step budget (it is the same
// mutable counter `runTool` enforces MAX_STEPS against). Falls through to
// degrade+escalate on any further failure - never a second replan.
async function recoverExecuteFailure(ctx, { prompt, context, plan, planEntityId, started, error }) {
  const canReplan = !ctx.breakerOpen && !ctx.replanned && Boolean(plan) && hasBudget(ctx);
  if (!canReplan) {
    return { degraded: true, returnValue: await degradeAndEscalate(ctx, { prompt, plan, planEntityId, started, error }) };
  }
  ctx.replanned = true;

  const completedTools = ctx.ledger.filter((entry) => entry.ok).map((entry) => entry.tool);
  const failureContext = [
    context,
    `A previous attempt at this turn failed: ${error.message}`,
    completedTools.length
      ? `Already completed successfully this turn - preserve this work, do not repeat it: ${completedTools.join(", ")}`
      : null,
  ]
    .filter(Boolean)
    .join("\n\n");

  try {
    const replanned = await generatePlan(prompt, failureContext, ctx);
    const exec = await runExecute(prompt, failureContext, replanned.plan, ctx);
    recordRecovery(ctx, "replan", null, true);
    return { degraded: false, exec, plan: replanned.plan, tokens: replanned.tokens, usage: replanned };
  } catch (replanError) {
    recordRecovery(ctx, "replan", null, false);
    return {
      degraded: true,
      returnValue: await degradeAndEscalate(ctx, { prompt, plan, planEntityId, started, error: replanError }),
    };
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
    httpFn: opts.httpFn ?? createHttpFn(workspaceId, opts.apiBase),
    // Side-effect-free mode (issue #140, docs/AGENT-CORE.md §13): every write
    // site in this file and executor.js/gate.js checks this flag and skips
    // its HTTP call, so a dry-run turn's tool calls land in ctx.ledger with a
    // synthetic result but never touch lifeos-api.
    dryRun: Boolean(opts.dryRun),
    ledger: [],
    pendingApprovals: [],
    stepCount: 0,
    stepBudgetExhausted: false,
    nowSecs: opts.nowSecs,
    sleepFn: opts.sleepFn,
    // Self-healing (issue #129, docs/AGENT-CORE.md §9): one shared budget
    // across every recovery action this turn, plus the query circuit-breaker
    // state the wrapped queryFn below mutates directly.
    recoveryBudget: RECOVERY_BUDGET,
    recoveries: [],
    argRepaired: new Set(),
    replanned: false,
    breakerOpen: false,
    queryFailures: 0,
  };
  ctx.queryFn = wrapQueryFnWithBreaker(opts.queryFn ?? defaultQuery, ctx);

  // 1. Gate - fail closed before any model call.
  const gate = await checkGate(ctx);
  if (!gate.ok) {
    const text = gate.reason === "kill_switch" ? "Agent paused: the kill switch is on for this workspace." : null;
    return { success: false, runId, outcome: gate.reason, error: gate.reason, ledger: ctx.ledger, ...(text ? { text } : {}) };
  }

  // 1a. Cache probe (issue #127, docs/AGENT-CORE.md §10). API-key mode only,
  // and only for side-effect-free plain completions: skipped for anything
  // `needsPlanning` flags as multi-step, and for anything `looksActiony`
  // flags as an imperative mutation request - a cached answer must never
  // skip a needed mutation. No spend before the probe: this sits before
  // context assembly and before any model call.
  const isPlanningNeeded = needsPlanning(prompt);
  // Dry-run never serves (or stores to) the cache - a cache hit would skip
  // the execute stage entirely, which defeats the whole point of a
  // tool-routing eval (issue #140).
  const cacheEligible = isCacheMode() && !isPlanningNeeded && !looksActiony(prompt) && !ctx.dryRun;
  const cacheRequest = { workspace: workspaceId, model: ctx.model, prompt, params: {} };
  if (cacheEligible) {
    const cacheResult = await cacheProbe(cacheRequest, opts.cache);
    if (cacheResult.hit) {
      const outcome = "completed";
      await finalize(ctx, {
        plan: null,
        planEntityId: null,
        prompt,
        outcome,
        tokens: 0,
        usage: emptyUsage(),
        text: cacheResult.completion,
        refined: false,
        started,
        cache: cacheResult.hit,
      });
      return { success: true, runId, outcome, text: cacheResult.completion, cache: cacheResult.hit };
    }
  }

  try {
    // 2. Context assembly - world snapshot + lifeos-memory activation recall
    // (docs/AGENT-CORE.md §5, #124). The memory block is the compiler's own
    // token-budgeted output, appended verbatim - never a re-query, never a
    // raw dump.
    const worldSnapshot = await buildWorldSnapshot(ctx);
    // Corrective-RAG (issue #130, docs/AGENT-CORE.md §12): the grade/rewrite/
    // re-retrieve/cite cycle only applies to question turns - an imperative
    // request never needs a citation instruction or a web-fallback nudge.
    const isQuestion = isQuestionTurn(prompt);
    // Best-effort, once per turn (docs/AGENT-CORE.md §5): both the plain and
    // corrective-RAG context paths below share this same window so a
    // rewritten re-fetch doesn't re-read a different slice of history.
    const recentTurns = await fetchRecentTurns(ctx.httpFn, ctx.workspaceId);
    const memory = isQuestion
      ? await correctiveRetrieve(
          { httpFn: ctx.httpFn, workspaceId: ctx.workspaceId, queryFn: ctx.queryFn },
          ctx,
          prompt,
          recentTurns,
        )
      : await fetchMemoryContext(ctx.httpFn, ctx.workspaceId, prompt, recentTurns);
    ctx.memoryInjected = isQuestion ? Boolean(memory.hasContent) : Boolean(memory.block);
    ctx.recall = memory.recall;
    ctx.rag = isQuestion ? memory.rag : null;
    const manual = await fetchActiveManual(ctx.httpFn, ctx.workspaceId);
    const context = [worldSnapshot, memory.block, manual].filter(Boolean).join("\n\n");

    // 3. Plan (conditional). Tracks tokensIn/tokensOut separately (not just
    // the combined `tokens`) for the run-log lens's tokens_in/tokens_out
    // columns (issue #125).
    let plan = null;
    let planEntityId = null;
    let tokens = 0;
    let usage = emptyUsage();
    if (isPlanningNeeded) {
      const planned = await generatePlan(prompt, context, ctx);
      plan = planned.plan;
      tokens += planned.tokens;
      usage = addUsage(usage, planned);
      // Dry-run (issue #140): the plan still guides execution, it just isn't
      // persisted as a pipeline_run entity - nothing to update at finalize.
      planEntityId = ctx.dryRun ? null : await persistPlan(prompt, plan, ctx);
    }

    // 3b. Tool-RAG: index the registry lazily (fire-and-forget-ish - never
    // fails the turn, indexTools already catches internally) then retrieve
    // the top-K relevant tools + core set for this turn's execute stage.
    // Skipped in dry-run (issue #140) - it writes a digest entity and shells
    // to memvec; retrieveTools already falls back to the full catalog when
    // no index is available, which is a fine (arguably better) offering for
    // a routing eval.
    if (!ctx.dryRun) {
      try {
        await indexTools(REGISTRY, { httpFn: ctx.httpFn, workspaceId: ctx.workspaceId, ...opts.toolRag });
      } catch {
        // Defense-in-depth only; indexTools does not throw.
      }
    }
    const retrieval = await retrieveTools(prompt, REGISTRY, opts.toolRag);
    ctx.toolNames = retrieval.tools;
    ctx.toolsOffered = retrieval.tools.length;
    ctx.toolragFallback = retrieval.fallback;

    // 4. Execute (bounded). A thrown error here (a dead model provider, a
    // network failure that survived executor.js's own retry) enters the
    // self-healing ladder's replan/degrade/escalate tail (issue #129,
    // docs/AGENT-CORE.md §9) instead of failing the whole turn outright.
    let exec;
    try {
      exec = await runExecute(prompt, context, plan, ctx);
    } catch (error) {
      const recovery = await recoverExecuteFailure(ctx, { prompt, context, plan, planEntityId, started, error });
      if (recovery.degraded) return recovery.returnValue;
      exec = recovery.exec;
      plan = recovery.plan;
      tokens += recovery.tokens;
      usage = addUsage(usage, recovery.usage);
    }
    tokens += exec.tokens;
    usage = addUsage(usage, exec);
    let text = exec.text;
    let refined = false;

    // Step-budget runaway: stop and escalate rather than loop or verify.
    if (ctx.stepBudgetExhausted) {
      await escalate(ctx, "agent.step_budget_exhausted", { run_id: runId, ledger: ctx.ledger });
      const outcome = "step_budget_exhausted";
      await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started });
      return { success: false, runId, outcome, text, error: outcome, ledger: ctx.ledger };
    }

    // 5. Verify + one bounded refine round.
    let critiqueVerdict = null;
    if (ctx.pendingApprovals.length === 0) {
      const verdict = await critique(prompt, text, ctx);
      tokens += verdict.tokens;
      usage = addUsage(usage, verdict);
      critiqueVerdict = verdict.critique;
      if (!verdict.critique.ok && verdict.critique.fixable) {
        const redo = await runExecute(prompt, context, plan, ctx, verdict.critique.issue);
        tokens += redo.tokens;
        usage = addUsage(usage, redo);
        if (redo.text) text = redo.text;
        refined = true;
      }
    }

    // Confidence-based abstention (docs/AGENT-CORE.md §12): only for question
    // turns whose final memory grade never reached "sufficient", only on a
    // genuinely low critic confidence, and never on a turn that already
    // executed a tool successfully - real work is reported, not discarded.
    const toolsExecuted = ctx.ledger.some((entry) => entry.ok);
    const finalGrade = ctx.rag ? ctx.rag.regraded ?? ctx.rag.grade : null;
    const weakContext = finalGrade === "weak" || finalGrade === "none";
    const lowConfidence = Boolean(critiqueVerdict) && critiqueVerdict.confidence < ABSTAIN_THRESHOLD;
    const shouldAbstain =
      ctx.pendingApprovals.length === 0 && isQuestion && weakContext && lowConfidence && !toolsExecuted;
    if (shouldAbstain) {
      text = buildAbstentionResponse(critiqueVerdict.issue);
    }

    const outcome = ctx.pendingApprovals.length > 0 ? "awaiting_approval" : shouldAbstain ? "abstained" : "completed";

    // 6. Cache store (issue #127). Only a completed, probe-eligible turn that
    // made NO tool calls - a tool-using turn is NEVER cached (its effect is a
    // state change, not reusable text). Best-effort, workspace-scoped.
    if (outcome === "completed" && ctx.ledger.length === 0 && cacheEligible) {
      await cacheStore(cacheRequest, text, opts.cache);
    }

    await finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started });

    return {
      success: true,
      runId,
      outcome,
      text,
      ledger: ctx.ledger,
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
    // folding into the next sleep cycle. Best-effort - never rethrows. Skipped
    // in dry-run (issue #140) - same rule as the success-path finalize().
    if (!ctx.dryRun) await ingestTurnOutcome(ctx.httpFn, ctx.workspaceId, prompt, "failed", "");
    return { success: false, runId, outcome: "failed", error: error.message, ledger: ctx.ledger };
  }
}

// Persists the plan status + the agent.turn row, then writes the turn's
// outcome back to memory (issue #124) best-effort so the next sleep cycle
// (consolidate.rs) can fold it - no new subsystem, `events` stays the path.
async function finalize(ctx, { plan, planEntityId, prompt, outcome, tokens, usage, text, refined, started, cache, error }) {
  const planStatus =
    outcome === "completed"
      ? "completed"
      : outcome === "awaiting_approval"
        ? "awaiting_approval"
        : outcome === "degraded"
          ? "degraded"
          : outcome === "abstained"
            ? "abstained"
            : "failed";
  if (plan && !ctx.dryRun) await updatePlanStatus(planEntityId, plan, prompt, planStatus, ctx);
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
    error: error ?? (outcome === "step_budget_exhausted" ? outcome : null),
    result_preview: (text || "").slice(0, 500),
    // Present (issue #127) only on a cache-served turn ('exact' | 'semantic');
    // undefined elsewhere so a JSON.stringify of attrs simply omits the key.
    cache: cache ?? undefined,
    // Self-healing summary (issue #129, docs/AGENT-CORE.md §9) - additive,
    // always present (empty array on a turn with no recovery activity).
    recoveries: ctx.recoveries ?? [],
  });
  // Both writes below are skipped in dry-run (issue #140) - a routing eval
  // must never fold synthetic scenario turns into real memory or lessons.
  if (ctx.dryRun) return;
  await ingestTurnOutcome(ctx.httpFn, ctx.workspaceId, prompt, outcome, text);
  // planner.prompt decision group (#156, group 2 of 3): only stamped when
  // this turn actually planned (ctx.plannerVariant set by generatePlan).
  // Success = the plan executed to its resolution without needing the
  // recovery ladder's one replan and without degrading.
  if (ctx.plannerVariant) {
    await recordOutcome(ctx.httpFn, ctx.workspaceId, PLANNER_PROMPT_GROUP, ctx.plannerVariant, !ctx.replanned && outcome !== "degraded");
  }
  // Distill-after (issue #128, docs/AGENT-CORE.md §6): best-effort, bounded
  // to at most one lesson per turn, never affects the already-computed
  // turn result.
  await distillLesson(ctx.queryFn, prompt, outcome, text, {
    httpFn: ctx.httpFn,
    workspaceId: ctx.workspaceId,
    model: ctx.model,
  });
}
