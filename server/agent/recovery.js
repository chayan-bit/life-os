// Bounded self-healing ladder (issue #129, docs/AGENT-CORE.md §9). Deliberately
// minimal: retry -> argument repair -> tool substitute -> one replan ->
// degrade -> escalate, all sharing ONE per-turn recovery budget so recovery
// itself can never loop. This module holds the shared primitives; executor.js
// wires retry/repair/substitute into `runTool`, loop.js wires replan/degrade/
// escalate + the query circuit-breaker around the model calls.
export const RECOVERY_BUDGET = 4;
export const RETRY_BACKOFF_MS = 250;
// Two consecutive model-call throws within a turn trip the breaker - a stand-in
// for a full per-provider circuit breaker (see docs/AGENT-CORE.md §9 deviation
// note: `lifeos-agents` has no runtime provider fallback to reuse from JS).
export const QUERY_BREAKER_THRESHOLD = 2;

// A malformed tool call surfaces as one of these HTTP statuses - a client
// error, never a transient blip, so it is repaired (schema hint), not retried.
export const ARG_REPAIR_STATUSES = new Set([400, 422]);

// Read-only equivalents only - never a write/gated tool, so a substitute can
// never actuate something the model didn't ask for.
export const SUBSTITUTES = Object.freeze({
  "search.query": "memory.recall",
  "memory.recall": "search.query",
  "entity.get": "entity.list",
});

export function isRetryableStatus(status) {
  return typeof status === "number" && status >= 500;
}

// True while the shared per-turn budget still has capacity for one more
// recovery action of ANY kind (retry, repair, substitute, or replan).
export function hasBudget(ctx) {
  return (ctx.recoveryBudget ?? RECOVERY_BUDGET) > 0;
}

// Appends one ledger entry to ctx.recoveries and decrements the shared
// budget. Never mutates in place beyond ctx's own bookkeeping fields - the
// entry object itself is a fresh copy.
export function recordRecovery(ctx, kind, tool, ok) {
  ctx.recoveries = ctx.recoveries ?? [];
  ctx.recoveries.push({ kind, tool: tool ?? null, ok: Boolean(ok) });
  ctx.recoveryBudget = (ctx.recoveryBudget ?? RECOVERY_BUDGET) - 1;
}

// A compact rendering of a registry tool's expected args, derived from its
// Zod raw shape - handed back to the model as `repair_hint` alongside the
// server's own error message.
export function schemaHint(entry) {
  const shape = entry?.inputSchema ?? {};
  const fields = Object.entries(shape).map(([key, zType]) => {
    const optional = typeof zType?.isOptional === "function" && zType.isOptional();
    return optional ? `${key}?` : key;
  });
  return `expected args: { ${fields.join(", ")} }`;
}

// Backoff sleep, injectable via ctx.sleepFn for tests (never a real timer in
// a test run).
export async function recoverySleep(ctx) {
  const sleepFn = ctx.sleepFn ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  await sleepFn(RETRY_BACKOFF_MS);
}

// Wraps queryFn with the circuit breaker: two consecutive throws within this
// turn (any call site - plan, execute, verify, replan) opens the breaker, and
// every call after that fails fast without ever reaching the model again.
// A successful call resets the consecutive-failure counter.
export function wrapQueryFnWithBreaker(queryFn, ctx) {
  return async function* breakerWrappedQueryFn(request) {
    if (ctx.breakerOpen) {
      throw new Error("circuit_breaker_open: model provider unavailable this turn");
    }
    try {
      for await (const message of queryFn(request)) {
        yield message;
      }
      ctx.queryFailures = 0;
    } catch (err) {
      ctx.queryFailures = (ctx.queryFailures ?? 0) + 1;
      if (ctx.queryFailures >= QUERY_BREAKER_THRESHOLD) ctx.breakerOpen = true;
      throw err;
    }
  };
}
