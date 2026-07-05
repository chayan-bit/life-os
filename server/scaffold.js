// Self-extension builder (issues #72/#73/#74/#75, docs/SELF-EXTENSION.md).
// Drives the Claude Agent SDK, tool-restricted by all three defense-in-depth
// layers (§2), in an isolated git worktree, requires a schema-valid
// structured-output manifest (§3), passes both validators (§4), and commits
// the result as the install (§5).
//
// `slugify()` still picks the module id *before* `query()` runs (Layer B's
// hook needs a concrete target directory up front), but the agent's own
// structured-output manifest.id is now asserted to match it - drift between
// the two fails the build rather than silently installing a mismatched
// manifest.
//
import fs from "node:fs/promises";
import path from "node:path";
import { query as defaultQuery } from "@anthropic-ai/claude-agent-sdk";
import { ModuleManifest, moduleManifestJsonSchema } from "./lib/moduleManifest.js";
import { buildSandboxConfig } from "./lib/sandbox.js";
import { createPreToolUseHook } from "./lib/preToolUseHook.js";
import { scopeDirs } from "./lib/tierScopes.js";
import { slugify } from "./lib/slugify.js";
import { commitAndMerge, createWorktree, removeWorktree } from "./lib/worktree.js";
import { loadManifestFromFile } from "./lib/loadManifest.js";
import { persistManifestEntity as defaultPersistManifestEntity } from "./lib/manifestEntity.js";
import { getValidators } from "./validators/registry.js";

// Tier 0 (manifest build). The self-extension ladder (docs/SELF-EXTENSION-V2.md)
// parameterizes scope + validators by tier; scaffold.js is the T0 caller.
const T0_TIER = "T0";
const T0_BASE_REF = "main"; // the scaffold branch is cut from main (worktree.js)

const DEFAULT_REPO_ROOT = path.resolve(import.meta.dirname, "..");
const DEFAULT_API_BASE = process.env.LIFEOS_API_URL || "http://127.0.0.1:8080";

// Layer A (docs/SELF-EXTENSION.md §2) - the primary gate. `dontAsk` denies
// anything not pre-approved instead of prompting, which is what makes this
// safe to run headless/unattended. Never `bypassPermissions` - it isn't
// constrained by `allowedTools` at all.
const ALLOWED_TOOLS = ["Read", "Glob", "Grep", "Edit", "Write", "Bash"];
const DISALLOWED_TOOLS = ["WebFetch", "WebSearch", "Bash(rm -rf *)", "Bash(git push *)", "Bash(curl *)"];

async function copyTemplate(repoRoot, worktreePath, moduleId) {
  const templateDir = path.join(repoRoot, "modules", "_template");
  const targetModuleDir = path.join(worktreePath, "modules", moduleId);
  await fs.cp(templateDir, targetModuleDir, { recursive: true });
  return targetModuleDir;
}

function buildPrompt(userPrompt, moduleId) {
  return [
    `Edit modules/${moduleId}/module.js (already seeded from the _template scaffold) so it satisfies this request:`,
    userPrompt,
    `Keep it a single osRegisterModule({...}) call, id: "${moduleId}". Only edit files under modules/${moduleId}/.`,
    `When done, your structured output must summarize the manifest you wrote: id, name, icon, color, entityTypes (with attrs), views, botCommands, and agentTools - matching the id "${moduleId}" exactly.`,
  ].join("\n\n");
}

// Consumes the SDK's async-generator result stream, watching for a denied
// tool call (tracked via the hook wrapper below, not by re-parsing SDK
// messages - the hook already knows the ground truth) and a terminal
// success/error `result` message carrying the schema-validated structured
// output (docs/SELF-EXTENSION.md §3). Returns the parsed ModuleManifest.
async function runAgent(queryFn, prompt, options, hookState, moduleId) {
  const stream = queryFn({ prompt, options });
  let resultMessage = null;

  for await (const message of stream) {
    if (message.type === "result") {
      resultMessage = message;
    }
  }

  if (hookState.denied) {
    throw new Error(`PreToolUse hook denied a write outside the module dir: ${hookState.reason}`);
  }
  if (!resultMessage || resultMessage.subtype !== "success" || resultMessage.is_error) {
    // Covers error_during_execution / error_max_turns / error_max_budget_usd
    // and the SDK's own structured-output retry exhaustion.
    throw new Error(`Agent SDK query did not complete successfully (subtype: ${resultMessage?.subtype ?? "none"})`);
  }
  // A `result` message can report subtype:"success" while still having
  // stopped short of a real answer (a hook/permission/sandbox boundary cut
  // the turn off, not a hard error) - `terminal_reason` distinguishes that
  // from an actual completed run and turns a generic "expected object,
  // received undefined" into a diagnosable cause.
  if (resultMessage.terminal_reason && resultMessage.terminal_reason !== "completed") {
    throw new Error(
      `Agent stopped before finishing (terminal_reason: ${resultMessage.terminal_reason})` +
        (resultMessage.deferred_tool_use ? `; deferred_tool_use: ${JSON.stringify(resultMessage.deferred_tool_use)}` : ""),
    );
  }

  const parsed = ModuleManifest.safeParse(resultMessage.structured_output);
  if (!parsed.success) {
    throw new Error(`Structured output failed ModuleManifest validation: ${parsed.error.message}`);
  }
  if (parsed.data.id !== moduleId) {
    throw new Error(`Structured output id "${parsed.data.id}" does not match target module id "${moduleId}"`);
  }

  return parsed.data;
}

export async function scaffoldModule(prompt, workspaceId, opts = {}) {
  const repoRoot = opts.repoRoot ?? DEFAULT_REPO_ROOT;
  const queryFn = opts.queryFn ?? defaultQuery;
  const persistManifestEntity = opts.persistManifestEntity ?? defaultPersistManifestEntity;
  const apiBase = opts.apiBase ?? DEFAULT_API_BASE;

  // Dispatch T0's gates through the validator registry (docs/SELF-EXTENSION-V2.md
  // §9) rather than importing them directly: protected-surface (§5) + structural
  // + render-smoke, in that order. Render-smoke stays overridable via opts.
  const t0Validators = new Map(getValidators(T0_TIER).map((v) => [v.name, v]));
  const validateRenderSmoke = opts.validateRenderSmoke ?? t0Validators.get("renderSmoke").run;

  const moduleId = slugify(prompt);
  const { worktreePath, branch } = await createWorktree(repoRoot, moduleId);

  try {
    const targetModuleDir = await copyTemplate(repoRoot, worktreePath, moduleId);

    // Wraps Layer B's hook so scaffold.js can observe a denial directly,
    // rather than inferring it from the SDK's message stream.
    const hookState = { denied: false, reason: null };
    // Layer B, tier-parameterized: T0's write-scope is modules/<id>/**, and the
    // never-generable surfaces (§5) are denied regardless. `root` is the
    // worktree so file paths resolve repo-relative for glob matching.
    const baseHook = createPreToolUseHook({ tier: T0_TIER, params: { moduleId }, root: worktreePath });
    const trackedHook = async (input) => {
      const result = await baseHook(input);
      if (result.hookSpecificOutput?.permissionDecision === "deny") {
        hookState.denied = true;
        hookState.reason = result.hookSpecificOutput.permissionDecisionReason;
      }
      return result;
    };

    const options = {
      cwd: worktreePath,
      allowedTools: ALLOWED_TOOLS,
      disallowedTools: DISALLOWED_TOOLS,
      permissionMode: "dontAsk",
      hooks: { PreToolUse: [{ matcher: "Write|Edit", hooks: [trackedHook] }] },
      outputFormat: { type: "json_schema", schema: moduleManifestJsonSchema },
      // Optional model override (e.g. a cheaper/faster model for a light
      // build) - omitted entirely when unset so the SDK/CLI's own default
      // applies, same opt-in-only shape as every other `opts.*` here.
      ...(opts.model ? { model: opts.model } : {}),
      ...buildSandboxConfig(scopeDirs(T0_TIER, { moduleId })),
    };

    const manifest = await runAgent(queryFn, buildPrompt(prompt, moduleId), options, hookState, moduleId);

    // Validator 0 (§5, §9) - the never-generable-surface gate: inspects the
    // worktree diff and hard-rejects if the agent touched any protected surface
    // (defense in depth behind Layer B, which already confines T0 writes).
    const protectedCheck = await t0Validators.get("protectedSurface").run({ worktreePath, baseRef: T0_BASE_REF });
    if (!protectedCheck.valid) {
      throw new Error(`Protected-surface validation failed: ${protectedCheck.errors.join("; ")}`);
    }

    // Validator 1 (§4, issue #74) - re-loads the file the agent actually
    // wrote (not the structured-output summary) and checks it against
    // module.schema.json, plus dup-type-id and dangling-view-ref checks
    // against the worktree's full modules/ tree (a worktree checkout already
    // contains every sibling module, so no separate lookup is needed).
    const structural = await t0Validators.get("structural").run(path.join(targetModuleDir, "module.js"), {
      modulesDir: path.join(worktreePath, "modules"),
    });
    if (!structural.valid) {
      throw new Error(`Structural validation failed: ${structural.errors.join("; ")}`);
    }

    const modulePath = path.join(targetModuleDir, "module.js");

    // Persists the real object-shaped manifest (module.js's own
    // osRegisterModule({...}) argument - the same shape the 14 static day-1
    // modules use, not the array-shaped structured-output summary above) as
    // a generic entity so the frontend can render the full multi-view
    // ModuleManifestPage for this hot-installed module instead of degrading
    // to a flat GenericList (issue #121, docs/SELF-EXTENSION-V2.md §6).
    // Best-effort only: a persistence failure must not fail the install -
    // the GenericList fallback still works without it.
    try {
      const fileManifest = await loadManifestFromFile(modulePath);
      await persistManifestEntity(apiBase, moduleId, fileManifest, { workspaceId });
    } catch (error) {
      console.warn(`[scaffold] failed to persist module_manifest entity for '${moduleId}': ${error.message}`);
    }

    // Validator 2 (§4, issue #75) - boots the real app stack (its own
    // default repoRoot, not the worktree/scratch `repoRoot` above: the
    // frontend build and lifeos-api binary only exist in the real checkout).
    // `modulePath` still points into this worktree (removed only after this
    // validator + commitAndMerge below both succeed), so render.js can load
    // the real manifest to seed the manifest entity and assert every
    // declared view mounts a node (issue #121).
    const render = await validateRenderSmoke(moduleId, manifest, { modulePath });
    if (!render.valid) {
      throw new Error(`Render smoke validation failed: ${render.errors.join("; ")}`);
    }

    await commitAndMerge(repoRoot, worktreePath, branch, moduleId);
    await removeWorktree(repoRoot, worktreePath, branch);

    return { success: true, moduleId, workspaceId, manifest };
  } catch (error) {
    await removeWorktree(repoRoot, worktreePath, branch).catch(() => {});
    return { success: false, moduleId, workspaceId, error: error.message };
  }
}

// CLI entry point (issue #78): `lifeos-drain` spawns this exact process
// (`node scaffold.js <prompt> <workspaceId>`) to build a bot-queued module
// request. The last stdout line is `scaffoldModule`'s return value verbatim,
// JSON-encoded, so the Rust side has a stable process contract to parse -
// no separate serialization logic needed on either side. Not exercised by
// the vitest suite (needs a real ANTHROPIC_API_KEY and mutates real git
// state), see docs/SELF-EXTENSION.md's "Implemented (issue #72)" note.
if (process.argv[1] === import.meta.filename) {
  const prompt = process.argv[2];
  const workspaceId = process.argv[3];
  if (!prompt || !workspaceId) {
    console.error("usage: node scaffold.js <prompt> <workspaceId>");
    process.exit(2);
  }
  const result = await scaffoldModule(prompt, workspaceId);
  console.log(JSON.stringify(result));
  process.exitCode = result.success ? 0 : 1;
}
