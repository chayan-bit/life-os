// Build pipeline orchestrator (docs/SELF-EXTENSION-V2.md §4). Sequences the
// spec -> plan -> build DAG -> validate -> gate -> commit stages, topo-ordered
// and sequential (parallel worktrees are future work). Every stage writes an
// append-only `events` row (commit.js::emitBuildEvent) and updates the
// persisted pipeline_run so the whole build is inspectable and replayable.
//
// Partial success is surfaced honestly: a failed or gated node aborts its
// subtree (transitive dependents become `skipped`), but already-committed nodes
// STAY (forward-only, each a revertable commit). The result never reports a
// failed/skipped/gated node as installed.
//
// Resume-on-approval (issue #142): a T3+ node halts at a gate (gate.js) and its
// worktree is discarded. The persisted run carries the full `plan` (see
// plan.js::runAttrs), so `resume.js` can re-enter the DAG at the approved node
// - `executeDag` accepts a seed of prior node states and keeps every already
// `completed` node instead of rebuilding it, and `ctx.approvedNodeIds` tells
// `finishTierNode` to COMMIT the approved node rather than re-gate it.
import { query as defaultQuery } from "@anthropic-ai/claude-agent-sdk";
import { createHttpFn } from "../agent/http.js";
import { generateSpec } from "./spec.js";
import { generateBuildPlan, validatePlan, persistBuildRun, patchBuildRun, GATED_TIERS } from "./plan.js";
import { buildNode } from "./node.js";
import { validateNode } from "./validate.js";
import { gateNode } from "./gate.js";
import { commitNode, emitBuildEvent, headHash } from "./commit.js";
import { removeWorktree } from "../lib/worktree.js";

const newRunId = () => `build_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

// Assembles the immutable per-run context. Shared by a fresh run and a resume
// so both flow through the identical build/validate/gate/commit machinery; the
// only resume-specific fields are `approvedNodeIds` (nodes to commit instead of
// gate) and `plan` (the ordered node list, persisted for a later resume).
export function buildCtx(runId, workspaceId, opts = {}) {
  return {
    runId,
    workspaceId,
    repoRoot: opts.repoRoot,
    apiBase: opts.apiBase,
    queryFn: opts.queryFn ?? defaultQuery,
    httpFn: opts.httpFn ?? createHttpFn(workspaceId, opts.apiBase),
    validateFn: opts.validateFn,
    validateRenderSmoke: opts.validateRenderSmoke,
    persistManifestEntity: opts.persistManifestEntity,
    model: opts.model,
    // T3's real validator (server/validators/t3Route.js) shells cargo/git;
    // this is its DI seam so vitest never runs real cargo.
    execFn: opts.execFn,
    // Per-node token ceiling for the T5 supervisor+subagent split
    // (server/build/t5Subsystem.js); defaults to T5_TOKEN_BUDGET when absent.
    budget: opts.budget,
    // Nodes a human has approved: finishTierNode commits these instead of
    // re-gating them (resume path). Empty on a fresh run.
    approvedNodeIds: opts.approvedNodeIds ?? new Set(),
    // The ordered DAG, stamped into the persisted run so a resume can rebuild
    // it (plan.js::runAttrs reads this). Set once the plan is validated.
    plan: opts.plan ?? [],
  };
}

// Immutable per-node record. `commit` and `reason` are present only when they
// apply, so a JSON snapshot omits them otherwise.
export function nodeRecord(node, status, extra = {}) {
  return { id: node.id, tier: node.tier, status, ...extra };
}

// True iff every dependency of `node` reached `completed` (a failed, gated, or
// skipped dependency blocks the whole subtree).
function depsSatisfied(node, states) {
  return node.dependsOn.every((dep) => states.get(dep)?.status === "completed");
}

// Runs one already-built, non-T0 node through validate -> gate -> commit,
// discarding its worktree on every terminal path. Returns the node's record.
// An approved node (ctx.approvedNodeIds) bypasses the gate and commits.
async function finishTierNode(ctx, node, built, runEntityId) {
  const verdict = await validateNode(ctx, node, built);
  if (!verdict.valid) {
    await removeWorktree(ctx.repoRoot, built.worktreePath, built.branch).catch(() => {});
    await emitBuildEvent(ctx, "build.node.failed", { tier: node.tier, node: node.id, outcome: "failed", errors: verdict.errors });
    return nodeRecord(node, "failed", { reason: verdict.errors.join("; ") });
  }
  if (GATED_TIERS.has(node.tier) && !ctx.approvedNodeIds.has(node.id)) {
    await gateNode(ctx, node, runEntityId, built.summary);
    await removeWorktree(ctx.repoRoot, built.worktreePath, built.branch).catch(() => {});
    return nodeRecord(node, "awaiting_approval");
  }
  const commit = await commitNode(ctx, node, built);
  await removeWorktree(ctx.repoRoot, built.worktreePath, built.branch).catch(() => {});
  await emitBuildEvent(ctx, "build.node.completed", { tier: node.tier, node: node.id, outcome: "completed", commit });
  return nodeRecord(node, "completed", { commit });
}

// Builds + finalizes one node. T0 comes back already committed; every other
// tier flows through validate/gate/commit.
async function runNode(ctx, node, runEntityId) {
  const built = await buildNode(ctx, node);
  if (built.alreadyCommitted) {
    const commit = await headHash(ctx.repoRoot);
    await emitBuildEvent(ctx, "build.node.completed", { tier: node.tier, node: node.id, outcome: "completed", commit });
    return nodeRecord(node, "completed", { commit });
  }
  return finishTierNode(ctx, node, built, runEntityId);
}

// Processes the DAG in topological order. A node already `completed` in `seed`
// (a resume of an earlier run) is kept as-is, never rebuilt; a node whose deps
// did not all complete is skipped (subtree abort); a build/validate throw fails
// that node.
export async function executeDag(ctx, request, ordered, runEntityId, seed = new Map()) {
  const states = new Map();
  for (const node of ordered) {
    const prior = seed.get(node.id);
    if (prior && prior.status === "completed") {
      states.set(node.id, prior);
      continue;
    }
    if (!depsSatisfied(node, states)) {
      states.set(node.id, nodeRecord(node, "skipped", { reason: "dependency failed" }));
      continue;
    }
    states.set(node.id, nodeRecord(node, "building"));
    await patchBuildRun(ctx, runEntityId, request, "running", [...states.values()]);
    try {
      states.set(node.id, await runNode(ctx, node, runEntityId));
    } catch (error) {
      await emitBuildEvent(ctx, "build.node.failed", { tier: node.tier, node: node.id, outcome: "failed", error: error.message });
      states.set(node.id, nodeRecord(node, "failed", { reason: error.message }));
    }
  }
  return states;
}

// Persists the final node statuses, emits build.completed, and returns the
// pipeline result. Shared by a fresh run and a resume.
export async function finalizeRun(ctx, request, ordered, runEntityId, states) {
  const nodes = ordered.map((n) => states.get(n.id));
  const success = nodes.every((n) => n.status === "completed");
  const status = success ? "completed" : "partial";
  await patchBuildRun(ctx, runEntityId, request, status, nodes);
  await emitBuildEvent(ctx, "build.completed", { outcome: status, nodes });

  const committed = nodes.filter((n) => n.status === "completed").length;
  const summary = `${committed}/${nodes.length} nodes committed`;
  return { success, runId: ctx.runId, nodes, summary };
}

// runBuildPipeline(request, workspaceId, opts) - the pipeline entry point.
// DI surface mirrors the agent loop: queryFn, httpFn, validateFn overrides,
// plus repoRoot / validateRenderSmoke (forwarded to the T0 scaffold flow).
export async function runBuildPipeline(request, workspaceId, opts = {}) {
  const runId = opts.runId ?? newRunId();
  const ctx = buildCtx(runId, workspaceId, opts);

  // SPEC + PLAN, then deterministic DAG validation. Any failure here (bad
  // structured output, missing params, a cycle) fails closed BEFORE any
  // worktree is created - nothing is built or committed.
  let ordered;
  try {
    const spec = await generateSpec(request, ctx);
    const plan = await generateBuildPlan(request, spec, ctx);
    ordered = validatePlan(plan);
  } catch (error) {
    return { success: false, runId, nodes: [], summary: `plan rejected: ${error.message}`, error: error.message };
  }
  ctx.plan = ordered;

  const runEntityId = await persistBuildRun(ctx, request, ordered.map((n) => nodeRecord(n, "pending")));
  const states = await executeDag(ctx, request, ordered, runEntityId);
  return finalizeRun(ctx, request, ordered, runEntityId, states);
}
