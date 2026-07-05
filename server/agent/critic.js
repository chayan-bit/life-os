// Verify stage (docs/AGENT-CORE.md §3 step 5). One structured-output model call
// judges the execute result against the goal. A single real, fixable issue
// triggers EXACTLY ONE refine round - bounded, never an open loop.
import { z } from "zod";
import { emptyUsage, foldUsage } from "./usage.js";

// `confidence` defaults to 1 (never abstain) when a caller's structured
// output omits it - a pre-existing scripted verdict (agent.test.js) without
// the field must still parse, not fall back to an always-passing default.
export const CritiqueSchema = z.object({
  ok: z.boolean(),
  issue: z.string().nullable(),
  fixable: z.boolean(),
  confidence: z.number().min(0).max(1).default(1),
});

export const critiqueJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["ok", "issue", "fixable", "confidence"],
  properties: {
    ok: { type: "boolean" },
    issue: { type: ["string", "null"] },
    fixable: { type: "boolean" },
    confidence: { type: "number", minimum: 0, maximum: 1 },
  },
};

function buildCritiquePrompt(goal, resultText) {
  return [
    "Judge whether the result below satisfies the goal. Be strict but fair.",
    "Return ok=true if it does. If not, give a single concrete issue and whether it is fixable in one more pass.",
    "Also return a calibrated confidence (0-1) that the result is correct and complete - low confidence on a",
    "genuinely uncertain answer, not just politeness.",
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
  let usage = emptyUsage();
  for await (const message of ctx.queryFn({ prompt: buildCritiquePrompt(goal, resultText), options })) {
    if (message.type === "result") {
      structured = message.structured_output;
      usage = foldUsage(usage, message.usage);
    }
  }
  const tokens = usage.tokensIn + usage.tokensOut;
  const parsed = CritiqueSchema.safeParse(structured);
  if (!parsed.success) {
    return { critique: { ok: true, issue: null, fixable: false, confidence: 1 }, tokens, ...usage };
  }
  return { critique: parsed.data, tokens, ...usage };
}
