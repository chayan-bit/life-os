import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runBuildPipeline } from "../build/pipeline.js";
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
      nodes: [{ id: "r", tier: "T3", params: { crate: "lifeos-x" }, description: "weekly summary route", dependsOn: [] }],
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

describe("runBuildPipeline - T1 validator placeholder fails closed", () => {
  it("fails a T1 node against the not-yet-implemented registry validator (until #133)", async () => {
    const plan = {
      nodes: [{ id: "t1", tier: "T1", params: { kind: "board" }, description: "board renderer", dependsOn: [] }],
    };
    const beforeCount = await mainLogCount();

    // No validateFn injected: the real registry placeholder for T1 runs and
    // rejects, so the node fails and nothing ships.
    const result = await runBuildPipeline("show topics as a board", "ws_test", {
      repoRoot,
      queryFn: makeQueryFn(plan, []),
      httpFn: makeHttpFn([]),
    });

    expect(result.success).toBe(false);
    expect(result.nodes[0].status).toBe("failed");
    expect(result.nodes[0].reason).toMatch(/not yet implemented for T1/);
    expect(await mainLogCount()).toBe(beforeCount);
  });
});
