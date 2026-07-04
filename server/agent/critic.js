// Verify stage (docs/AGENT-CORE.md §3 step 5). One structured-output model call
// judges the execute result against the goal. A single real, fixable issue
// triggers EXACTLY ONE refine round - bounded, never an open loop.
import { z } from "zod";

export const CritiqueSchema = z.object({
  ok: z.boolean(),
  issue: z.string().nullable(),
  fixable: z.boolean(),
});

export const critiqueJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["ok", "issue", "fixable"],
  properties: {
    ok: { type: "boolean" },
    issue: { type: ["string", "null"] },
    fixable: { type: "boolean" },
  },
};

function usageTokens(usage) {
  if (!usage) return 0;
  return (Number(usage.input_tokens) || 0) + (Number(usage.output_tokens) || 0);
}

function buildCritiquePrompt(goal, resultText) {
  return [
    "Judge whether the result below satisfies the goal. Be strict but fair.",
    "Return ok=true if it does. If not, give a single concrete issue and whether it is fixable in one more pass.",
    `Goal: ${goal}`,
    `Result: ${resultText || "(no textual result)"}`,
  ].join("\n\n");
}

// Returns { critique, tokens }. On a malformed/absent structured output it
// degrades to a passing verdict rather than blocking the turn.
export async function critique(goal, resultText, ctx) {
  const options = {
    purpose: "verify",
    outputFormat: { type: "json_schema", schema: critiqueJsonSchema },
    ...(ctx.model ? { model: ctx.model } : {}),
  };
  let structured = null;
  let tokens = 0;
  for await (const message of ctx.queryFn({ prompt: buildCritiquePrompt(goal, resultText), options })) {
    if (message.type === "result") {
      structured = message.structured_output;
      tokens += usageTokens(message.usage);
    }
  }
  const parsed = CritiqueSchema.safeParse(structured);
  if (!parsed.success) {
    return { critique: { ok: true, issue: null, fixable: false }, tokens };
  }
  return { critique: parsed.data, tokens };
}
