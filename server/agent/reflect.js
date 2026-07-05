// Lesson distillation - the "distill after" half of the "distill after,
// recall before" cycle (docs/AGENT-CORE.md §6, issue #128). Rides the
// EXISTING procedural-memory pipeline instead of a new subsystem: emits a
// `feedback.given` event, which is exactly the input
// `HeuristicPolicyLearner` (services/lifeos-memory/src/procedural.rs)
// already turns into a `memory.rule.added` event at the next sleep cycle -
// which `rules_for_prompt` then injects into every `/api/memory/context`
// response (issue #124 already pipes that into agent turns). There is no
// recall-side code to build here: recall is the existing memory-context
// passthrough.
import { z } from "zod";

const LESSON_BLOCK_MAX_CHARS = 500;

// Conservative heuristic for "this turn probably taught us something durable"
// - a correction or an explicit preference/instruction. When in doubt, skip:
// a missed lesson costs nothing, a wrong one pollutes procedural memory.
const CORRECTIVE_RE =
  /\b(always|never|don't|do not|instead|wrong|incorrect|mistake|keep\s+\S+\s+under|next time|from now on|please stop|prefer(?:s|red)?)\b/i;

export function looksCorrective(prompt) {
  return CORRECTIVE_RE.test(String(prompt || ""));
}

export const LessonSchema = z.object({
  rule: z.string().nullable(),
  confidence: z.number(),
  kind: z.enum(["lesson", "skill"]),
});

// JSON schema for the SDK's structured-output (outputFormat) - kept in
// lockstep with LessonSchema above.
export const lessonJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["rule", "confidence", "kind"],
  properties: {
    rule: { type: ["string", "null"] },
    confidence: { type: "number" },
    kind: { type: "string", enum: ["lesson", "skill"] },
  },
};

function buildLessonPrompt(goal, resultText) {
  return [
    "A turn just completed. Decide whether it taught a durable, reusable lesson",
    "or skill about how to behave going forward (e.g. a correction or a stated",
    "preference) - NOT a one-off fact about this specific turn's content.",
    "Return rule=null if nothing durable should be remembered.",
    `Goal: ${goal}`,
    `Result: ${String(resultText || "(no textual result)").slice(0, LESSON_BLOCK_MAX_CHARS)}`,
  ].join("\n\n");
}

// distillLesson(queryFn, prompt, outcome, resultText, opts) - runs ONLY after
// a substantive (completed) turn whose prompt reads as corrective. One cheap
// structured-output call decides whether there is a durable lesson; if so,
// emits exactly ONE `feedback.given` event carrying `attrs.feedback` (the
// exact field `HeuristicPolicyLearner` reads) and `attrs.confidence`, which
// `HeuristicPolicyLearner` now reads too (clamped to [0.3, 0.95]) instead of
// discarding it in favor of a fixed default. Best-effort: never throws,
// never affects the turn result already computed by the caller.
export async function distillLesson(queryFn, prompt, outcome, resultText, opts = {}) {
  if (outcome !== "completed") return;
  if (!looksCorrective(prompt)) return;

  const { httpFn, workspaceId, model } = opts;
  try {
    const options = {
      purpose: "reflect",
      outputFormat: { type: "json_schema", schema: lessonJsonSchema },
      ...(model ? { model } : {}),
    };
    let structured = null;
    for await (const message of queryFn({ prompt: buildLessonPrompt(prompt, resultText), options })) {
      if (message.type === "result") structured = message.structured_output;
    }
    const parsed = LessonSchema.safeParse(structured);
    if (!parsed.success || !parsed.data.rule) return;

    const { rule, confidence, kind } = parsed.data;
    await httpFn("POST", "/api/event", {
      type: "feedback.given",
      actor: "agent",
      attrs: {
        // The exact field HeuristicPolicyLearner.derive_rule_deltas reads
        // (services/lifeos-memory/src/procedural.rs) - NOT `rule`, despite
        // the confusingly similar name of the LessonSchema field above.
        feedback: `${kind}: ${rule}`,
        confidence,
        source: "agent-reflect",
      },
      workspace_id: workspaceId,
    });
  } catch {
    // Best-effort: a distillation failure must never affect an
    // already-returned turn result.
  }
}
