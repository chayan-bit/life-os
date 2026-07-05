// VALIDATE stage (docs/SELF-EXTENSION-V2.md §4 step 4, §9). Runs the tier's
// validators (server/validators/registry.js::getValidators) against a built
// node's worktree. A failure discards that worktree and aborts the node's
// subtree (handled by the orchestrator); this module only decides pass/fail.
//
// T0 nodes never reach here - they are built and validated end-to-end inside
// scaffoldModule (node.js). This default therefore handles the generic-shaped
// validators every non-T0 tier carries today: the protected-surface gate
// (§5, object arg) plus the fail-closed placeholder that stands in until each
// tier's real validators land (#133+). An injected ctx.validateFn overrides
// this entirely (tests, and future per-tier validator wiring).
import { getValidators } from "../validators/registry.js";

const BASE_REF = "main";
const PROTECTED_SURFACE = "protectedSurface";

// Runs each validator for `tier` against the worktree, short-circuiting on the
// first failure (fail closed). protectedSurface needs the worktree + base ref;
// the placeholder (and any future no-arg validator) is called bare.
async function runRegistryValidators(tier, worktreePath) {
  const validators = getValidators(tier);
  for (const validator of validators) {
    const result =
      validator.name === PROTECTED_SURFACE
        ? await validator.run({ worktreePath, baseRef: BASE_REF })
        : await validator.run();
    if (!result.valid) return result;
  }
  return { valid: true, errors: [] };
}

// Validates one built node. Uses the injected ctx.validateFn when present
// (signature: (tier, worktreePath, node, ctx) -> { valid, errors }), else the
// registry default above.
export async function validateNode(ctx, node, { worktreePath }) {
  if (typeof ctx.validateFn === "function") {
    return ctx.validateFn(node.tier, worktreePath, node, ctx);
  }
  return runRegistryValidators(node.tier, worktreePath);
}
