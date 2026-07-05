// Shared Claude Agent SDK build-role primitives (docs/SELF-EXTENSION-V2.md §4).
// Every tier build drives the SDK the same way: a tier-scoped tool allowlist, a
// tracked Layer B PreToolUse hook so a scope escape is observable directly, and
// one stream-consumption contract that accumulates usage tokens, surfaces a hook
// denial, rejects a non-successful result, and validates the structured output.
//
// node.js (T1-T4, single call per node) and t5Subsystem.js (T5's
// supervisor/scaffolder/tester/reviewer split, many calls per node) both build
// on these, so the allowlist and the consumption rules are defined exactly once.
import { z } from "zod";
import { createPreToolUseHook } from "../lib/preToolUseHook.js";
import { emptyUsage, foldUsage } from "../agent/usage.js";
import { slugify } from "../lib/slugify.js";

// A tool-writing role (scaffolder, tester, T1-T4 build) gets the full set;
// a read-only role (supervisor, reviewer) gets only the inspection tools.
export const ALLOWED_TOOLS = ["Read", "Glob", "Grep", "Edit", "Write", "Bash"];
export const READ_ONLY_TOOLS = ["Read", "Glob", "Grep"];
export const DISALLOWED_TOOLS = ["WebFetch", "WebSearch", "Bash(rm -rf *)", "Bash(git push *)", "Bash(curl *)"];

export const BuildNodeSummary = z.object({
  tier: z.string(),
  files: z.array(z.string()),
  summary: z.string(),
});

export const buildNodeSummaryJsonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["tier", "files", "summary"],
  properties: {
    tier: { type: "string" },
    files: { type: "array", items: { type: "string" } },
    summary: { type: "string" },
  },
};

// A stable, filesystem-safe worktree key per node (branch/dir name), so two
// nodes of one run - or the several roles of one T5 node sharing it - never
// collide.
export function nodeKey(ctx, node) {
  return `build-${slugify(ctx.runId)}-${slugify(node.id)}`;
}

// Wraps Layer B's hook so a caller can observe a denial directly, exactly as
// scaffold.js does. Returns { hook, state }; `state.denied`/`state.reason` are
// set the moment a write outside the tier scope is refused.
export function trackedHook(scope) {
  const state = { denied: false, reason: null };
  const base = createPreToolUseHook(scope);
  const hook = async (input) => {
    const result = await base(input);
    if (result.hookSpecificOutput?.permissionDecision === "deny") {
      state.denied = true;
      state.reason = result.hookSpecificOutput.permissionDecisionReason;
    }
    return result;
  };
  return { hook, state };
}

// Consumes one SDK role stream. Accumulates usage across every result message,
// surfaces a hook denial as an error, rejects a non-successful/interrupted
// result, and (when `schema` is given) validates the structured output.
// Returns { data, tokens } - the combined in+out token count lets a caller
// enforce a spend budget across a multi-role sequence.
export async function consumeRole(queryFn, prompt, options, { hookState = null, schema = null } = {}) {
  let resultMessage = null;
  let usage = emptyUsage();
  for await (const message of queryFn({ prompt, options })) {
    if (message.type === "result") {
      resultMessage = message;
      usage = foldUsage(usage, message.usage);
    }
  }
  if (hookState?.denied) {
    throw new Error(`PreToolUse hook denied a write outside the tier scope: ${hookState.reason}`);
  }
  if (!resultMessage || resultMessage.subtype !== "success" || resultMessage.is_error) {
    throw new Error(`build agent did not complete successfully (subtype: ${resultMessage?.subtype ?? "none"})`);
  }
  const tokens = usage.tokensIn + usage.tokensOut;
  if (!schema) return { data: resultMessage.structured_output, tokens };
  const parsed = schema.safeParse(resultMessage.structured_output);
  if (!parsed.success) {
    throw new Error(`build role structured output failed validation: ${parsed.error.message}`);
  }
  return { data: parsed.data, tokens };
}
