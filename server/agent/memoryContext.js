// lifeos-memory activation recall injection (docs/AGENT-CORE.md §5, issue
// #124). A thin consumer of the existing `/api/memory/context` (the
// token-budgeted compiler in `compiler.rs`) and `/api/memory/ingest` routes -
// no new subsystem, no Rust changes. The loop is a consumer of the brain
// that already exists in `services/lifeos-memory`, not a re-implementation.

const MEMORY_BUDGET_TOKENS = 2000;
const MEMORY_TOP_K = 8;
const RESULT_PREVIEW_CHARS = 500;
const MEMORY_BLOCK_LABEL = "## Memory (activation recall)";

// Fetches the compiler's token-budgeted context block for `goal`, workspace
// scoped. Returns { block: null, recall: null } on ANY failure or an empty
// compiled context - a memory outage (or a turn needing no memory) must
// never fail, or pad, the turn. `block` is a passthrough of the compiler's
// own output, clearly labeled, never a re-query or a raw memory dump.
export async function fetchMemoryContext(httpFn, workspaceId, goal) {
  try {
    const res = await httpFn("POST", "/api/memory/context", {
      query: goal,
      workspace_id: workspaceId,
      recent_turns: [],
      budget_tokens: MEMORY_BUDGET_TOKENS,
      top_k: MEMORY_TOP_K,
    });
    const compiled = res?.ok ? res.data?.context : null;
    if (!compiled) return { block: null, recall: null };
    return { block: [MEMORY_BLOCK_LABEL, compiled].join("\n"), recall: res.data?.recall ?? null };
  } catch {
    return { block: null, recall: null };
  }
}

// Writes the turn's outcome back as an observation event (source: 'agent') so
// the existing sleep cycle (`consolidate.rs`) folds agent experience into
// durable memory - no new subsystem, `events` stays the single write path.
// Fire-and-forget: catches and never throws, so a write-back failure can
// never affect the turn result already returned to the caller.
export async function ingestTurnOutcome(httpFn, workspaceId, goal, outcome, resultText) {
  try {
    const preview = String(resultText || "").slice(0, RESULT_PREVIEW_CHARS);
    const content = preview ? `${goal} -> ${outcome}: ${preview}` : `${goal} -> ${outcome}`;
    await httpFn("POST", "/api/memory/ingest", {
      content,
      source: "agent",
      workspace_id: workspaceId,
    });
  } catch {
    // Best-effort write-back only; must never affect an already-returned turn.
  }
}
