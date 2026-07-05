// T5 validator (issue #137, docs/SELF-EXTENSION-V2.md §9 T5 row) - the
// build/test/clippy + scope + reviewer-sign-off gate for a generated crate.
//
// Given a worktree + params {crate}, a threaded reviewer verdict, and an
// injectable execFn, this proves:
//  1. scope: every changed file is confined to services/<crate>/** or the one
//     shared services/Cargo.toml (nothing else in the tree moved).
//  2. services/Cargo.toml, if touched, was edited ADDITIVELY only - a
//     pure-addition unified diff never emits a body line starting with `-`
//     (the same additive-diff proof T3 applies to mod.rs). A destructive edit
//     to the workspace members array is rejected.
//  3. reviewer sign-off: the pipeline threads the maker-checker's verdict; a
//     node without a recorded { approve: true } fails (belt-and-braces on top
//     of t5Subsystem.js's own in-loop enforcement).
//  4. `cargo build -p <crate>`, `cargo test -p <crate>`, and
//     `cargo clippy -p <crate> -- -D warnings` all succeed - shelled via the
//     injectable execFn so vitest never runs real cargo.
//
// Eval-gate boundary: as documented for #125/#132, the sampled Haiku judge has
// no callable surface from this JS pipeline, so a validated T5 node still flows
// through gate.js's approval-only halt (now with requires_typed_confirm) rather
// than an autonomous eval-gate.
//
// Fail-closed: an uninspectable diff, a destructive Cargo.toml edit, a missing
// sign-off, or any cargo/clippy failure all reject.
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";

const defaultExecFile = promisify(execFileCb);

const CARGO_BUILD_TIMEOUT_MS = 10 * 60 * 1000;
const CARGO_TEST_TIMEOUT_MS = 10 * 60 * 1000;
const CARGO_CLIPPY_TIMEOUT_MS = 10 * 60 * 1000;

const CARGO_TOML = "services/Cargo.toml";

function cratePrefix(crate) {
  return `services/${crate}/`;
}

// Splits a NUL-delimited `git status --porcelain -z` stream into repo-relative
// changed paths. A rename/copy's origin token is consumed and dropped - a T5
// crate build never renames a file.
function changedPathsFromPorcelain(stdout) {
  const tokens = stdout.split("\0").filter((t) => t.length > 0);
  const paths = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const entry = tokens[i];
    const status = entry.slice(0, 2);
    paths.push(entry.slice(3));
    if (status[0] === "R" || status[0] === "C") i += 1; // consume the origin token
  }
  return paths;
}

// Scope check: nothing is committed yet at validate time (node.js leaves the
// worktree uncommitted), so `git status --porcelain -uall` is the build's full
// diff. Every path must be under services/<crate>/ or exactly services/Cargo.toml.
async function checkScope({ worktreePath, crate }) {
  let stdout;
  try {
    ({ stdout } = await defaultExecFile("git", ["status", "--porcelain", "-uall", "-z"], { cwd: worktreePath }));
  } catch (error) {
    return { valid: false, errors: [`could not inspect the build diff: ${error.message}`] };
  }
  const prefix = cratePrefix(crate);
  const changed = changedPathsFromPorcelain(stdout);
  const outOfScope = changed.filter((p) => p !== CARGO_TOML && !p.startsWith(prefix));
  if (outOfScope.length > 0) {
    return { valid: false, errors: [`T5 build touched file(s) outside its scope: ${outOfScope.join(", ")}`] };
  }
  return { valid: true, errors: [] };
}

// services/Cargo.toml must be additive-only: a pure-addition unified diff never
// emits a body line starting with `-` (the `---` file header is excluded).
async function checkCargoTomlAdditive({ worktreePath }) {
  let stdout;
  try {
    ({ stdout } = await defaultExecFile("git", ["diff", "--", CARGO_TOML], { cwd: worktreePath }));
  } catch (error) {
    return { valid: false, errors: [`could not inspect ${CARGO_TOML} diff: ${error.message}`] };
  }
  if (stdout.trim().length === 0) return { valid: true, errors: [] }; // Cargo.toml untouched
  const removed = stdout.split("\n").filter((line) => line.startsWith("-") && !line.startsWith("---"));
  if (removed.length > 0) {
    return { valid: false, errors: [`${CARGO_TOML} diff removes or modifies existing line(s): ${removed.join(" | ")}`] };
  }
  return { valid: true, errors: [] };
}

// The maker-checker's adversarial sign-off must be recorded and positive.
function checkReview(review) {
  if (!review || review.approve !== true) {
    return { valid: false, errors: ["T5 node lacks a reviewer sign-off (review.approve must be true)"] };
  }
  return { valid: true, errors: [] };
}

async function runCargo(execFn, worktreePath, args, timeout, label) {
  try {
    await execFn("cargo", args, { cwd: worktreePath, timeout });
    return { valid: true, errors: [] };
  } catch (error) {
    return { valid: false, errors: [`${label} failed: ${error.message}`] };
  }
}

// validateT5Crate({ worktreePath, params: { crate }, review, opts }) -> { valid, errors }
// opts.execFn is DI for cargo/clippy ONLY - git status/diff always shell the
// real git, since a caller's fixture is itself a real scratch git repo.
export async function validateT5Crate({ worktreePath, params = {}, review, opts = {} } = {}) {
  const { crate } = params;
  if (!crate) {
    return { valid: false, errors: ["T5 validator requires params.crate"] };
  }
  const execFn = opts.execFn ?? defaultExecFile;

  const scope = await checkScope({ worktreePath, crate });
  if (!scope.valid) return scope;

  const cargoToml = await checkCargoTomlAdditive({ worktreePath });
  if (!cargoToml.valid) return cargoToml;

  const signoff = checkReview(review);
  if (!signoff.valid) return signoff;

  const build = await runCargo(execFn, worktreePath, ["build", "-p", crate], CARGO_BUILD_TIMEOUT_MS, "cargo build");
  if (!build.valid) return build;

  const test = await runCargo(execFn, worktreePath, ["test", "-p", crate], CARGO_TEST_TIMEOUT_MS, "cargo test");
  if (!test.valid) return test;

  const clippy = await runCargo(execFn, worktreePath, ["clippy", "-p", crate, "--", "-D", "warnings"], CARGO_CLIPPY_TIMEOUT_MS, "cargo clippy");
  if (!clippy.valid) return clippy;

  return { valid: true, errors: [] };
}

export const t5CrateValidator = { name: "t5Crate", run: validateT5Crate };
