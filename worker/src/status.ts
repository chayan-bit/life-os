// Shared approval-status vocabulary (issue #142, docs/SECURITY.md §2) - kept
// in one small module so the Worker and services/lifeos-api/src/routes/
// approval.rs agree on exactly what "pending" and "resolved" mean for a
// gated entity.
//
// Two systems create pending gates, and they use two different statuses:
// every draft_action-backed route (including this Worker's own `/draft`) and
// storage-backend switches write `pending_approval`, while server/build/
// gate.js's T3+ build gates write `awaiting_approval`. Both must be treated
// as "awaiting a human" everywhere a gate is listed or resolved - the Worker
// previously only recognized `pending_approval`, so a real build gate never
// showed up in `/pending`, its approve/deny keyboard, the T5 typed-confirm
// reply, or the daily digest (it read as `already_resolved` instead).
export const PENDING_APPROVAL_STATUS = "pending_approval";
export const AWAITING_APPROVAL_STATUS = "awaiting_approval";
export const PENDING_STATUSES: readonly string[] = [PENDING_APPROVAL_STATUS, AWAITING_APPROVAL_STATUS];

export const APPROVED_STATUS = "approved";
// Matches services/lifeos-api/src/routes/approval.rs's deny transition. The
// Worker previously wrote "denied" here - a second terminal status for the
// same entity the API never recognized - so a gate the Worker denied stayed
// forever CAS-unresolvable from the API's point of view. Unified on
// "rejected" so both tiers' resolved rows read identically everywhere
// `events`/`entities` is read.
export const REJECTED_STATUS = "rejected";

export function isPendingApproval(status: string | null | undefined): status is string {
  return status != null && PENDING_STATUSES.includes(status);
}
