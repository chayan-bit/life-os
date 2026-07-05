// BUILD stage (docs/SELF-EXTENSION-V2.md §4 step 3). Builds one DAG node in an
// isolated git worktree scoped to its tier.
//
// T0 nodes reuse the full existing scaffold flow (scaffold.js::scaffoldModule):
// worktree + structured manifest + all three T0 validators + commit, end to
// end. Rather than reimplement any of that, a T0 node delegates to that
// exported function and reports it already committed.
//
// T1-T5 nodes get an honest minimal build here: a fresh worktree, Layer B's
// per-tier PreToolUse hook, Layer C's Seatbelt sandbox scoped to the tier's
// writable dirs, and a per-tier prompt requiring a Zod structured summary
// { tier, files, summary }. Their real generators (and richer prompts/schemas)
// land per tier with #133+; validation (validate.js) still fails these closed
// until then, so nothing ships unvalidated.
import { z } from "zod";
import { scaffoldModule } from "../scaffold.js";
import { buildSandboxConfig } from "../lib/sandbox.js";
import { createPreToolUseHook } from "../lib/preToolUseHook.js";
import { scopeDirs } from "../lib/tierScopes.js";
import { createWorktree, removeWorktree } from "../lib/worktree.js";
import { slugify } from "../lib/slugify.js";

const ALLOWED_TOOLS = ["Read", "Glob", "Grep", "Edit", "Write", "Bash"];
const DISALLOWED_TOOLS = ["WebFetch", "WebSearch", "Bash(rm -rf *)", "Bash(git push *)", "Bash(curl *)"];

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

// Honest, minimal per-tier prompts. Each names the tier's exact write-scope so
// the agent cannot claim it needs anything broader; Layer B enforces it anyway.
export const TIER_PROMPTS = {
  T1: (node) => t1Prompt(node),
  T2: (node) => tierPrompt("Tier 2 agent capability (tool)", node),
  T3: (node) => tierPrompt("Tier 3 backend route or pipeline stage", node),
  T4: (node) => tierPrompt("Tier 4 additive migration or derived-index rebuild", node),
  T5: (node) => tierPrompt("Tier 5 subsystem (new crate)", node),
};

function tierPrompt(role, node) {
  const scope = scopeDirs(node.tier, node.params).join(", ");
  return [
    `You are building a ${role} for the Life OS self-extension ladder.`,
    `Task: ${node.description}`,
    `Write only within this tier's scope: ${scope}. Never touch anything else.`,
    "When done, your structured output must summarize what you wrote: the tier, the list of files you changed, and a one-line summary.",
  ].join("\n\n");
}

// T1 real generator prompt (issue #133) - a new Generic<Kind>.jsx renderer,
// following the exact props/data-fetch contract every sibling renderer under
// frontend/src/core/renderers/ already uses, plus its two registrations.
// The t1Render validator (server/validators/t1Render.js) checks both
// registrations exist, an a11y-focusable node mounts, and a visual baseline,
// so a build that skips either one fails closed downstream regardless of
// what this prompt asks for.
function t1Prompt(node) {
  const kind = node.params?.kind;
  const pascalKind = typeof kind === "string" && kind.length > 0 ? kind.charAt(0).toUpperCase() + kind.slice(1) : "";
  const scope = scopeDirs(node.tier, node.params).join(", ");
  return [
    "You are building a Tier 1 view renderer for the Life OS self-extension ladder.",
    `Task: ${node.description}`,
    `Write only within this tier's scope: ${scope}. Never touch anything else.`,
    `Create frontend/src/core/renderers/Generic${pascalKind}.jsx for the new view kind '${kind}'. ` +
      "Study the sibling Generic*.jsx components already in that directory (GenericList, GenericBoard, GenericMap, " +
      "GenericTimeline, etc.) and match their props contract exactly: `entities`, `setEntities`, `display` " +
      "(resolved via displayHelpers.js's resolveDisplay/resolveField), and `onSelect` for item activation. " +
      "Fetch any extra data it needs (e.g. graph edges from GET /api/edge) the same way sibling renderers call " +
      "apiCall from ../../lib/api. Use the repo's existing light, minimalist neo-* CSS classes/palette, add zero " +
      "new npm dependencies, and make every interactive element keyboard-reachable (tabIndex, Enter/Space activates " +
      "it, same handler as onClick) with an aria-label.",
    `Register the new kind in frontend/src/core/ModuleManifestPage.jsx's KIND_RENDERERS map ('${kind}': Generic${pascalKind}), ` +
      `and add '${kind}' to the RENDERER_KINDS array in frontend/src/core/rendererKinds.js (plain JS, no JSX/React imports).`,
    "When done, your structured output must summarize what you wrote: the tier, the list of files you changed, and a one-line summary.",
  ].join("\n\n");
}

// A stable, filesystem-safe worktree key per node (branch/dir name), so two
// nodes of one run never collide.
function nodeKey(ctx, node) {
  return `build-${slugify(ctx.runId)}-${slugify(node.id)}`;
}

// Consumes the SDK stream for a T1-T5 build: surfaces a hook denial directly,
// rejects a non-successful/interrupted result, and validates the tier summary.
async function runBuildAgent(ctx, prompt, options, hookState) {
  let resultMessage = null;
  for await (const message of ctx.queryFn({ prompt, options })) {
    if (message.type === "result") resultMessage = message;
  }
  if (hookState.denied) {
    throw new Error(`PreToolUse hook denied a write outside the tier scope: ${hookState.reason}`);
  }
  if (!resultMessage || resultMessage.subtype !== "success" || resultMessage.is_error) {
    throw new Error(`build agent did not complete successfully (subtype: ${resultMessage?.subtype ?? "none"})`);
  }
  const parsed = BuildNodeSummary.safeParse(resultMessage.structured_output);
  if (!parsed.success) {
    throw new Error(`build node summary failed validation: ${parsed.error.message}`);
  }
  return parsed.data;
}

// Wraps Layer B's hook so this module can observe a denial directly, exactly as
// scaffold.js does.
function trackedHook(scope) {
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

// T0: delegate the whole node to scaffoldModule (its own worktree + validators
// + commit). Returns an already-committed result the orchestrator records as-is.
async function buildT0Node(ctx, node) {
  const prompt = node.params.prompt ?? node.description;
  const result = await scaffoldModule(prompt, ctx.workspaceId, {
    repoRoot: ctx.repoRoot,
    queryFn: ctx.queryFn,
    ...(ctx.apiBase ? { apiBase: ctx.apiBase } : {}),
    ...(ctx.validateRenderSmoke ? { validateRenderSmoke: ctx.validateRenderSmoke } : {}),
    ...(ctx.persistManifestEntity ? { persistManifestEntity: ctx.persistManifestEntity } : {}),
    ...(ctx.model ? { model: ctx.model } : {}),
  });
  if (!result.success) throw new Error(result.error);
  return { alreadyCommitted: true, moduleId: result.moduleId, summary: result.manifest };
}

// T1-T5: fresh worktree + tier-scoped agent build. Leaves the worktree in place
// (uncommitted) for validate.js/gate.js/commit.js; the orchestrator removes it
// on every terminal path.
async function buildTierNode(ctx, node) {
  const { worktreePath, branch } = await createWorktree(ctx.repoRoot, nodeKey(ctx, node));
  try {
    const scope = { tier: node.tier, params: node.params, root: worktreePath };
    const { hook, state } = trackedHook(scope);
    const options = {
      cwd: worktreePath,
      allowedTools: ALLOWED_TOOLS,
      disallowedTools: DISALLOWED_TOOLS,
      permissionMode: "dontAsk",
      hooks: { PreToolUse: [{ matcher: "Write|Edit", hooks: [hook] }] },
      outputFormat: { type: "json_schema", schema: buildNodeSummaryJsonSchema },
      ...(ctx.model ? { model: ctx.model } : {}),
      ...buildSandboxConfig(scopeDirs(node.tier, node.params)),
    };
    const prompt = TIER_PROMPTS[node.tier](node);
    const summary = await runBuildAgent(ctx, prompt, options, state);
    return { alreadyCommitted: false, worktreePath, branch, summary };
  } catch (error) {
    await removeWorktree(ctx.repoRoot, worktreePath, branch).catch(() => {});
    throw error;
  }
}

// Builds one node by tier. T0 delegates to scaffold (fully committed on
// success); every other tier returns an uncommitted worktree for the downstream
// validate -> gate -> commit stages.
export async function buildNode(ctx, node) {
  if (node.tier === "T0") return buildT0Node(ctx, node);
  return buildTierNode(ctx, node);
}
