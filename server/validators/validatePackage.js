// Marketplace package validator (issue #147) - the Tier-0 validator chain,
// re-run on a signed module package's manifest before an install activates it.
// "Install re-validates through the tier ladder" is the issue's non-negotiable
// acceptance: a package is only trusted because its signature verifies AND its
// manifest still passes the same gates a fresh T0 build would.
//
// A marketplace package is manifest-only (declarative T0 data, no code diff),
// so of the T0 validators in server/validators/registry.js it runs:
//   - structural (validateStructural): ajv schema + entity-type collision +
//     dangling-view-ref checks - the meaningful gate for a manifest.
//   - protectedSurface (adapted): the live PROTECTED_SURFACES / TIER_SCOPES
//     source of truth (server/lib/tierScopes.js) asserted against the T0
//     install target `modules/<id>/module.js`. There is no git worktree/diff
//     for a manifest-only artifact, so we evaluate the write target directly
//     (the same evaluateWrite decision Layer B uses), rather than a diff.
//
// The render-smoke validator (headless-Chromium boot, server/validators/
// render.js) is deliberately SKIPPED here: booting a browser per install is far
// too heavy for a synchronous HTTP install path. Render validation stays a
// build-time (scaffold) concern - noted, not silently dropped.
//
// Two shapes: `validatePackage(manifest)` for direct import (tests / any Node
// caller), and a CLI wrapper (`node validatePackage.js <manifest.json>`) whose
// LAST stdout line is the JSON result `{ valid, errors, tier }` - the same
// last-line-JSON contract lifeos-drain / lifeos-api use to shell Node entries.
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { validateStructural } from "./structural.js";
import { evaluateWrite } from "../lib/tierScopes.js";

const REPO_ROOT = path.resolve(import.meta.dirname, "..", "..");
// The manifest.id slug schema (module.schema.json) - safe as a directory name.
const SAFE_ID = /^[a-z][a-z0-9_]*$/;

// Materializes the manifest as an isolated modules/<id>/module.js in a temp dir
// and runs the real structural validator against it. Isolated (no siblings) so
// it checks the manifest's own internal consistency + schema, not collisions
// with whatever modules happen to be installed on this particular host - a
// marketplace package legitimately re-declares its own entity types.
async function runStructural(manifest) {
  const id = typeof manifest?.id === "string" && SAFE_ID.test(manifest.id) ? manifest.id : "_invalid_pkg";
  const modulesDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-pkg-"));
  try {
    const dir = path.join(modulesDir, id);
    await fs.mkdir(dir, { recursive: true });
    const source = `osRegisterModule(${JSON.stringify(manifest ?? {})});`;
    await fs.writeFile(path.join(dir, "module.js"), source, "utf8");
    const { valid, errors } = await validateStructural(path.join(dir, "module.js"), { modulesDir });
    return { valid, errors };
  } finally {
    await fs.rm(modulesDir, { recursive: true, force: true });
  }
}

// The protectedSurface gate, adapted to a manifest-only artifact: assert the T0
// install target is inside the T0 write-scope and not a never-generable surface
// (deny-wins), using the same evaluateWrite decision Layer B applies to every
// build write. A schema-valid id can never escape its module dir, but this
// reuses the single source of truth rather than re-trusting the schema.
function runProtectedSurface(manifest) {
  const id = typeof manifest?.id === "string" ? manifest.id : "";
  const target = `modules/${id}/module.js`;
  const decision = evaluateWrite({ tier: "T0", params: { moduleId: id }, root: REPO_ROOT }, target);
  if (decision.allowed) return { valid: true, errors: [] };
  return { valid: false, errors: [`install target rejected: ${decision.reason}`] };
}

// Runs the full T0 package chain. Fail-closed: any errors from any gate mean
// the package is not installable.
export async function validatePackage(manifest) {
  const results = [runProtectedSurface(manifest), await runStructural(manifest)];
  const errors = results.flatMap((r) => r.errors);
  return { valid: errors.length === 0, errors, tier: "T0" };
}

// CLI: `node validatePackage.js <manifest.json>`. Reads the manifest JSON from
// the given path and prints the result as the LAST stdout line. Any failure to
// read/parse is itself a validation failure (fail-closed), never a crash - the
// Rust install route treats a non-zero/no-output run as "do not install".
async function main() {
  const manifestPath = process.argv[2];
  let result;
  if (!manifestPath) {
    result = { valid: false, errors: ["no manifest path given"], tier: "T0" };
  } else {
    try {
      const manifest = JSON.parse(await fs.readFile(manifestPath, "utf8"));
      result = await validatePackage(manifest);
    } catch (error) {
      result = { valid: false, errors: [`could not read/parse manifest: ${error.message}`], tier: "T0" };
    }
  }
  process.stdout.write(`${JSON.stringify(result)}\n`);
  process.exit(result.valid ? 0 : 1);
}

// Run as a CLI only when invoked directly, not when imported by a test.
if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename)) {
  main();
}
