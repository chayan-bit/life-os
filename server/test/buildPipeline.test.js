import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runBuildPipeline } from "../build/pipeline.js";
import { getValidators } from "../validators/registry.js";
import { TIERS } from "../build/plan.js";
import { slugify } from "../lib/slugify.js";

const execFile = promisify(execFileCb);
const REAL_TEMPLATE = path.resolve(import.meta.dirname, "..", "..", "modules", "_template");

let repoRoot;

async function git(args, cwd = repoRoot) {
  return execFile("git", args, { cwd });
}

async function mainLogCount() {
  const { stdout } = await git(["log", "--oneline", "main"]);
  return stdout.split("\n").filter(Boolean).length;
}

beforeEach(async () => {
  repoRoot = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-build-repo-"));
  await git(["init", "-b", "main"]);
  await git(["config", "user.email", "test@example.com"]);
  await git(["config", "user.name", "Test"]);
  await fs.mkdir(path.join(repoRoot, "modules"), { recursive: true });
  await fs.cp(REAL_TEMPLATE, path.join(repoRoot, "modules", "_template"), { recursive: true });
  await git(["add", "modules"]);
  await git(["commit", "-m", "seed _template"]);
});

afterEach(async () => {
  await fs.rm(repoRoot, { recursive: true, force: true });
});

// ------- mock building blocks ------------------------------------------------

// Entity type ids must be unique across the whole modules/ tree (the structural
// validator's dup-type-id check runs against every sibling module in the
// worktree), so each generated module gets its own type id derived from its id.
function typeIdFor(id) {
  return `item_${id}`;
}

function moduleSourceFor(id) {
  const t = typeIdFor(id);
  return `osRegisterModule({
  id: "${id}",
  name: "Gen",
  icon: "BookOpen",
  color: "var(--neo-yellow)",
  entityTypes: { ${t}: { label: "Item", plural: "Items", icon: "FileText", attrs: { name: { type: "text", required: true } } } },
  views: [{ id: "all", label: "All", kind: "list", type: "${t}" }],
  botCommands: [{ cmd: "add", help: "Add", handler: "handleAdd" }],
  agentTools: [{ name: "${id}.add", schema: {}, impl: "handleAdd", gated: false }],
});
`;
}

function manifestFor(id) {
  const t = typeIdFor(id);
  return {
    id,
    name: "Gen",
    icon: "BookOpen",
    color: "var(--neo-yellow)",
    entityTypes: [{ id: t, label: "Item", plural: "Items", icon: "FileText", attrs: { name: { type: "text", required: true } } }],
    views: [{ id: "all", label: "All", kind: "list", type: t }],
    botCommands: [{ cmd: "add", help: "Add" }],
    agentTools: [{ name: `${id}.add`, gated: false }],
  };
}

const SPEC = { summary: "s", entities: ["item"], views: ["list"], tools: [], routes: [], migrations: [] };

// A routed mock queryFn: spec/plan calls are keyed by options.purpose; a T0
// scaffold call is detected by its ModuleManifest outputFormat (writes
// modules/<id>/module.js into the worktree, id derived from the worktree dir);
// a T1-T5 build call is detected by its { files } summary schema (writes one
// in-scope file so the node has a commitable diff).
function makeQueryFn(plan, calls) {
  return async function* queryFn(params) {
    const purpose = params.options?.purpose;
    if (purpose === "build_spec") {
      calls.push("spec");
      yield { type: "result", subtype: "success", is_error: false, structured_output: SPEC };
      return;
    }
    if (purpose === "build_plan") {
      calls.push("plan");
      yield { type: "result", subtype: "success", is_error: false, structured_output: plan };
      return;
    }
    const props = params.options?.outputFormat?.schema?.properties ?? {};
    if (props.entityTypes) {
      const id = path.basename(params.options.cwd).replace(/^scaffold-/, "");
      await fs.writeFile(path.join(params.options.cwd, "modules", id, "module.js"), moduleSourceFor(id), "utf8");
      yield { type: "result", subtype: "success", is_error: false, structured_output: manifestFor(id) };
      return;
    }
    // T1-T5 build node: write one in-scope file so commit has something to add.
    const target = path.join(params.options.cwd, "frontend", "src", "core", "renderers", "Generic.jsx");
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, "export default function Generic() { return null; }\n", "utf8");
    yield { type: "result", subtype: "success", is_error: false, structured_output: { tier: "T1", files: ["Generic.jsx"], summary: "ok" } };
  };
}

// Records every httpFn call and returns a stable created-entity id.
function makeHttpFn(calls) {
  return async function httpFn(method, path_, body) {
    calls.push({ method, path: path_, body });
    return { ok: true, data: { id: "ent_1" } };
  };
}

const noopPersist = async () => {};

// ------- tests ---------------------------------------------------------------

describe("runBuildPipeline - two-node T0 DAG happy path", () => {
  it("specs, plans, persists the DAG, builds+commits both nodes, emits build.completed", async () => {
    const mA = slugify("add a reading list module");
    const mB = slugify("add a habit tracker module");
    const plan = {
      nodes: [
        { id: "a", tier: "T0", params: { moduleId: mA, prompt: "add a reading list module" }, description: "add a reading list module", dependsOn: [] },
        { id: "b", tier: "T0", params: { moduleId: mB, prompt: "add a habit tracker module" }, description: "add a habit tracker module", dependsOn: [] },
      ],
    };
    const queryCalls = [];
    const httpCalls = [];
    const beforeCount = await mainLogCount();

    const result = await runBuildPipeline("build two modules", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, queryCalls),
      httpFn: makeHttpFn(httpCalls),
      persistManifestEntity: noopPersist,
      validateRenderSmoke: async () => ({ valid: true, errors: [] }),
    });

    expect(queryCalls).toEqual(["spec", "plan"]);
    expect(result.success).toBe(true);
    expect(result.nodes.map((n) => n.status)).toEqual(["completed", "completed"]);
    expect(result.nodes.every((n) => n.commit)).toBe(true);

    // DAG persisted as a pipelines/pipeline_run entity.
    const persisted = httpCalls.find((c) => c.method === "POST" && c.path === "/api/entity" && c.body.type === "pipeline_run");
    expect(persisted).toBeTruthy();
    expect(persisted.body.module).toBe("pipelines");
    expect(persisted.body.attrs.origin).toBe("build");

    // build.completed event emitted.
    const done = httpCalls.find((c) => c.path === "/api/event" && c.body.type === "build.completed");
    expect(done).toBeTruthy();
    expect(done.body.outcome).toBe("completed");

    // Two commits landed on main (on top of the seed).
    expect(await mainLogCount()).toBe(beforeCount + 2);
    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });
});

describe("runBuildPipeline - mid-DAG validator failure aborts the subtree", () => {
  it("commits the passing node, fails the invalid node, skips its dependent, keeps the commit", async () => {
    const plan = {
      nodes: [
        { id: "A", tier: "T1", params: { kind: "board" }, description: "renderer A", dependsOn: [] },
        { id: "B", tier: "T1", params: { kind: "graph" }, description: "renderer B", dependsOn: [] },
        { id: "C", tier: "T1", params: { kind: "calendar" }, description: "renderer C", dependsOn: ["B"] },
      ],
    };
    const beforeCount = await mainLogCount();

    const result = await runBuildPipeline("build renderers", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, []),
      httpFn: makeHttpFn([]),
      validateFn: async (tier, worktreePath, node) =>
        node.id === "B" ? { valid: false, errors: ["boom"] } : { valid: true, errors: [] },
    });

    expect(result.success).toBe(false);
    const byId = Object.fromEntries(result.nodes.map((n) => [n.id, n]));
    expect(byId.A.status).toBe("completed");
    expect(byId.A.commit).toBeTruthy();
    expect(byId.B.status).toBe("failed");
    expect(byId.C.status).toBe("skipped");
    expect(byId.C.reason).toMatch(/dependency failed/);

    // A's commit remains (forward-only) - exactly one new commit on main.
    expect(await mainLogCount()).toBe(beforeCount + 1);
    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });
});

describe("runBuildPipeline - T3 node is gated, not committed", () => {
  it("marks the node awaiting_approval, creates a pending_approval entity, commits nothing", async () => {
    const plan = {
      nodes: [{ id: "r", tier: "T3", params: { crate: "lifeos-x", name: "week" }, description: "weekly summary route", dependsOn: [] }],
    };
    const httpCalls = [];
    const beforeCount = await mainLogCount();

    const result = await runBuildPipeline("add a weekly summary endpoint", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, []),
      httpFn: makeHttpFn(httpCalls),
      validateFn: async () => ({ valid: true, errors: [] }),
    });

    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("awaiting_approval");

    const pending = httpCalls.find((c) => c.method === "POST" && c.path === "/api/entity" && c.body.type === "pending_approval");
    expect(pending).toBeTruthy();
    expect(pending.body.attrs.tier).toBe("T3");
    const gated = httpCalls.find((c) => c.path === "/api/event" && c.body.type === "build.node.gated");
    expect(gated).toBeTruthy();

    // Nothing committed.
    expect(await mainLogCount()).toBe(beforeCount);
  });
});

describe("runBuildPipeline - cyclic plan fails closed before any build", () => {
  it("rejects the plan, builds nothing, persists no run", async () => {
    const plan = {
      nodes: [
        { id: "x", tier: "T1", params: { kind: "board" }, description: "x", dependsOn: ["y"] },
        { id: "y", tier: "T1", params: { kind: "graph" }, description: "y", dependsOn: ["x"] },
      ],
    };
    const httpCalls = [];
    const beforeCount = await mainLogCount();

    const result = await runBuildPipeline("cyclic build", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, []),
      httpFn: makeHttpFn(httpCalls),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/cycle/);
    expect(result.nodes).toEqual([]);
    // No pipeline_run persisted, nothing committed, no worktree left behind.
    expect(httpCalls.some((c) => c.body?.type === "pipeline_run")).toBe(false);
    expect(await mainLogCount()).toBe(beforeCount);
    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });
});

// ------- T5 subsystem build (issue #137) -------------------------------------

const T5_CRATE = "lifeos-finance";
const CARGO_TOML_SEED = '[workspace]\nresolver = "2"\nmembers = [\n    "lifeos-api",\n]\n';

// Seeds a committed services/Cargo.toml so the scaffolder's additive member
// edit diffs cleanly (mirrors the real workspace shape).
async function seedServicesWorkspace() {
  await fs.mkdir(path.join(repoRoot, "services"), { recursive: true });
  await fs.writeFile(path.join(repoRoot, "services", "Cargo.toml"), CARGO_TOML_SEED, "utf8");
  await git(["add", "services"]);
  await git(["commit", "-m", "seed services workspace"]);
}

// A routed T5 mock: spec/plan by purpose; supervisor by its {files,testStrategy}
// schema; reviewer by its {approve,issues} schema; scaffolder vs tester by the
// role word in the prompt. `opts` tunes reviewer verdicts and per-call usage.
function makeT5QueryFn(plan, calls, opts = {}) {
  const reviewVerdicts = opts.reviewVerdicts ?? [{ approve: true, issues: [] }];
  const usage = opts.usage ?? { input_tokens: 1, output_tokens: 1 };
  let reviewIdx = 0;
  return async function* queryFn(params) {
    const purpose = params.options?.purpose;
    if (purpose === "build_spec") {
      yield { type: "result", subtype: "success", is_error: false, structured_output: SPEC };
      return;
    }
    if (purpose === "build_plan") {
      yield { type: "result", subtype: "success", is_error: false, structured_output: plan };
      return;
    }
    const props = params.options?.outputFormat?.schema?.properties ?? {};
    const prompt = params.prompt ?? "";
    if (props.testStrategy) {
      calls.push("supervisor");
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        usage,
        structured_output: {
          files: [{ path: `services/${T5_CRATE}/src/lib.rs`, purpose: "core" }],
          testStrategy: "scratch-db integration test",
          riskNotes: "none",
        },
      };
      return;
    }
    if (props.approve) {
      calls.push("reviewer");
      const verdict = reviewVerdicts[Math.min(reviewIdx, reviewVerdicts.length - 1)];
      reviewIdx += 1;
      yield { type: "result", subtype: "success", is_error: false, usage, structured_output: verdict };
      return;
    }
    // buildNodeSummary schema: scaffolder writes the crate + additive Cargo.toml;
    // tester writes only a scratch-DB test.
    const crateDir = path.join(params.options.cwd, "services", T5_CRATE);
    if (prompt.includes("scaffolder")) {
      calls.push("scaffolder");
      await fs.mkdir(path.join(crateDir, "src"), { recursive: true });
      await fs.writeFile(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${T5_CRATE}"\nversion = "0.1.0"\nedition = "2021"\n`, "utf8");
      await fs.writeFile(path.join(crateDir, "src", "lib.rs"), "pub fn add(a: i64, b: i64) -> i64 { a + b }\n", "utf8");
      const cargoToml = path.join(params.options.cwd, "services", "Cargo.toml");
      await fs.writeFile(cargoToml, CARGO_TOML_SEED.replace('    "lifeos-api",\n', `    "lifeos-api",\n    "${T5_CRATE}",\n`), "utf8");
      yield { type: "result", subtype: "success", is_error: false, usage, structured_output: { tier: "T5", files: [`services/${T5_CRATE}/src/lib.rs`], summary: "scaffolded" } };
      return;
    }
    calls.push("tester");
    await fs.mkdir(path.join(crateDir, "tests"), { recursive: true });
    await fs.writeFile(path.join(crateDir, "tests", "it.rs"), '#[test]\nfn ok() { let _ = std::env::temp_dir().join("scratch.db"); assert_eq!(1, 1); }\n', "utf8");
    yield { type: "result", subtype: "success", is_error: false, usage, structured_output: { tier: "T5", files: [`services/${T5_CRATE}/tests/it.rs`], summary: "tested" } };
  };
}

function t5Plan() {
  return { nodes: [{ id: "t5", tier: "T5", params: { crate: T5_CRATE }, description: "new finance subsystem crate", dependsOn: [] }] };
}

describe("runBuildPipeline - T5 subsystem builds via the bounded supervisor+3 split and gates (issue #137)", () => {
  it("runs supervisor->scaffolder->tester->reviewer, validates via the REAL t5Crate validator, then halts awaiting_approval with requires_typed_confirm, uncommitted", async () => {
    await seedServicesWorkspace();
    const beforeCount = await mainLogCount();
    const calls = [];
    const httpCalls = [];

    const result = await runBuildPipeline("build a finance module with its own ingest pipeline", "ws_test", {
      repoRoot,
      queryFn: makeT5QueryFn(t5Plan(), calls),
      httpFn: makeHttpFn(httpCalls),
      execFn: async () => ({ stdout: "", stderr: "" }),
    });

    expect(calls).toEqual(["supervisor", "scaffolder", "tester", "reviewer"]);
    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("awaiting_approval");

    const pending = httpCalls.find((c) => c.method === "POST" && c.path === "/api/entity" && c.body.type === "pending_approval");
    expect(pending).toBeTruthy();
    expect(pending.body.attrs.tier).toBe("T5");
    expect(pending.body.attrs.requires_typed_confirm).toBe(true);
    const planned = httpCalls.find((c) => c.path === "/api/event" && c.body.type === "build.t5.planned");
    expect(planned).toBeTruthy();

    // Nothing committed - the gate halts before commit.js ever runs.
    expect(await mainLogCount()).toBe(beforeCount);
  });

  it("fails the node when the T5 validator's cargo test fails (execFn DI)", async () => {
    await seedServicesWorkspace();
    const beforeCount = await mainLogCount();
    const httpCalls = [];

    const result = await runBuildPipeline("build a finance module", "ws_test", {
      repoRoot,
      queryFn: makeT5QueryFn(t5Plan(), []),
      httpFn: makeHttpFn(httpCalls),
      execFn: async (cmd, args) => {
        if (cmd === "cargo" && args[0] === "test") throw new Error("test failed");
        return { stdout: "", stderr: "" };
      },
    });

    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("failed");
    expect(result.nodes[0].reason).toMatch(/cargo test failed/);
    expect(await mainLogCount()).toBe(beforeCount);
  });

  it("recovers via one bounded fix round when the reviewer rejects once then approves", async () => {
    await seedServicesWorkspace();
    const calls = [];

    const result = await runBuildPipeline("build a finance module", "ws_test", {
      repoRoot,
      queryFn: makeT5QueryFn(t5Plan(), calls, {
        reviewVerdicts: [{ approve: false, issues: ["missing error handling"] }, { approve: true, issues: [] }],
      }),
      httpFn: makeHttpFn([]),
      execFn: async () => ({ stdout: "", stderr: "" }),
    });

    // supervisor, scaffolder, tester, reviewer(reject), scaffolder(fix), reviewer(approve)
    expect(calls).toEqual(["supervisor", "scaffolder", "tester", "reviewer", "scaffolder", "reviewer"]);
    expect(result.nodes[0].status).toBe("awaiting_approval");
  });

  it("fails the node on a second reviewer rejection - no third round", async () => {
    await seedServicesWorkspace();
    const calls = [];

    const result = await runBuildPipeline("build a finance module", "ws_test", {
      repoRoot,
      queryFn: makeT5QueryFn(t5Plan(), calls, { reviewVerdicts: [{ approve: false, issues: ["bad"] }] }),
      httpFn: makeHttpFn([]),
      execFn: async () => ({ stdout: "", stderr: "" }),
    });

    expect(result.nodes[0].status).toBe("failed");
    expect(result.nodes[0].reason).toMatch(/reviewer rejected the crate after one fix round/);
    // Exactly two reviewer calls (initial + re-review), never a third.
    expect(calls.filter((c) => c === "reviewer")).toHaveLength(2);
  });

  it("stops with budget_exhausted when the token ceiling is exceeded mid-sequence", async () => {
    await seedServicesWorkspace();
    const calls = [];

    // Ceiling 10; the supervisor alone reports 100 tokens, so the budget is
    // exhausted before the scaffolder is ever called.
    const result = await runBuildPipeline("build a finance module", "ws_test", {
      repoRoot,
      budget: 10,
      queryFn: makeT5QueryFn(t5Plan(), calls, { usage: { input_tokens: 100, output_tokens: 0 } }),
      httpFn: makeHttpFn([]),
      execFn: async () => ({ stdout: "", stderr: "" }),
    });

    expect(result.nodes[0].status).toBe("failed");
    expect(result.nodes[0].reason).toMatch(/budget_exhausted/);
    expect(calls).toEqual(["supervisor"]);
  });
});

describe("validator registry - every tier T0-T5 resolves to real validators", () => {
  it("has no fail-closed placeholder for any known tier", () => {
    for (const tier of TIERS) {
      const validators = getValidators(tier);
      expect(validators.length).toBeGreaterThan(0);
      expect(validators.some((v) => v.name === "notImplemented")).toBe(false);
    }
  });

  it("still fails closed for an unknown tier", () => {
    const validators = getValidators("T9");
    expect(validators.some((v) => v.name === "notImplemented")).toBe(true);
  });
});

describe("runBuildPipeline - T4 node builds, validates via the REAL t4Migration validator, and gates (issue #136)", () => {
  it("writes one additive migration file, validates it with the real sqlite3 CLI (execFn DI), then halts awaiting_approval without committing", async () => {
    const name = "due_date";
    const plan = {
      nodes: [{ id: "m", tier: "T4", params: { name }, description: "make due date a fast-queryable field", dependsOn: [] }],
    };
    // Seed one pre-existing migration on main (0001_core.sql, a non-virtual
    // CREATE TABLE) so the T4 build's own file can be a purely additive 0002
    // - a repo's very first-ever migration can't be additive-only by
    // definition, so this mirrors the real migrations/ directory's shape.
    await fs.mkdir(path.join(repoRoot, "migrations"), { recursive: true });
    await fs.writeFile(path.join(repoRoot, "migrations", "0001_core.sql"), "CREATE TABLE entities (id TEXT PRIMARY KEY);\n", "utf8");
    await git(["add", "migrations"]);
    await git(["commit", "-m", "seed 0001_core.sql"]);

    const beforeCount = await mainLogCount();
    const httpCalls = [];

    const queryFn = async function* (params) {
      const purpose = params.options?.purpose;
      if (purpose === "build_spec") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: SPEC };
        return;
      }
      if (purpose === "build_plan") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: plan };
        return;
      }
      // T4 build node: write ONE additive migration file - exactly the
      // t4Migration validator's file-discipline + statement-shape + scratch-
      // apply expectations.
      const migrationsDir = path.join(params.options.cwd, "migrations");
      await fs.mkdir(migrationsDir, { recursive: true });
      await fs.writeFile(
        path.join(migrationsDir, `0002_${name}.sql`),
        "ALTER TABLE entities ADD COLUMN due_text TEXT;\n" +
          "CREATE INDEX IF NOT EXISTS idx_entities_id ON entities (id);\n",
        "utf8",
      );
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        structured_output: { tier: "T4", files: [`migrations/0002_${name}.sql`], summary: "ok" },
      };
    };

    // No validateFn override: exercises the REAL t4Migration + protectedSurface
    // validators end to end, shelling the real sqlite3 CLI (no execFn DI
    // needed here since sqlite3 - unlike cargo - is fast enough to run for
    // real in this test).
    const result = await runBuildPipeline("make due date a fast-queryable field", "ws_test", {
      repoRoot,
      queryFn,
      httpFn: makeHttpFn(httpCalls),
    });

    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("awaiting_approval");

    const pending = httpCalls.find((c) => c.method === "POST" && c.path === "/api/entity" && c.body.type === "pending_approval");
    expect(pending).toBeTruthy();
    expect(pending.body.attrs.tier).toBe("T4");
    const gated = httpCalls.find((c) => c.path === "/api/event" && c.body.type === "build.node.gated");
    expect(gated).toBeTruthy();

    // Nothing committed - the gate halts before commit.js ever runs.
    expect(await mainLogCount()).toBe(beforeCount);
  });
});

describe("runBuildPipeline - T3 node builds, validates via the REAL t3Route validator, and gates (issue #135)", () => {
  it("writes an in-scope route + additive mod.rs + scratch-DB test, validates it with mocked cargo, then halts awaiting_approval without committing", async () => {
    const crate = "lifeos-x";
    const name = "week";
    const plan = {
      nodes: [{ id: "r", tier: "T3", params: { crate, name }, description: "weekly summary route", dependsOn: [] }],
    };
    const beforeCount = await mainLogCount();
    const httpCalls = [];

    const queryFn = async function* (params) {
      const purpose = params.options?.purpose;
      if (purpose === "build_spec") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: SPEC };
        return;
      }
      if (purpose === "build_plan") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: plan };
        return;
      }
      // T3 build node: write the route, an additive mod.rs, and a
      // scratch-DB integration test - exactly the t3Route validator's
      // scope + additive-diff + scratch-DB expectations.
      const routesDir = path.join(params.options.cwd, "services", crate, "src", "routes");
      const testsDir = path.join(params.options.cwd, "services", crate, "tests");
      await fs.mkdir(routesDir, { recursive: true });
      await fs.mkdir(testsDir, { recursive: true });
      const modPath = path.join(routesDir, "mod.rs");
      await fs.writeFile(modPath, "pub fn router() {}\n", "utf8");
      // Seed + commit mod.rs first so the additive edit below has a base to
      // diff against (mirrors createWorktree cutting from main's committed tip).
      await execFile("git", ["add", "."], { cwd: params.options.cwd });
      await execFile("git", ["commit", "-m", "seed mod.rs", "--allow-empty"], { cwd: params.options.cwd });
      await fs.writeFile(
        modPath,
        `pub fn router() {}\n// --- generated (T3) ---\nmod ${name};\n`,
        "utf8",
      );
      await fs.writeFile(path.join(routesDir, `${name}.rs`), "pub async fn week() {}\n", "utf8");
      await fs.writeFile(
        path.join(testsDir, `${name}_integration.rs`),
        '#[test]\nfn ok() { let _ = std::env::temp_dir().join("scratch.db"); }\n',
        "utf8",
      );
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        structured_output: {
          tier: "T3",
          files: [`services/${crate}/src/routes/${name}.rs`, `services/${crate}/src/routes/mod.rs`, `services/${crate}/tests/${name}_integration.rs`],
          summary: "ok",
        },
      };
    };

    // No validateFn override: exercises the REAL t3Route + protectedSurface
    // validators end to end. cargo build/test/clippy are mocked via
    // opts.execFn so this test never shells real cargo.
    const result = await runBuildPipeline("add a weekly summary endpoint", "ws_test", {
      repoRoot,
      queryFn,
      httpFn: makeHttpFn(httpCalls),
      execFn: async () => ({ stdout: "", stderr: "" }),
    });

    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("awaiting_approval");

    const pending = httpCalls.find((c) => c.method === "POST" && c.path === "/api/entity" && c.body.type === "pending_approval");
    expect(pending).toBeTruthy();
    expect(pending.body.attrs.tier).toBe("T3");
    const gated = httpCalls.find((c) => c.path === "/api/event" && c.body.type === "build.node.gated");
    expect(gated).toBeTruthy();

    // Nothing committed - the gate halts before commit.js ever runs.
    expect(await mainLogCount()).toBe(beforeCount);
  });
});

describe("runBuildPipeline - T2 node builds, validates, and commits a generated tool (issue #134)", () => {
  it("writes a valid pure-descriptor file and commits the node via the REAL t2Tool validator", async () => {
    const plan = {
      nodes: [{ id: "t2", tier: "T2", params: { name: "rMultiple" }, description: "R-multiple tool", dependsOn: [] }],
    };
    const beforeCount = await mainLogCount();

    const queryFn = async function* (params) {
      const purpose = params.options?.purpose;
      if (purpose === "build_spec") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: SPEC };
        return;
      }
      if (purpose === "build_plan") {
        yield { type: "result", subtype: "success", is_error: false, structured_output: plan };
        return;
      }
      // T2 build node: write ONE valid generated-tool descriptor file.
      const dir = path.join(params.options.cwd, "server", "agent", "tools", "generated");
      await fs.mkdir(dir, { recursive: true });
      const source = [
        'import { z } from "zod";',
        "",
        "const inputSchema = z.object({ tradeId: z.string() });",
        "",
        "export default {",
        '  name: "rMultiple",',
        '  description: "Compute the R-multiple for a closed trade entity.",',
        '  classification: "allowed",',
        "  inputSchema,",
        '  example: { tradeId: "ent_trade_1" },',
        "  request: ({ args }) => ({ method: \"GET\", path: `/api/entity/${args.tradeId}` }),",
        "};",
        "",
      ].join("\n");
      await fs.writeFile(path.join(dir, "rMultiple.js"), source, "utf8");
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        structured_output: { tier: "T2", files: ["server/agent/tools/generated/rMultiple.js"], summary: "ok" },
      };
    };

    // No validateFn override: exercises the REAL t2Tool + protectedSurface
    // validators end to end (no mocked "always pass" shortcut).
    const result = await runBuildPipeline("give the AI a tool to compute R-multiple", "ws_test", {
      repoRoot,
      queryFn,
      httpFn: makeHttpFn([]),
    });

    expect(result.success).toBe(true);
    expect(result.nodes[0].status).toBe("completed");
    expect(result.nodes[0].commit).toBeTruthy();
    expect(await mainLogCount()).toBe(beforeCount + 1);
  });
});

describe("runBuildPipeline - T1 node flow with the real t1Render validator mocked to pass", () => {
  it("writes the renderer + registrations and commits the node", async () => {
    const plan = {
      nodes: [{ id: "t1", tier: "T1", params: { kind: "graph" }, description: "show topics as a graph", dependsOn: [] }],
    };
    const beforeCount = await mainLogCount();

    // makeQueryFn's generic T1-T5 branch writes one in-scope renderer file
    // (frontend/src/core/renderers/Generic.jsx) with a { tier, files, summary }
    // structured output - exactly the shape a real T1 build agent produces.
    // The t1Render validator itself is mocked here (validateFn override) so
    // this test exercises the pipeline's build->validate->commit wiring, not
    // a real Playwright boot (that's t1Render.test.js's job).
    const result = await runBuildPipeline("show topics as a graph", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, []),
      httpFn: makeHttpFn([]),
      validateFn: async (tier, worktreePath, node) => {
        expect(tier).toBe("T1");
        expect(node.params).toEqual({ kind: "graph" });
        return { valid: true, errors: [] };
      },
    });

    expect(result.success).toBe(true);
    expect(result.nodes[0].status).toBe("completed");
    expect(result.nodes[0].commit).toBeTruthy();
    expect(await mainLogCount()).toBe(beforeCount + 1);
  });
});
