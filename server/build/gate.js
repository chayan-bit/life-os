// GATE stage (docs/SELF-EXTENSION-V2.md §4 step 5, §8). A T3+ node that passed
// validation is NOT committed autonomously: it creates a pending_approval
// entity linked to the build run, emits a gate event, and halts. The node stays
// `awaiting_approval`; its worktree is discarded (resume-on-approval is out of
// scope - see the honesty note below).
//
// HONESTY NOTE: the sampled Haiku eval-judge (HARNESS-LOOP.md §2) lives only
// inside the Rust pipeline runner (services/lifeos-pipelines) with no callable
// surface from this JS pipeline - the same "wiring only" boundary #125 hit. So
// this gate is APPROVAL-only today: it records the human-approval requirement
// and halts; the eval-judge score is not consulted here. Resuming a build from
// an approved gate (re-building the node's worktree and committing it) is
// deferred to a follow-up.
import { emitBuildEvent } from "./commit.js";

// Creates the pending_approval entity a human approves before this node ships.
// Best-effort id return (like persistBuildRun) - the node still halts even if
// the row could not be written, because the absence of a commit is itself the
// safe default.
async function createPendingApproval(ctx, node, runEntityId, summary) {
  try {
    const res = await ctx.httpFn("POST", "/api/entity", {
      module: "pipelines",
      type: "pending_approval",
      title: `pending_approval ${ctx.runId}:${node.id}`,
      status: "awaiting_approval",
      attrs: {
        run_id: ctx.runId,
        pipeline_run_entity_id: runEntityId,
        node: node.id,
        tier: node.tier,
        // T5 is the highest-blast-radius tier (a whole crate), so its approval
        // demands an EXPLICIT typed confirmation, not a one-tap approve. The
        // downstream approval UI/bot reads this flag (its enforcement is out of
        // scope here - the flag is the contract). Issue #137, §4/§8.
        requires_typed_confirm: node.tier === "T5",
        summary,
      },
      workspace_id: ctx.workspaceId,
    });
    return res?.ok ? res.data?.id ?? null : null;
  } catch {
    return null;
  }
}

// Gates a T3+ node: records the pending approval, emits `build.node.gated`, and
// returns the awaiting-approval outcome (the node is never committed here).
export async function gateNode(ctx, node, runEntityId, summary) {
  const approvalId = await createPendingApproval(ctx, node, runEntityId, summary);
  await emitBuildEvent(ctx, "build.node.gated", {
    tier: node.tier,
    node: node.id,
    outcome: "awaiting_approval",
    approval_id: approvalId,
  });
  return { status: "awaiting_approval", approvalId };
}
