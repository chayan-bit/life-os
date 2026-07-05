// Validator registry (docs/SELF-EXTENSION-V2.md §9) - the per-tier validator
// gates the build pipeline dispatches to per DAG node. Today's two Tier-0
// validators (structural, render-smoke) become the T0 entry; every tier
// additionally gets `protectedSurfaceValidator` FIRST, so a diff touching any
// never-generable surface (§5) is hard-rejected regardless of tier.
//
// Fail-closed: a tier whose real validators aren't built yet resolves to a
// placeholder that REJECTS - an unvalidatable tier must never pass.
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";
import { isProtectedPath } from "../lib/tierScopes.js";
import { validateStructural } from "./structural.js";
import { validateRenderSmoke } from "./render.js";
import { validateT1Render } from "./t1Render.js";
import { validateT2Tool } from "./t2Tool.js";
import { validateT3Route } from "./t3Route.js";
import { validateT4Migration } from "./t4Migration.js";
import { validateT5Crate } from "./t5Crate.js";

const execFile = promisify(execFileCb);
const DEFAULT_BASE_REF = "main";

// Splits a NUL-delimited git stream into non-empty tokens.
function splitZ(stdout) {
  return stdout.split("\0").filter((t) => t.length > 0);
}

// Parses `git status --porcelain -z`: each entry is `XY <path>`, and a rename
// or copy (`R`/`C`) is followed by a second NUL-terminated origin path. Returns
// the set of new/current paths (the origin of a rename is reported separately
// by the diff, and matching either against a protected surface is what we want).
function parsePorcelainZ(tokens) {
  const paths = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const entry = tokens[i];
    const status = entry.slice(0, 2);
    paths.push(entry.slice(3));
    if (status[0] === "R" || status[0] === "C") i += 1; // consume the origin token
  }
  return paths;
}

// Collects every path the worktree changed vs `baseRef`: committed changes
// (`diff base...HEAD`) unioned with uncommitted ones (`status --porcelain`),
// since a build may validate either before its commit (Tier 0) or after it
// (pipeline nodes).
async function changedPaths(worktreePath, baseRef) {
  const opts = { cwd: worktreePath };
  const [{ stdout: diffOut }, { stdout: statusOut }] = await Promise.all([
    execFile("git", ["diff", "--name-only", "-z", `${baseRef}...HEAD`], opts),
    // -uall so a brand-new file inside a new dir is listed individually, not
    // collapsed to `?? server/` (git's default), which would hide the path.
    execFile("git", ["status", "--porcelain", "-uall", "-z"], opts),
  ]);
  return new Set([...splitZ(diffOut), ...parsePorcelainZ(splitZ(statusOut))]);
}

// The §5 gate: reject if ANY changed path matches a never-generable surface.
// Runs at every tier. Git failure fails closed (rejects) - an un-inspectable
// diff must not be assumed clean.
async function runProtectedSurface({ worktreePath, baseRef = DEFAULT_BASE_REF }) {
  let paths;
  try {
    paths = await changedPaths(worktreePath, baseRef);
  } catch (error) {
    return { valid: false, errors: [`could not inspect the build diff for protected surfaces: ${error.message}`] };
  }
  const hits = [...paths].filter((p) => isProtectedPath(p));
  if (hits.length > 0) {
    return { valid: false, errors: hits.map((p) => `diff touches a never-generable protected surface: ${p}`) };
  }
  return { valid: true, errors: [] };
}

const protectedSurface = { name: "protectedSurface", run: runProtectedSurface };
const structural = { name: "structural", run: validateStructural };
const renderSmoke = { name: "renderSmoke", run: validateRenderSmoke };
const t1Render = { name: "t1Render", run: validateT1Render };
const t2Tool = { name: "t2Tool", run: validateT2Tool };
const t3Route = { name: "t3Route", run: validateT3Route };
const t4Migration = { name: "t4Migration", run: validateT4Migration };
const t5Crate = { name: "t5Crate", run: validateT5Crate };

function placeholder(tier) {
  return {
    name: "notImplemented",
    run: async () => ({ valid: false, errors: [`validator not yet implemented for ${tier}`] }),
  };
}

// Every tier T0-T5 now has a real, tier-specific validator behind the
// protected-surface gate (issue #133 landed T1's, #134 T2's, #135 T3's, #136
// T4's, #137 T5's) - the fail-closed placeholder is retained only for an
// UNKNOWN tier (getValidators below), never for a known one.
const TIER_VALIDATORS = {
  T0: [protectedSurface, structural, renderSmoke],
  T1: [protectedSurface, t1Render],
  T2: [protectedSurface, t2Tool],
  T3: [protectedSurface, t3Route],
  T4: [protectedSurface, t4Migration],
  T5: [protectedSurface, t5Crate],
};

// Ordered validator list for `tier`. Unknown tier fails closed (empty-safe:
// callers treat "no validators" as un-validatable and must not proceed).
export function getValidators(tier) {
  return TIER_VALIDATORS[tier] ?? [placeholder(tier ?? "unknown")];
}
