// Layer B (docs/SELF-EXTENSION.md §2, docs/SELF-EXTENSION-V2.md §3) - the
// code-level guarantee that holds even if Layer A (allowedTools/permissionMode)
// is ever misconfigured or bypassed. `allowedTools` can't express "Write only
// within tier T's scope", so this PreToolUse hook does: it denies any
// Write/Edit/MultiEdit whose `file_path` resolves outside the tier's allowlist
// OR touches a never-generable protected surface (§5). Isolated from
// scaffold.js so it's unit-testable without the Agent SDK.
import path from "node:path";
import { evaluateWrite } from "./tierScopes.js";

// Strict prefix match, `path.sep`-bounded, so a sibling directory that merely
// starts with the same characters (`modules/foo_bar` when the target is
// `modules/foo`) is never mistaken for "inside." Retained for the deprecated
// single-dir (string) scope form below.
export function isPathAllowed(targetModuleDir, filePath) {
  const resolvedTarget = path.resolve(targetModuleDir);
  const resolvedFile = path.resolve(filePath);
  return resolvedFile === resolvedTarget || resolvedFile.startsWith(resolvedTarget + path.sep);
}

function deny(reason) {
  return {
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: reason,
    },
  };
}

// `scope` is either:
//   - an object `{ tier, params, root }` (v2, per-tier glob allowlist), or
//   - a string `targetModuleDir` (DEPRECATED Tier-0 single-dir confinement,
//     kept so pre-existing callers/tests keep working unchanged).
export function createPreToolUseHook(scope) {
  if (typeof scope === "string") {
    return async (input) => {
      const filePath = input?.tool_input?.file_path;
      if (typeof filePath !== "string" || isPathAllowed(scope, filePath)) return {};
      return deny("writes confined to the new module dir");
    };
  }

  return async (input) => {
    const filePath = input?.tool_input?.file_path;
    if (typeof filePath !== "string") return {}; // e.g. a Bash tool call - no path to check
    const verdict = evaluateWrite(scope, filePath);
    return verdict.allowed ? {} : deny(verdict.reason);
  };
}
