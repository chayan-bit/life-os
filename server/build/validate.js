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

// Each named validator gets exactly the args shape its `run` function
// expects; a validator with no entry here (the fail-closed placeholders) is
// called bare. Adding a tier-specific validator (e.g. t1Render, #133) only
// needs one new line here, not a growing if/else chain.
const VALIDATOR_ARGS = {
  protectedSurface: (worktreePath, node) => ({ worktreePath, baseRef: BASE_REF }),
  t1Render: (worktreePath, node) => ({ worktreePath, params: node.params }),
};

// Runs each validator for `tier` against the worktree, short-circuiting on the
// first failure (fail closed).
async function runRegistryValidators(tier, worktreePath, node) {
  const validators = getValidators(tier);
  for (const validator of validators) {
    const argsFn = VALIDATOR_ARGS[validator.name];
    const result = argsFn ? await validator.run(argsFn(worktreePath, node)) : await validator.run();
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
  return runRegistryValidators(node.tier, worktreePath, node);
}
