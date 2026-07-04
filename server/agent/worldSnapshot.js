// Compact world-model snapshot (docs/AGENT-CORE.md §3 step 2, §11). A cheap,
// bounded read over `entities` that gives the turn its situational block so the
// agent never re-asks for context it could compute. This is a genuine stub:
// counts only, no memory injection yet.
//
// #124: lifeos-memory activation recall injected here.

const LIST_LIMIT = 200;
const DONE_STATUSES = new Set(["done", "completed", "archived", "cancelled"]);

async function safeList(httpFn, query) {
  try {
    const res = await httpFn("GET", `/api/entity?${query}`);
    return res?.ok && Array.isArray(res.data) ? res.data : [];
  } catch {
    return [];
  }
}

// Returns a short text block for the system prompt. Never throws - a snapshot
// failure must not fail the turn.
export async function buildWorldSnapshot(ctx) {
  const { httpFn } = ctx;
  const [tasks, pending] = await Promise.all([
    safeList(httpFn, `module=tasks&limit=${LIST_LIMIT}`),
    safeList(httpFn, `status=pending_approval&limit=${LIST_LIMIT}`),
  ]);

  const openTasks = tasks.filter((t) => !DONE_STATUSES.has(String(t?.status || "").toLowerCase())).length;
  const pendingDrafts = pending.length;

  return [
    "World snapshot (read-only situational context):",
    `- open tasks: ${openTasks}`,
    `- drafts awaiting approval: ${pendingDrafts}`,
  ].join("\n");
}
