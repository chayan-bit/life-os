// RESUME-ON-APPROVAL (issue #142, closes the #132/#135 gap). A T3+ build node
// halts at a gate (gate.js) as a `pipelines/pending_approval` entity, worktree
// discarded. When a human approves it, lifeos-drain shells
// `node build/run.js --resume <approvalEntityId> <workspaceId>`, which calls
// this. There is ONE resume entry point: re-load the persisted run's plan +
// node statuses, mark the approved node so it commits instead of re-gating, and
// re-enter the DAG - already-committed nodes are kept, the approved node and
// anything skipped-behind-it run now.
import { validatePlan } from "./plan.js";
import { buildCtx, executeDag, finalizeRun } from "./pipeline.js";

function safeParse(value) {
  if (typeof value !== "string") return value ?? {};
  try {
    return JSON.parse(value);
  } catch {
    return {};
  }
}

// GET one entity, normalizing attrs to an object (the real API returns attrs as
// a JSON value; a mocked httpFn may return a string). Returns null on miss.
async function fetchEntity(ctx, id) {
  const res = await ctx.httpFn("GET", `/api/entity/${id}`);
  if (!res?.ok || !res.data) return null;
  const entity = res.data;
  return { ...entity, attrs: safeParse(entity.attrs) };
}

function fail(runId, error) {
  return { success: false, runId, nodes: [], summary: error, error };
}

export async function resumeBuildPipeline(approvalEntityId, workspaceId, opts = {}) {
  const ctx = buildCtx(opts.runId ?? `resume_${Date.now()}`, workspaceId, opts);

  const approval = await fetchEntity(ctx, approvalEntityId);
  if (!approval) return fail(ctx.runId, `approval entity '${approvalEntityId}' not found`);
  if (approval.status !== "approved") {
    return fail(ctx.runId, `approval '${approvalEntityId}' is not approved (status=${approval.status})`);
  }

  const { run_id: runId, pipeline_run_entity_id: runEntityId, node: approvedNode } = approval.attrs;
  if (!runEntityId) return fail(ctx.runId, "approval entity has no pipeline_run_entity_id to resume");

  const run = await fetchEntity(ctx, runEntityId);
  if (!run) return fail(runId ?? ctx.runId, `pipeline run '${runEntityId}' not found`);

  const plan = run.attrs.plan;
  if (!Array.isArray(plan) || plan.length === 0) {
    return fail(runId ?? ctx.runId, "pipeline run has no persisted plan to resume");
  }

  // Deterministic re-validation of the persisted plan (re-topo; no model in the
  // loop) - the same guard a fresh run applies before touching a worktree.
  let ordered;
  try {
    ordered = validatePlan({ nodes: plan });
  } catch (error) {
    return fail(runId ?? ctx.runId, `persisted plan invalid: ${error.message}`);
  }

  ctx.runId = runId ?? ctx.runId;
  ctx.plan = ordered;
  ctx.approvedNodeIds = new Set(approvedNode ? [approvedNode] : []);

  const request = run.attrs.input;
  const priorNodes = Array.isArray(run.attrs.nodes) ? run.attrs.nodes : [];
  const seed = new Map(priorNodes.map((n) => [n.id, n]));

  const states = await executeDag(ctx, request, ordered, runEntityId, seed);
  return finalizeRun(ctx, request, ordered, runEntityId, states);
}
