// T3 validator (issue #135, docs/SELF-EXTENSION-V2.md §9 T3 row) - the
// integration-test + build/clippy gate for a generated backend route.
//
// Given a worktree + params {crate, name}, this proves:
//  1. scope: the ONLY files the build touched are the route, its own
//     integration test, and (optionally) the crate's routes/mod.rs.
//  2. mod.rs, if touched, was edited additively only - no existing
//     registration line removed or modified (a pure-addition diff never
//     emits a body line starting with `-`).
//  3. the integration test exists and proves it targets a scratch DB (the
//     crate's own `std::env::temp_dir()` + Config-literal pattern), never a
//     canonical `lifeos.db` / `~/` / hardcoded `/Users/...` path.
//  4. `cargo build -p <crate>`, `cargo test -p <crate> --test
//     <name>_integration`, and `cargo clippy -p <crate> -- -D warnings` all
//     succeed - shelled via an injectable execFn so vitest never runs real
//     cargo.
//
// Fail-closed: an uninspectable diff, a missing test file, a canonical DB
// reference, or any cargo/clippy failure all reject.
import fs from "node:fs/promises";
import path from "node:path";
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";

const defaultExecFile = promisify(execFileCb);

const CARGO_BUILD_TIMEOUT_MS = 5 * 60 * 1000;
const CARGO_TEST_TIMEOUT_MS = 5 * 60 * 1000;
const CARGO_CLIPPY_TIMEOUT_MS = 5 * 60 * 1000;

// Canonical/real-DB markers a scratch-DB integration test must never
// reference: the real `lifeos.db` file, a home-relative path, or a
// hardcoded absolute /Users path (this developer's real filesystem).
const CANONICAL_DB_MARKERS = [/\blifeos\.db\b/, /~\//, /\/Users\//];
// The established scratch-DB pattern every sibling integration test under
// services/lifeos-api/tests/ already uses (docs/SELF-EXTENSION-V2.md #135 design).
const SCRATCH_DB_PATTERN = /temp_dir\s*\(\s*\)/;

function routePath(crate, name) {
  return `services/${crate}/src/routes/${name}.rs`;
}
function modPath(crate) {
  return `services/${crate}/src/routes/mod.rs`;
}
function testPath(crate, name) {
  return `services/${crate}/tests/${name}_integration.rs`;
}

async function fileExists(absPath) {
  try {
    await fs.access(absPath);
    return true;
  } catch {
    return false;
  }
}

// Splits a NUL-delimited `git status --porcelain -z` stream into repo-relative
// changed paths (new + modified). A rename/copy's origin token is consumed
// and dropped - the T3 scope never renames a file.
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

// Scope check: nothing is committed yet at validate time (node.js leaves a
// T1-T5 worktree uncommitted), so `git status --porcelain -uall` alone is the
// build's full diff against its base. Git inspection always shells the real
// `git` (never mocked - only cargo/clippy are DI'd, since a test's fixture is
// itself a real scratch git repo).
async function checkScope({ worktreePath, crate, name }) {
  const allowed = new Set([routePath(crate, name), modPath(crate), testPath(crate, name)]);
  let stdout;
  try {
    ({ stdout } = await defaultExecFile("git", ["status", "--porcelain", "-uall", "-z"], { cwd: worktreePath }));
  } catch (error) {
    return { valid: false, errors: [`could not inspect the build diff: ${error.message}`] };
  }
  const changed = changedPathsFromPorcelain(stdout);
  const outOfScope = changed.filter((p) => !allowed.has(p));
  if (outOfScope.length > 0) {
    return { valid: false, errors: [`T3 build touched file(s) outside its scope: ${outOfScope.join(", ")}`] };
  }
  return { valid: true, errors: [] };
}

// mod.rs must be additive-only: a pure-addition unified diff never emits a
// body line starting with `-` (the `---` file header is excluded).
async function checkModAdditive({ worktreePath, crate }) {
  const rel = modPath(crate);
  let stdout;
  try {
    ({ stdout } = await defaultExecFile("git", ["diff", "--", rel], { cwd: worktreePath }));
  } catch (error) {
    return { valid: false, errors: [`could not inspect mod.rs diff: ${error.message}`] };
  }
  if (stdout.trim().length === 0) return { valid: true, errors: [] }; // mod.rs untouched
  const removed = stdout.split("\n").filter((line) => line.startsWith("-") && !line.startsWith("---"));
  if (removed.length > 0) {
    return { valid: false, errors: [`mod.rs diff removes or modifies existing line(s): ${removed.join(" | ")}`] };
  }
  return { valid: true, errors: [] };
}

// The integration test must exist and prove it targets a scratch DB, never
// the canonical lifeos.db / a hardcoded home/user path.
async function checkIntegrationTest({ worktreePath, crate, name }) {
  const rel = testPath(crate, name);
  const abs = path.join(worktreePath, rel);
  if (!(await fileExists(abs))) {
    return { valid: false, errors: [`missing integration test file ${rel}`] };
  }
  const source = await fs.readFile(abs, "utf8");
  if (CANONICAL_DB_MARKERS.some((re) => re.test(source))) {
    return { valid: false, errors: [`integration test references a canonical DB path (must use a scratch/temp DB): ${rel}`] };
  }
  if (!SCRATCH_DB_PATTERN.test(source)) {
    return {
      valid: false,
      errors: [`integration test must construct its DB via the established std::env::temp_dir() scratch-db pattern: ${rel}`],
    };
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

// validateT3Route({ worktreePath, params: { crate, name }, opts }) -> { valid, errors }
// opts.execFn is DI: (cmd, args, options) -> Promise<{ stdout, stderr }> -
// never real cargo/git in vitest, callers inject a mock.
export async function validateT3Route({ worktreePath, params = {}, opts = {} } = {}) {
  const { crate, name } = params;
  if (!crate || !name) {
    return { valid: false, errors: ["T3 validator requires params.crate and params.name"] };
  }
  // opts.execFn is DI for cargo/clippy ONLY - git status/diff always shell
  // the real git, since a caller's fixture is itself a real scratch git repo.
  const execFn = opts.execFn ?? defaultExecFile;

  const scope = await checkScope({ worktreePath, crate, name });
  if (!scope.valid) return scope;

  const modAdditive = await checkModAdditive({ worktreePath, crate });
  if (!modAdditive.valid) return modAdditive;

  const testCheck = await checkIntegrationTest({ worktreePath, crate, name });
  if (!testCheck.valid) return testCheck;

  const build = await runCargo(execFn, worktreePath, ["build", "-p", crate], CARGO_BUILD_TIMEOUT_MS, "cargo build");
  if (!build.valid) return build;

  const test = await runCargo(
    execFn,
    worktreePath,
    ["test", "-p", crate, "--test", `${name}_integration`],
    CARGO_TEST_TIMEOUT_MS,
    "cargo test",
  );
  if (!test.valid) return test;

  const clippy = await runCargo(
    execFn,
    worktreePath,
    ["clippy", "-p", crate, "--", "-D", "warnings"],
    CARGO_CLIPPY_TIMEOUT_MS,
    "cargo clippy",
  );
  if (!clippy.valid) return clippy;

  return { valid: true, errors: [] };
}

export const t3RouteValidator = { name: "t3Route", run: validateT3Route };
