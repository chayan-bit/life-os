// lifeos-memory activation recall injection (docs/AGENT-CORE.md §5, issue
// #124). A thin consumer of the existing `/api/memory/context` (the
// token-budgeted compiler in `compiler.rs`) and `/api/memory/ingest` routes -
// no new subsystem, no Rust changes. The loop is a consumer of the brain
// that already exists in `services/lifeos-memory`, not a re-implementation.

const MEMORY_BUDGET_TOKENS = 2000;
const MEMORY_TOP_K = 8;
const RESULT_PREVIEW_CHARS = 500;
const MEMORY_BLOCK_LABEL = "## Memory (activation recall)";
// The compiler's own last-K-turns window (compiler.rs `BudgetSpec::recent_turns_k`
// default) - kept in sync so the client-side fetch bound matches what the
// compiler can actually use.
export const RECENT_TURNS_K = 6;

// Extracts the compiler's compiled text from `/api/memory/context`'s
// `context` field. `context_handler` (services/lifeos-api/src/routes/memory.rs)
// serializes the Rust `CompiledContext` struct (services/lifeos-memory/src/
// compiler.rs) verbatim - an object shaped `{ text, sections, tokens_used,
// budget_tokens }`, not a bare string. Tolerates a plain string too (older
// mocks / a future simpler compiler response) so this stays a passthrough,
// never a re-query, regardless of which shape arrives.
function extractCompiledText(context) {
  if (typeof context === "string") return context;
  if (context && typeof context.text === "string") return context.text;
  return null;
}

// Fetches the last `k` agent turns for `workspaceId`, oldest-to-newest, in
// the `Turn { role, content }` shape `ContextRequest.recent_turns` expects
// (services/lifeos-memory/src/compiler.rs). Turns are read back from the
// `agent.turn` events `persistTurn` (loop.js) already writes - no new
// storage, no new write path. `GET /api/event` returns newest-first
// (`ORDER BY ts DESC`, services/lifeos-api/src/routes/event.rs), so the
// result is reversed to the oldest-to-newest order `compile_context`'s
// `.rev().take(k)` window relies on (it treats the last element as most
// recent). Best-effort: any failure degrades to no recent-turns context,
// never fails the turn.
export async function fetchRecentTurns(httpFn, workspaceId, k = RECENT_TURNS_K) {
  try {
    const res = await httpFn("GET", `/api/event?type=agent.turn&limit=${k}`);
    const events = res?.ok && Array.isArray(res.data) ? res.data : [];
    return events
      .slice(0, k)
      .reverse()
      .map((event) => {
        const goal = event.attrs?.goal ?? "";
        const outcome = event.outcome ?? event.attrs?.outcome ?? "unknown";
        return { role: "agent", content: `${goal} -> ${outcome}` };
      });
  } catch {
    return [];
  }
}

// Fetches the compiler's token-budgeted context block for `goal`, workspace
// scoped, threading `recentTurns` through as the compiler's last-K-turns
// working-memory window. Returns { block: null, recall: null } on ANY
// failure or an empty compiled context - a memory outage (or a turn needing
// no memory) must never fail, or pad, the turn. `block` is a passthrough of
// the compiler's own compiled text, clearly labeled, never a re-query or a
// raw memory dump.
export async function fetchMemoryContext(httpFn, workspaceId, goal, recentTurns = []) {
  try {
    const res = await httpFn("POST", "/api/memory/context", {
      query: goal,
      workspace_id: workspaceId,
      recent_turns: recentTurns,
      budget_tokens: MEMORY_BUDGET_TOKENS,
      top_k: MEMORY_TOP_K,
    });
    const compiled = res?.ok ? extractCompiledText(res.data?.context) : null;
    if (!compiled || !compiled.trim()) return { block: null, recall: null };
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
