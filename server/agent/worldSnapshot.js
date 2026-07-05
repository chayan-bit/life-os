// Compact world-model snapshot (docs/AGENT-CORE.md §3 step 2, §11). A cheap,
// bounded read over `entities`/`jobs` that gives the turn its situational
// block so the agent never re-asks for context it could compute. Counts
// only, no row dumps - lifeos-memory activation recall (#124) is fetched and
// appended alongside this snapshot in loop.js, not here - see
// server/agent/memoryContext.js.

const LIST_LIMIT = 200;
const DONE_STATUSES = new Set(["done", "completed", "archived", "cancelled"]);
const MS_PER_DAY = 86_400_000;

// Returns the rows on success, an empty list on a well-formed non-ok reply,
// or `null` on a thrown/transport-level failure - `null` is distinguished
// from "legitimately zero rows" so buildWorldSnapshot can tell a broken read
// from an empty workspace.
async function safeGet(httpFn, path) {
  try {
    const res = await httpFn("GET", path);
    return res?.ok && Array.isArray(res.data) ? res.data : [];
  } catch {
    return null;
  }
}

// Entities store attrs as an object or a JSON string depending on the read
// path; parse defensively so a malformed row never crashes the snapshot
// (same defensive shape as gate.js's readAttrs).
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

const isOpenTask = (task) => !DONE_STATUSES.has(String(task?.status || "").toLowerCase());

// A task is "due" the same way the `/today` bot command already defines it
// (docs/MODULES.md §2.2): undated tasks never count, dated ones count once
// their `attrs.due` UTC day is today or earlier. No separate follow-up
// entity type exists in the schema, so tasks-with-a-due-date is the honest
// signal to reuse rather than inventing new schema.
function isDueTask(task, todayEndMs) {
  const due = readAttrs(task).due;
  if (!due) return false;
  const dueMs = Date.parse(due);
  return Number.isFinite(dueMs) && dueMs < todayEndMs;
}

// A trade is "open" per its documented attrs shape (docs/MODULES.md §2.4):
// no `closed_at` yet. Trades have no separate lifecycle status convention,
// so this is the one honest signal the schema already gives.
const isOpenTrade = (trade) => !readAttrs(trade).closed_at;

// Returns a short text block for the turn's prompt, or null if the read
// failed outright. Never throws - a snapshot failure must not fail the
// turn, this is context, not a gate.
export async function buildWorldSnapshot(ctx) {
  const { httpFn, nowSecs = Math.floor(Date.now() / 1000) } = ctx;
  try {
    const todayEndMs = (Math.floor((nowSecs * 1000) / MS_PER_DAY) + 1) * MS_PER_DAY;
    const results = await Promise.all([
      safeGet(httpFn, `/api/entity?module=tasks&limit=${LIST_LIMIT}`),
      safeGet(httpFn, `/api/entity?module=trading&type=trade&limit=${LIST_LIMIT}`),
      safeGet(httpFn, `/api/entity?status=pending_approval&limit=${LIST_LIMIT}`),
      safeGet(httpFn, `/api/jobs?status=pending&limit=${LIST_LIMIT}`),
    ]);

    // Every category unreachable -> the read is broken, not the workspace
    // empty. Omit the whole block rather than render a misleading all-zero
    // snapshot; the turn still proceeds without it (see loop.js).
    if (results.every((r) => r === null)) return null;

    const [tasks, trades, pendingApprovals, jobs] = results.map((r) => r ?? []);
    const openTasks = tasks.filter(isOpenTask);
    // "Drafts" and "pending approvals" collapse to the same signal in this
    // schema: draft.create (the only gated write, actionRegistry.js) is what
    // sets status='pending_approval', on any module - there is no separate
    // universal draft type to query.
    return [
      "World snapshot (read-only situational context):",
      `- open tasks: ${openTasks.length}`,
      `- open trades: ${trades.filter(isOpenTrade).length}`,
      `- drafts / pending approvals: ${pendingApprovals.length}`,
      `- pending jobs: ${jobs.length}`,
      `- tasks due today or overdue: ${openTasks.filter((t) => isDueTask(t, todayEndMs)).length}`,
    ].join("\n");
  } catch {
    return null;
  }
}
