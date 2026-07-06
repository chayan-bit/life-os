// Daily/scheduled digest (issue #71): due tasks, blocked items, realized PnL,
// and drafts awaiting approval. Pure content builder over the existing,
// already-tested command functions - no Telegram call here, so it's fully
// unit-testable against a local DB. `index.ts`'s `scheduled` handler is the
// thin, network-touching glue that actually sends it, same reason
// `/telegram` isn't unit-tested at that layer (worker/test/index.test.ts).
import type { WorkerDb } from "@lifeos/db/client/worker";
import { listPendingApprovals } from "./approvals.js";
import { formatPendingApproval, inbox, pnl, today } from "./commands.js";

// `tz` (finding 41, correctness audit): "due today" must follow the user's
// calendar day, not UTC's - see commands.ts's `dayWindow` for why. Optional
// and forwarded straight to `today()`, which falls back to the same default
// (Asia/Kolkata) when omitted - index.ts's `scheduled` handler passes
// `env.LIFEOS_TZ` here; this module's own tests can omit it.
export async function buildDigest(db: WorkerDb, workspaceId: string, nowSecs: number, tz?: string): Promise<string> {
  const [dueToday, blocked, realizedPnl, pending] = await Promise.all([
    today(db, workspaceId, nowSecs, tz),
    // No `task.blocked` event exists yet - "uncategorized captures" is the
    // closest analog to "blocked items" until one does, same note as
    // docs/PLATFORM-SYSTEMS.md §3's #65 entry.
    inbox(db, workspaceId),
    pnl(db, workspaceId),
    listPendingApprovals(db, workspaceId),
  ]);

  const pendingSection = pending.length === 0 ? "Nothing pending approval." : pending.map(formatPendingApproval).join("\n");

  return ["Daily digest", "", "Due today:", dueToday, "", "Inbox (uncategorized / blocked):", blocked, "", realizedPnl, "", "Pending approval:", pendingSection].join("\n");
}
