// The gating state machine surfaced in Telegram - issue #66
// (docs/SECURITY.md §2): pending_approval|awaiting_approval -> approved|
// rejected, every transition an event, approval only ever enqueues work for
// the Mac to execute - the Worker itself never calls Nango's proxy, the
// browser actuator, or trade-exec directly (it holds no provider tokens,
// docs/ARCHITECTURE.md §3.1). Real dispatch of the enqueued
// `execute_approval` job is services/lifeos-drain's (issue #142): for a T3+
// build gate it resumes the halted pipeline (`run_approval_resume_from_
// payload`/`ScaffoldJsResumer`); a plain draft has nothing to resume and is
// just acknowledged, since the outward Nango/browser-actuator/trade-exec call
// itself still isn't wired up. Either way, "on approve" here means "queued
// for execution," not "executed."
import type { WorkerDb } from "@lifeos/db/client/worker";
import { type Entity, getEntityById, listEntities, transitionEntityStatus } from "./entities.js";
import { recordEvent } from "./events.js";
import { enqueueJob } from "./jobs.js";
import { APPROVED_STATUS, isPendingApproval, PENDING_STATUSES, REJECTED_STATUS } from "./status.js";

export async function listPendingApprovals(db: WorkerDb, workspaceId: string, limit = 10): Promise<Entity[]> {
  // `listEntities` only filters on a single status value, so a gate sitting
  // in either pending status (status.ts) is fetched with one call per status
  // and merged here rather than widening that shared helper for this one
  // caller - newest-first across both, then re-capped to `limit`.
  const rows = await Promise.all(PENDING_STATUSES.map((status) => listEntities(db, workspaceId, { status, limit })));
  return rows
    .flat()
    .sort((a, b) => b.createdAt - a.createdAt)
    .slice(0, limit);
}

export type ApprovalResult =
  | { outcome: "approved"; entity: Entity }
  | { outcome: "denied"; entity: Entity }
  | { outcome: "not_found" }
  | { outcome: "already_resolved"; entity: Entity }
  // A T5-style gate (attrs.requires_typed_confirm) tapped without the exact
  // phrase: not approved, and the caller is told the phrase to reply with.
  // Server-enforced, mirroring services/lifeos-api's /api/approval route.
  | { outcome: "requires_typed_confirm"; entity: Entity; phrase: string };

function parseAttrs(entity: Entity): Record<string, unknown> {
  try {
    return JSON.parse(entity.attrs) as Record<string, unknown>;
  } catch {
    return {};
  }
}

// Highest-blast-radius gates (T5 subsystem builds, gate.js §4/§8) carry
// `attrs.requires_typed_confirm`: a one-tap approve is refused; the human must
// type an exact phrase. The flag is the cross-surface contract (bot + PWA +
// API all read it); this reads it.
export function requiresTypedConfirm(entity: Entity): boolean {
  return parseAttrs(entity).requires_typed_confirm === true;
}

// The exact phrase a typed-confirm gate demands: the build node id when present
// (gate.js stamps `attrs.node`), else the entity's title, else its id.
export function confirmPhrase(entity: Entity): string {
  const node = parseAttrs(entity).node;
  if (typeof node === "string" && node.length > 0) return node;
  return entity.title ?? entity.id;
}

// DB status a resolution outcome writes - kept distinct from the outcome tag
// itself: "denied" is ApprovalResult's outcome name, but the stored status is
// "rejected" (status.ts), matching services/lifeos-api's deny transition.
const DB_STATUS_FOR_OUTCOME = { approved: APPROVED_STATUS, denied: REJECTED_STATUS } as const;

async function resolveOrAlreadyResolved(
  db: WorkerDb,
  workspaceId: string,
  id: string,
  outcome: "approved" | "denied",
): Promise<ApprovalResult> {
  const existing = await getEntityById(db, workspaceId, id);
  if (!existing) return { outcome: "not_found" };
  const fromStatus = existing.status;
  if (!isPendingApproval(fromStatus)) return { outcome: "already_resolved", entity: existing };

  // `fromStatus` (not a hardcoded constant) so the CAS matches whichever
  // pending status the entity actually carries (status.ts's
  // pending_approval OR awaiting_approval) - the guard above already proved
  // it's one of the two.
  const updated = await transitionEntityStatus(db, workspaceId, id, fromStatus, DB_STATUS_FOR_OUTCOME[outcome]);
  // A concurrent tap could win the race between the check above and the
  // conditional UPDATE - treat that as already_resolved too, not a crash.
  if (!updated) return { outcome: "already_resolved", entity: existing };

  return { outcome, entity: updated } as ApprovalResult;
}

export async function approveEntity(
  db: WorkerDb,
  workspaceId: string,
  id: string,
  typed?: string,
): Promise<ApprovalResult> {
  // Typed-confirm gate is checked BEFORE the status transition so a bare tap on
  // a T5 gate can never approve it (docs/SELF-EXTENSION-V2.md §8). A plain draft
  // (no flag) skips this entirely.
  const existing = await getEntityById(db, workspaceId, id);
  if (!existing) return { outcome: "not_found" };
  if (!isPendingApproval(existing.status)) return { outcome: "already_resolved", entity: existing };
  if (requiresTypedConfirm(existing)) {
    const phrase = confirmPhrase(existing);
    if ((typed ?? "").trim() !== phrase) return { outcome: "requires_typed_confirm", entity: existing, phrase };
  }

  const result = await resolveOrAlreadyResolved(db, workspaceId, id, "approved");
  if (result.outcome !== "approved") return result;

  await recordEvent(db, workspaceId, `${result.entity.type}.approved`, id);
  await enqueueJob(db, workspaceId, "execute_approval", { entity_id: id, entity_type: result.entity.type });

  return result;
}

export async function denyEntity(db: WorkerDb, workspaceId: string, id: string): Promise<ApprovalResult> {
  const result = await resolveOrAlreadyResolved(db, workspaceId, id, "denied");
  if (result.outcome !== "denied") return result;

  // docs/SECURITY.md §2's exact naming for the deny transition.
  await recordEvent(db, workspaceId, `${result.entity.type}.rejected`, id);

  return result;
}
