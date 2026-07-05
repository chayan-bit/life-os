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
  T2: (node) => t2Prompt(node),
  T3: (node) => t3Prompt(node),
  T4: (node) => t4Prompt(node),
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

// T2 real generator prompt (issue #134) - a self-authored agent tool
// (Voyager-style), written as ONE pure request descriptor file. The t2Tool
// validator (server/validators/t2Tool.js) statically re-checks the import
// posture, the shape, and dry-runs `request()` against the shared
// isRouteAllowed allowlist, so a build that strays from this contract fails
// closed downstream regardless of what this prompt asks for.
function t2Prompt(node) {
  const name = node.params?.name;
  const scope = scopeDirs(node.tier, node.params).join(", ");
  return [
    "You are building a Tier 2 self-authored agent tool (Voyager-style) for the Life OS self-extension ladder.",
    `Task: ${node.description}`,
    `Write only within this tier's scope: ${scope}. Never touch anything else.`,
    `Create exactly one file, server/agent/tools/generated/${name}.js, exporting a default PURE REQUEST DESCRIPTOR - ` +
      "never a handler that performs I/O itself. The shape is:\n" +
      "  { name, description, classification: 'allowed'|'gated', inputSchema: <a zod object schema>, " +
      "example: <args satisfying inputSchema>, request: ({args, workspaceId}) => ({method, path, body?}) }",
    "The ONLY import allowed anywhere in the file is `zod` (`import { z } from \"zod\"`). No other import/require, " +
      "no `child_process`/`fs`/`net`/`http`/`https`/`fetch(`, no `process.env` reads, no dynamic `import(`. The " +
      "executor performs the actual HTTP call through its own chokepoint (capability check, ledger, retry) - your " +
      "`request` function only computes what to call, it never calls anything.",
    "`request`'s returned `{method, path}` MUST target one of: GET/POST /api/entity(/:id), GET/POST /api/edge, " +
      "GET /api/search, POST /api/memory/recall, POST /api/event, POST /api/browser/scrape. Anything else " +
      "(configs, module-request, jobs, llm, agent, whatsapp, storage, travel, notion, connections, anything with " +
      "'order'/'broker') is rejected and the tool will never be installed.",
    "`example` is REQUIRED - concrete args your `inputSchema` accepts, used both to prove the schema round-trips " +
      "(`inputSchema.safeParse(example)` must succeed) and to dry-run `request()` against the route allowlist.",
    "Concrete example - a read-computation tool over trade entities (R-multiple):\n" +
      "```js\n" +
      'import { z } from "zod";\n\n' +
      "const inputSchema = z.object({ tradeId: z.string() });\n\n" +
      "export default {\n" +
      '  name: "rMultiple",\n' +
      '  description: "Compute the R-multiple (reward/risk) for a closed trade entity.",\n' +
      '  classification: "allowed",\n' +
      "  inputSchema,\n" +
      '  example: { tradeId: "ent_trade_1" },\n' +
      "  request: ({ args }) => ({ method: \"GET\", path: `/api/entity/${args.tradeId}` }),\n" +
      "};\n" +
      "```\n" +
      "(the R-multiple itself is computed by the caller from the fetched entity's attrs - this descriptor only " +
      "reads; it never writes.)",
    `Write ${name}.js to solve: ${node.description}`,
    "When done, your structured output must summarize what you wrote: the tier, the list of files you changed, and a one-line summary.",
  ].join("\n\n");
}

// T3 real generator prompt (issue #135) - a new axum route + its additive
// mod.rs registration + its own integration test against a scratch DB. The
// t3Route validator (server/validators/t3Route.js) independently re-checks
// scope, mod.rs's additive-only diff, the scratch-DB pattern, and shells
// cargo build/test/clippy, so a build that strays from this contract fails
// closed downstream regardless of what this prompt asks for.
function t3Prompt(node) {
  const { crate, name } = node.params ?? {};
  const scope = scopeDirs(node.tier, node.params).join(", ");
  return [
    "You are building a Tier 3 backend route for the Life OS self-extension ladder.",
    `Task: ${node.description}`,
    `Write only within this tier's scope: ${scope}. Never touch anything else.`,
    `Create services/${crate}/src/routes/${name}.rs following the crate's existing route style (study the ` +
      "sibling files already under that directory): an async handler taking `State(state): State<AppState>`, " +
      "resolving the workspace the same way the crate's other routes do, and returning `ApiResult<Json<...>>`. " +
      "Read-only or draft-writing routes ONLY - never perform an outward effect (send/post/publish/place an " +
      "order) directly; if the route needs to act outward, it must write a `pending_approval` draft entity " +
      "instead, per docs/SECURITY.md §2's gating state machine, and let the existing approve flow perform the " +
      "real effect later.",
    `Register the new route ADDITIVELY in services/${crate}/src/routes/mod.rs under a ` +
      "'// --- generated (T3) ---' banner: add your new `mod ${name};` declaration and `.route(...)` line only - " +
      "never remove, reorder, or edit any existing line in that file (the t3Route validator diff-checks this " +
      "and rejects any destructive change to mod.rs).",
    `Write services/${crate}/tests/${name}_integration.rs as an HTTP-level integration test, following the ` +
      "EXACT Config-literal + scratch-DB pattern the crate's existing integration tests already use (a `Config` " +
      "struct literal pointing `db_path`/`derived_db_path` at a fresh path under `std::env::temp_dir()`) - " +
      `study a sibling test file under services/${crate}/tests/ before writing this one. NEVER point the test ` +
      "at the real `lifeos.db`, a `~/` path, or a hardcoded `/Users/...` path.",
    "When done, your structured output must summarize what you wrote: the tier, the list of files you changed, and a one-line summary.",
  ].join("\n\n");
}

// T4 real generator prompt (issue #136) - ONE additive migration file. The
// t4Migration validator (server/validators/t4Migration.js) independently
// re-checks file discipline, statement shape, and proves no-rewrite via a
// scratch apply, so a build that strays from this contract fails closed
// downstream regardless of what this prompt asks for.
function t4Prompt(node) {
  const { name } = node.params ?? {};
  const scope = scopeDirs(node.tier, node.params).join(", ");
  return [
    "You are building a Tier 4 additive migration for the Life OS self-extension ladder.",
    `Task: ${node.description}`,
    `Write only within this tier's scope: ${scope}. Never touch anything else.`,
    `Create exactly ONE new file, migrations/<NNNN>_${name}.sql, where <NNNN> is the next migration number ` +
      "(current highest existing migrations/<NNNN>_*.sql number, plus one, zero-padded to 4 digits - study the " +
      "migrations/ directory to find the current max before naming your file).",
    "Every statement in the file MUST be one of these additive shapes, and nothing else:\n" +
      "  - `ALTER TABLE <table> ADD COLUMN <col> <type> GENERATED ALWAYS AS (<expr>) VIRTUAL` (a computed, " +
      "expression-indexable column lifted from existing data, e.g. from a JSON attrs blob)\n" +
      "  - a plain nullable `ALTER TABLE <table> ADD COLUMN <col> <type>` (no `NOT NULL` without a `DEFAULT` - " +
      "SQLite rejects that combination, and this pipeline does)\n" +
      "  - `CREATE INDEX IF NOT EXISTS <idx> ON <table> (<cols-or-expr>)` (including an expression index)\n" +
      "  - `CREATE VIRTUAL TABLE IF NOT EXISTS <name> USING fts5(...)`",
    "FORBIDDEN, unconditionally: DROP, DELETE, UPDATE, TRUNCATE, RENAME, CREATE TRIGGER, and any non-virtual " +
      "CREATE TABLE. Any statement that rewrites or removes existing data or schema is rejected outright - when " +
      "in doubt, do not write it.",
    "Never target a table owned by a rebuild path, never a migration: `entity_vec`, `entity_vec_meta`, " +
      "`llm_cache` (server/memvec.py's semantic index) or `entities_idx`, `entities_fts`, `memory_idx`, " +
      "`memory_fts` (lifeos-api's lexical index, migrations/0003 and 0018) - those are rebuilt wholesale, not " +
      "migrated.",
    "Study migrations/0017_memory.sql and migrations/0018_derived_memory.sql for this repo's exact style " +
      "(header comment naming the issue/purpose, `IF NOT EXISTS` guards, GENERATED VIRTUAL column pattern), and " +
      "the `add_column_if_missing` idempotency note in services/lifeos-api/src/db.rs's module doc for why a plain " +
      "ADD COLUMN is NOT re-applied blindly at runtime (that guard lives in application code, not in your SQL file).",
    "Your structured output must summarize: the tier, the single file you wrote (its path), and a one-line summary.",
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
