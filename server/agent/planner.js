// Plan stage (docs/AGENT-CORE.md §3 step 3). A cheap deterministic heuristic
// decides whether a request is multi-step; if so, one structured-output model
// call emits an ordered plan, persisted as a pipeline-style DAG entity (reusing
// the lifeos-pipelines StageSpec shape, NOT a new table).
import { z } from "zod";
import { emptyUsage, foldUsage } from "./usage.js";

const MIN_MULTI_IMPERATIVES = 2;
const MIN_SOLO_IMPERATIVES = 3;
const IMPERATIVE_RE =
  /\b(find|create|make|tag|label|draft|write|update|edit|list|search|add|remove|delete|summari[sz]e|send|schedule|review|analy[sz]e|compile|generate)\b/gi;
const CONJUNCTION_RE = /\b(and|then|after|afterwards|also|next|plus)\b/gi;
const NUMBERED_STEP_RE = /(^|\n)\s*\d+[.)]\s+\S/;

// Plan shape mirrors lifeos-pipelines StageSpec (name/tool/description).
export const PlanSchema = z.object({
  stages: z
    .array(
      z.object({
        name: z.string(),
        tool: z.string().nullable(),
        description: z.string(),
      }),
    )
    .min(1),
});

// JSON schema for the SDK's structured-output (outputFormat) - kept in lockstep
// with PlanSchema above.
export const planJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["stages"],
  properties: {
    stages: {
      type: "array",
      minItems: 1,
      items: {
        type: "object",
        additionalProperties: false,
        required: ["name", "tool", "description"],
        properties: {
          name: { type: "string" },
          tool: { type: ["string", "null"] },
          description: { type: "string" },
        },
      },
    },
  },
};

const countMatches = (text, re) => (text.match(re) || []).length;

// Deterministic, no model call: numbered steps, or multiple imperatives joined
// by conjunctions/clauses, signal a multi-step request.
export function needsPlanning(prompt) {
  const text = String(prompt || "");
  if (NUMBERED_STEP_RE.test(text)) return true;
  const imperatives = countMatches(text, IMPERATIVE_RE);
  const conjunctions = countMatches(text, CONJUNCTION_RE);
  const clauses = text.split(/[;,.]/).filter((s) => s.trim()).length;
  if (imperatives >= MIN_SOLO_IMPERATIVES) return true;
  if (imperatives >= MIN_MULTI_IMPERATIVES && (conjunctions >= 1 || clauses >= 2)) return true;
  return false;
}

function buildPlanPrompt(goal, worldSnapshot) {
  return [
    "Break the user's request into a short ordered plan of stages.",
    "Each stage: a short name, the single tool it will call (or null for a reasoning-only step), and a one-line description.",
    worldSnapshot,
    `Request: ${goal}`,
  ].join("\n\n");
}

// Runs the structured-output planner call and returns a validated plan, or
// throws if the model's structured output is malformed.
export async function generatePlan(goal, worldSnapshot, ctx) {
  const options = {
    purpose: "plan",
    outputFormat: { type: "json_schema", schema: planJsonSchema },
    ...(ctx.model ? { model: ctx.model } : {}),
  };
  let structured = null;
  let usage = emptyUsage();
  for await (const message of ctx.queryFn({ prompt: buildPlanPrompt(goal, worldSnapshot), options })) {
    if (message.type === "result") {
      structured = message.structured_output;
      usage = foldUsage(usage, message.usage);
    }
  }
  const parsed = PlanSchema.safeParse(structured);
  if (!parsed.success) {
    throw new Error(`planner structured output invalid: ${parsed.error.message}`);
  }
  return { plan: parsed.data, tokens: usage.tokensIn + usage.tokensOut, ...usage };
}

// Persists the plan as a pipeline_run entity (reuses the pipeline DAG shape).
// Returns the created entity id (or null if persistence failed - best-effort).
export async function persistPlan(goal, plan, ctx) {
  const attrs = {
    pipeline_id: `agent:${ctx.runId}`,
    run_id: ctx.runId,
    input: goal,
    status: "running",
    origin: "agent",
    stages: plan.stages,
  };
  try {
    const res = await ctx.httpFn("POST", "/api/entity", {
      module: "pipelines",
      type: "pipeline_run",
      title: `agent run ${ctx.runId}`,
      status: "running",
      attrs,
      workspace_id: ctx.workspaceId,
    });
    return res?.ok ? res.data?.id ?? null : null;
  } catch {
    return null;
  }
}

// Updates a persisted plan entity's status at turn end (append-safe PATCH of
// the whole attrs blob - immutable copy, no in-place mutation).
export async function updatePlanStatus(entityId, plan, goal, status, ctx) {
  if (!entityId) return;
  const attrs = {
    pipeline_id: `agent:${ctx.runId}`,
    run_id: ctx.runId,
    input: goal,
    status,
    origin: "agent",
    stages: plan.stages,
  };
  try {
    await ctx.httpFn("PATCH", `/api/entity/${entityId}`, {
      status,
      attrs,
      workspace_id: ctx.workspaceId,
    });
  } catch {
    // Best-effort - a status write failure must not fail the turn.
  }
}
