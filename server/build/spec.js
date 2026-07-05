// SPEC stage (docs/SELF-EXTENSION-V2.md §4 step 1). One structured-output
// model call restates a free-text build request as a structured build spec:
// what entities / views / tools / routes / migrations it implies. The plan
// stage (plan.js) consumes this spec to decide which tiers the build needs.
//
// Each field is an array of short descriptors; empty arrays are fine (a pure
// T0 module request implies no routes or migrations, for example).
import { z } from "zod";

export const BuildSpec = z.object({
  summary: z.string(),
  entities: z.array(z.string()),
  views: z.array(z.string()),
  tools: z.array(z.string()),
  routes: z.array(z.string()),
  migrations: z.array(z.string()),
});

// JSON schema for the SDK's structured-output (outputFormat), kept in lockstep
// with BuildSpec above.
export const buildSpecJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["summary", "entities", "views", "tools", "routes", "migrations"],
  properties: {
    summary: { type: "string" },
    entities: { type: "array", items: { type: "string" } },
    views: { type: "array", items: { type: "string" } },
    tools: { type: "array", items: { type: "string" } },
    routes: { type: "array", items: { type: "string" } },
    migrations: { type: "array", items: { type: "string" } },
  },
};

function buildSpecPrompt(request) {
  return [
    "Restate the following build request as a structured build spec.",
    "List the entities, views, tools, backend routes, and DB migrations it implies.",
    "Use short descriptors; leave an array empty when the request implies nothing for it.",
    `Request: ${request}`,
  ].join("\n\n");
}

// Runs the structured spec call and returns a validated BuildSpec, or throws if
// the model's structured output is malformed (fail closed - a build must never
// proceed on an un-parseable spec).
export async function generateSpec(request, ctx) {
  const options = {
    purpose: "build_spec",
    outputFormat: { type: "json_schema", schema: buildSpecJsonSchema },
    ...(ctx.model ? { model: ctx.model } : {}),
  };
  let structured = null;
  for await (const message of ctx.queryFn({ prompt: buildSpecPrompt(request), options })) {
    if (message.type === "result") structured = message.structured_output;
  }
  const parsed = BuildSpec.safeParse(structured);
  if (!parsed.success) {
    throw new Error(`build spec structured output invalid: ${parsed.error.message}`);
  }
  return parsed.data;
}
