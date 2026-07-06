// Capture/query command logic - issues #65/#66. Kept as plain functions over
// (db, workspaceId, ...args) -> reply string, separate from bot.ts's grammY
// wiring, so each command is testable without constructing a Context.
// Everything here is a "medium action" per #65's scope (an entity/event read
// or write in the bot's own workspace); #66's /draft is the one exception -
// an outward action - and it stays gated (creates a pending_approval row,
// nothing more) exactly like every draft_action-backed route in
// services/lifeos-api.
import type { WorkerDb } from "@lifeos/db/client/worker";
import {
  type Entity,
  createEntity,
  listEntities,
  listInbox,
  listOpenTasksDueBy,
  markTaskDoneBySuffix,
} from "./entities.js";
import { sumClosedTradePnl } from "./events.js";
import type { ApprovalResult } from "./approvals.js";
import { enqueueJob } from "./jobs.js";
import { enqueueModuleRequest } from "./moduleRequests.js";
import { recallEntities } from "./recall.js";

const SHORT_ID_LEN = 6;

function shortId(id: string): string {
  return id.slice(-SHORT_ID_LEN);
}

// Default timezone for "today"-style windows - per the user (CLAUDE.md), not
// UTC. Callers with an `env` to read from thread `env.LIFEOS_TZ` through
// instead (index.ts's `scheduled` handler -> digest.ts -> here); bot.ts's
// live `/today` command has no such plumbing yet, so it falls back to this
// default, which is still correct for the common case.
const DEFAULT_TZ = "Asia/Kolkata";
const DAY_MS = 86_400_000;

export interface DayWindow {
  startSecs: number;
  endSecs: number;
  startIso: string;
  endIso: string;
}

// This instant's UTC offset in minutes for `timeZone`, via the runtime's own
// Intl/ICU data (correct for any IANA zone, DST-aware) rather than a
// hand-maintained offset table - Asia/Kolkata's is a fixed +330 (IST,
// +05:30, no DST), but this works for whatever `LIFEOS_TZ` names too.
function timezoneOffsetMinutes(atMs: number, timeZone: string): number {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).formatToParts(new Date(atMs));

  const part = (type: string) => Number(parts.find((p) => p.type === type)?.value ?? "0");
  // Reading `timeZone`'s wall-clock fields for this instant, then
  // reinterpreting them AS IF they were UTC, isolates exactly the offset -
  // the difference from the instant's real UTC millis.
  const asIfUtcMs = Date.UTC(part("year"), part("month") - 1, part("day"), part("hour"), part("minute"), part("second"));
  return Math.round((asIfUtcMs - atMs) / 60_000);
}

// The "today" window (finding 41, correctness audit) - previously computed
// on the UTC calendar day, wrong for most of the day for an IST (or any
// non-UTC) user. E.g. at 9pm IST (same UTC calendar day), the old UTC
// end-of-day cutoff (23:59:59 UTC) lands at 5:29am IST the NEXT day, so
// `/today` silently counted a chunk of tomorrow's IST tasks as due today.
export function dayWindow(nowSecs: number, tz: string = DEFAULT_TZ): DayWindow {
  const nowMs = nowSecs * 1000;
  const offsetMs = timezoneOffsetMinutes(nowMs, tz) * 60_000;
  const shiftedMs = nowMs + offsetMs; // "as if" UTC millis were already `tz`
  const startMs = Math.floor(shiftedMs / DAY_MS) * DAY_MS - offsetMs;
  const endMs = startMs + DAY_MS - 1000; // inclusive last second of the day

  return {
    startSecs: Math.floor(startMs / 1000),
    endSecs: Math.floor(endMs / 1000),
    startIso: new Date(startMs).toISOString(),
    endIso: new Date(endMs).toISOString(),
  };
}

export async function captureTask(db: WorkerDb, workspaceId: string, text: string): Promise<string> {
  const title = text.trim();
  if (!title) return "Usage: /task <what needs doing>";

  const entity = await createEntity(db, workspaceId, { module: "tasks", type: "task", title, status: "open", source: "telegram" });
  return `Task captured [${shortId(entity.id)}]: ${title}`;
}

export async function captureTopic(db: WorkerDb, workspaceId: string, text: string): Promise<string> {
  const title = text.trim();
  if (!title) return "Usage: /topic <what to learn>";

  const entity = await createEntity(db, workspaceId, { module: "learning", type: "topic", title, source: "telegram" });
  return `Topic added to inbox [${shortId(entity.id)}]: ${title}`;
}

export interface CaptureDraftResult {
  reply: string;
  entity: Entity | null; // null only on the empty-input usage message
}

// Outward actions are human-gated (docs/ARCHITECTURE.md hard rules): this
// only ever creates a pending_approval row, exactly like every
// draft_action-backed route in services/lifeos-api. Returns the entity too
// (not just a reply string, unlike the other capture commands) so bot.ts can
// attach an approve/deny inline keyboard (issue #66) to the same message.
export async function captureDraft(db: WorkerDb, workspaceId: string, text: string): Promise<CaptureDraftResult> {
  const body = text.trim();
  if (!body) return { reply: "Usage: /draft <what to draft>", entity: null };

  const entity = await createEntity(db, workspaceId, {
    module: "bot",
    type: "draft",
    status: "pending_approval",
    attrs: { text: body },
    source: "telegram",
  });
  return { reply: `Drafted [${shortId(entity.id)}], awaiting approval: ${body}`, entity };
}

export async function markDone(db: WorkerDb, workspaceId: string, suffix: string): Promise<string> {
  const arg = suffix.trim();
  if (!arg) return "Usage: /done <task id>";

  const result = await markTaskDoneBySuffix(db, workspaceId, arg);
  if (result.outcome === "not_found") return `No open task matching "${arg}".`;
  if (result.outcome === "ambiguous") return `More than one open task matches "${arg}" - use more of the id.`;
  return `Done: ${result.entity.title}`;
}

export async function today(db: WorkerDb, workspaceId: string, nowSecs: number, tz?: string): Promise<string> {
  const tasks = await listOpenTasksDueBy(db, workspaceId, dayWindow(nowSecs, tz).endSecs);
  if (tasks.length === 0) return "Nothing due today.";

  return tasks.map((t) => `[${shortId(t.id)}] ${t.title}`).join("\n");
}

export async function inbox(db: WorkerDb, workspaceId: string): Promise<string> {
  const rows = await listInbox(db, workspaceId);
  if (rows.length === 0) return "Inbox is empty.";

  return rows.map((e) => `[${shortId(e.id)}] (${e.module}/${e.type}) ${e.title ?? "(untitled)"}`).join("\n");
}

export async function pnl(db: WorkerDb, workspaceId: string): Promise<string> {
  const total = await sumClosedTradePnl(db, workspaceId);
  const sign = total >= 0 ? "+" : "";
  return `All-time realized PnL: ${sign}${total.toFixed(2)}`;
}

// Spaced-repetition prompt, naive: the learning/topic entity untouched the
// longest. No SM-2 scheduling / quality-rating loop yet (docs/MODULES.md's
// naive-but-real precedent, same as reading.rs's link_topics) - just enough
// to make `/quiz` do something real on top of `/topic` captures.
export async function quiz(db: WorkerDb, workspaceId: string): Promise<string> {
  const topics = await listEntities(db, workspaceId, { module: "learning", type: "topic", limit: 500 });
  if (topics.length === 0) return "No topics to quiz yet - add one with /topic.";

  const oldest = topics.reduce((least: Entity, t: Entity) => (t.updatedAt < least.updatedAt ? t : least));
  return `Quiz: what do you remember about "${oldest.title}"?`;
}

// issue #66: what a `/pending` listing shows above each item's approve/deny
// buttons - drafts (module='bot', attrs.text) get their raw text, anything
// else (draft_action-backed entities from services/lifeos-api, e.g.
// slack_post/gmail_send) falls back to its title/module/type.
export function formatPendingApproval(entity: Entity): string {
  if (entity.module === "bot" && entity.type === "draft") {
    const attrs = JSON.parse(entity.attrs) as { text?: string };
    return `[${shortId(entity.id)}] ${attrs.text ?? "(no text)"}`;
  }
  return `[${shortId(entity.id)}] (${entity.module}/${entity.type}) ${entity.title ?? "(untitled)"}`;
}

export function formatApprovalResult(result: ApprovalResult): string {
  if (result.outcome === "not_found") return "That draft no longer exists.";
  if (result.outcome === "already_resolved") return `Already ${result.entity.status} - no change.`;
  if (result.outcome === "requires_typed_confirm") return `Typed phrase did not match "${result.phrase}" - not approved.`;
  if (result.outcome === "approved") return "Approved - queued for execution.";
  return "Denied.";
}

// `/addmodule <prompt>` (issue #67, README.md's "Mac offline (queued)"
// flow): writes a `module_requests` row and replies "queued" - never writes
// code or files itself (there's no filesystem on a Cloudflare Worker to
// write to; the real scaffold.js build only ever runs on the Mac). `chatId`
// (issue #78) is carried through so `lifeos-drain` can notify this same chat
// once the Mac actually builds it.
export async function requestModule(
  db: WorkerDb,
  workspaceId: string,
  text: string,
  chatId?: string,
): Promise<string> {
  const prompt = text.trim();
  if (!prompt) return "Usage: /addmodule <what you want added>";

  await enqueueModuleRequest(db, workspaceId, prompt, chatId);
  return "Queued. The Mac will build it next time it's awake.";
}

// `/ingest <text>` (issue #67): the generic heavy-work queue for anything
// that isn't a module build - e.g. "ingest this" media/document processing
// (docs/MEDIA-INTELLIGENCE.md), which also only ever runs on the Mac
// (lifeos-ingest). Distinct from `/addmodule`'s dedicated `module_requests`
// table - this uses the general-purpose `jobs` table instead.
export async function ingest(db: WorkerDb, workspaceId: string, text: string): Promise<string> {
  const payload = text.trim();
  if (!payload) return "Usage: /ingest <url or description>";

  await enqueueJob(db, workspaceId, "ingest", { text: payload });
  return "Queued for the Mac.";
}

// `/recall <query>` (issue #69): "what did I note about X". Lexical
// substring match over the canonical DB, workspace-scoped, citing each hit's
// short id/module/type/title so the user can jump to it - see recall.ts for
// why this isn't the full FTS5+memvec RRF hybrid (that lives on the Mac).
export async function recall(db: WorkerDb, workspaceId: string, text: string): Promise<string> {
  const query = text.trim();
  if (!query) return "Usage: /recall <what to look for>";

  const hits = await recallEntities(db, workspaceId, query);
  if (hits.length === 0) return `Nothing found for "${query}".`;

  return hits.map((e) => `[${shortId(e.id)}] (${e.module}/${e.type}) ${e.title ?? "(untitled)"}`).join("\n");
}
