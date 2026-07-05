// PLAN stage (docs/SELF-EXTENSION-V2.md §4 step 2). One structured-output call
// turns the build spec into a build DAG - an ordered set of tier-tagged nodes
// with explicit dependencies - which is then deterministically validated (no
// model in the loop) and persisted as an inspectable/resumable pipeline_run
// entity, exactly as the agent planner persists its plans (server/agent/
// planner.js::persistPlan).
import { z } from "zod";

// The tier ladder (docs/SELF-EXTENSION-V2.md §3). T3+ nodes are human-gated
// before commit (§8); the gate stage (gate.js) reads this set.
export const TIERS = ["T0", "T1", "T2", "T3", "T4", "T5"];
export const GATED_TIERS = new Set(["T3", "T4", "T5"]);

// Params each tier's scope function (lib/tierScopes.js TIER_SCOPES) needs to
// resolve a concrete write-scope. A plan node whose params omit a required key
// would silently widen its scope to a bogus glob (`modules/undefined/**`), so
// the plan is rejected before any worktree is created.
export const REQUIRED_PARAMS = {
  T0: ["moduleId"],
  T1: ["kind"],
  T2: ["name"],
  T3: ["crate", "name"],
  T4: ["name"],
  T5: ["crate"],
};

export const BuildPlan = z.object({
  nodes: z
    .array(
      z.object({
        id: z.string().min(1),
        tier: z.enum(TIERS),
        params: z.record(z.string(), z.any()),
        description: z.string(),
        dependsOn: z.array(z.string()),
      }),
    )
    .min(1),
});

export const buildPlanJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["nodes"],
  properties: {
    nodes: {
      type: "array",
      minItems: 1,
      items: {
        type: "object",
        additionalProperties: false,
        required: ["id", "tier", "params", "description", "dependsOn"],
        properties: {
          id: { type: "string" },
          tier: { type: "string", enum: TIERS },
          params: { type: "object", additionalProperties: true },
          description: { type: "string" },
          dependsOn: { type: "array", items: { type: "string" } },
        },
      },
    },
  },
};

function buildPlanPrompt(request, spec) {
  return [
    "Break this build request into a build DAG of tier-tagged nodes.",
    "Each node: a unique id, a tier (T0 manifest, T1 view, T2 tool, T3 route, T4 migration, T5 subsystem),",
    "a params object with the keys that tier needs (T0: moduleId; T1: kind; T2: name; T3: crate,name; T4: name; T5: crate),",
    "a one-line description, and dependsOn (ids of nodes that must land first).",
    `Request: ${request}`,
    `Spec: ${JSON.stringify(spec)}`,
  ].join("\n\n");
}

// Asserts every node's params carry the keys its tier's scope function needs.
function assertParams(node) {
  for (const key of REQUIRED_PARAMS[node.tier] ?? []) {
    const value = node.params?.[key];
    if (typeof value !== "string" || value.length === 0) {
      throw new Error(`node '${node.id}' (${node.tier}) is missing required param '${key}'`);
    }
  }
}

// Kahn's algorithm: returns the nodes in a valid execution order, or throws if
// a dependency references an unknown node or the graph has a cycle (fail closed
// - an un-orderable DAG must never start building).
export function topoOrder(nodes) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  if (byId.size !== nodes.length) throw new Error("build plan has duplicate node ids");

  const indegree = new Map(nodes.map((n) => [n.id, 0]));
  for (const node of nodes) {
    for (const dep of node.dependsOn) {
      if (!byId.has(dep)) throw new Error(`node '${node.id}' depends on unknown node '${dep}'`);
      indegree.set(node.id, indegree.get(node.id) + 1);
    }
  }

  const ready = nodes.filter((n) => indegree.get(n.id) === 0);
  const ordered = [];
  while (ready.length > 0) {
    const node = ready.shift();
    ordered.push(node);
    for (const other of nodes) {
      if (!other.dependsOn.includes(node.id)) continue;
      indegree.set(other.id, indegree.get(other.id) - 1);
      if (indegree.get(other.id) === 0) ready.push(other);
    }
  }
  if (ordered.length !== nodes.length) throw new Error("build plan DAG has a cycle");
  return ordered;
}

// Deterministic post-validation of a model-produced plan: params present per
// tier, then a full topological ordering (which also rejects unknown deps and
// cycles). Returns the execution-ordered node list.
export function validatePlan(plan) {
  for (const node of plan.nodes) assertParams(node);
  return topoOrder(plan.nodes);
}

export async function generateBuildPlan(request, spec, ctx) {
  const options = {
    purpose: "build_plan",
    outputFormat: { type: "json_schema", schema: buildPlanJsonSchema },
    ...(ctx.model ? { model: ctx.model } : {}),
  };
  let structured = null;
  for await (const message of ctx.queryFn({ prompt: buildPlanPrompt(request, spec), options })) {
    if (message.type === "result") structured = message.structured_output;
  }
  const parsed = BuildPlan.safeParse(structured);
  if (!parsed.success) {
    throw new Error(`build plan structured output invalid: ${parsed.error.message}`);
  }
  return parsed.data;
}

// The attrs blob shared by the create and every subsequent status PATCH - one
// place so the persisted run row always has the same shape (immutable: callers
// pass a fresh nodes snapshot, never mutate this).
function runAttrs(ctx, request, status, nodes) {
  return {
    pipeline_id: `build:${ctx.runId}`,
    run_id: ctx.runId,
    input: request,
    status,
    origin: "build",
    nodes,
  };
}

// Persists the DAG as a pipelines/pipeline_run entity (same route + shape the
// agent planner uses). Returns the created entity id, or null on failure
// (best-effort: the build proceeds even if the inspector row could not be
// written, exactly as persistPlan degrades).
export async function persistBuildRun(ctx, request, nodes) {
  try {
    const res = await ctx.httpFn("POST", "/api/entity", {
      module: "pipelines",
      type: "pipeline_run",
      title: `build run ${ctx.runId}`,
      status: "running",
      attrs: runAttrs(ctx, request, "running", nodes),
      workspace_id: ctx.workspaceId,
    });
    return res?.ok ? res.data?.id ?? null : null;
  } catch {
    return null;
  }
}

// PATCHes the persisted run's node statuses + overall status as the build
// progresses (whole-attrs write, immutable copy). Best-effort - a trace write
// must never fail the build.
export async function patchBuildRun(ctx, entityId, request, status, nodes) {
  if (!entityId) return;
  try {
    await ctx.httpFn("PATCH", `/api/entity/${entityId}`, {
      status,
      attrs: runAttrs(ctx, request, status, nodes),
      workspace_id: ctx.workspaceId,
    });
  } catch {
    // Best-effort trace only.
  }
}
