// Resume-on-approval integration test (issue #142, closes the #132/#135 gap).
// A T3 node gates on the first run and its dependent is skipped; after a human
// approves the gate, resumeBuildPipeline re-enters the DAG, commits the approved
// node, and completes the previously-skipped dependent - end to end against a
// real git repo, with a stateful in-memory httpFn standing in for lifeos-api.
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runBuildPipeline } from "../build/pipeline.js";
import { resumeBuildPipeline } from "../build/resume.js";

const execFile = promisify(execFileCb);
let repoRoot;

async function git(args, cwd = repoRoot) {
  return execFile("git", args, { cwd });
}

async function mainLogCount() {
  const { stdout } = await git(["log", "--oneline", "main"]);
  return stdout.split("\n").filter(Boolean).length;
}

beforeEach(async () => {
  repoRoot = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-resume-repo-"));
  await git(["init", "-b", "main"]);
  await git(["config", "user.email", "test@example.com"]);
  await git(["config", "user.name", "Test"]);
  await fs.writeFile(path.join(repoRoot, "seed.txt"), "seed\n", "utf8");
  await git(["add", "."]);
  await git(["commit", "-m", "seed"]);
});

afterEach(async () => {
  await fs.rm(repoRoot, { recursive: true, force: true });
});

const SPEC = { summary: "s", entities: ["item"], views: ["list"], tools: [], routes: [], migrations: [] };

// A stateful entity store: POST creates + returns an id, PATCH mutates in place,
// GET reads back (with attrs as a live object). Mirrors just enough of
// lifeos-api for persistBuildRun / gate.js / resume.js to round-trip.
function makeStore() {
  const entities = new Map();
  let seq = 0;
  const httpFn = async (method, reqPath, body) => {
    if (method === "POST" && reqPath === "/api/entity") {
      const id = `ent_${++seq}`;
      entities.set(id, {
        id,
        module: body.module,
        type: body.type,
        title: body.title ?? null,
        status: body.status ?? null,
        attrs: body.attrs ?? {},
      });
      return { ok: true, data: { id } };
    }
    if (method === "PATCH" && reqPath.startsWith("/api/entity/")) {
      const id = reqPath.slice("/api/entity/".length);
      const e = entities.get(id);
      if (e) {
        if (body.status !== undefined) e.status = body.status;
        if (body.attrs !== undefined) e.attrs = body.attrs;
      }
      return { ok: true, data: { id } };
    }
    if (method === "GET" && reqPath.startsWith("/api/entity/")) {
      const id = reqPath.slice("/api/entity/".length);
      const e = entities.get(id);
      return e ? { ok: true, data: e } : { ok: false, status: 404, data: null };
    }
    if (reqPath === "/api/event") return { ok: true, data: { id: `evt_${++seq}` } };
    return { ok: true, data: {} };
  };
  return { entities, httpFn };
}

// A queryFn that writes a UNIQUE in-scope file per build node (so each node has
// a real, distinct diff to commit) and returns the standard tier summary.
function makeQueryFn(plan) {
  let buildSeq = 0;
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
    const n = ++buildSeq;
    const target = path.join(params.options.cwd, "frontend", "src", "core", "renderers", `Generic${n}.jsx`);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, `export default function Generic${n}() { return null; }\n`, "utf8");
    yield {
      type: "result",
      subtype: "success",
      is_error: false,
      structured_output: { tier: "T1", files: [`Generic${n}.jsx`], summary: "ok" },
    };
  };
}

describe("resumeBuildPipeline - a halted T3 build resumes after approval", () => {
  it("gates the T3 node (skipping its dependent), then on approval commits both", async () => {
    const plan = {
      nodes: [
        { id: "r", tier: "T3", params: { crate: "lifeos-x", name: "week" }, description: "weekly route", dependsOn: [] },
        { id: "d", tier: "T1", params: { kind: "board" }, description: "board view", dependsOn: ["r"] },
      ],
    };
    const store = makeStore();
    const opts = {
      repoRoot,
      queryFn: makeQueryFn(plan),
      httpFn: store.httpFn,
      validateFn: async () => ({ valid: true, errors: [] }),
    };
    const before = await mainLogCount();

    // First run: r gates (awaiting_approval), d skipped, nothing committed.
    const first = await runBuildPipeline("weekly summary feature", "ws_test", opts);
    expect(first.success).toBe(false);
    const byId1 = Object.fromEntries(first.nodes.map((n) => [n.id, n]));
    expect(byId1.r.status).toBe("awaiting_approval");
    expect(byId1.d.status).toBe("skipped");
    expect(await mainLogCount()).toBe(before);

    // The human approves the gate (flip the pending_approval entity to approved).
    const approval = [...store.entities.values()].find((e) => e.type === "pending_approval");
    expect(approval).toBeTruthy();
    expect(approval.attrs.node).toBe("r");
    expect(approval.attrs.pipeline_run_entity_id).toBeTruthy();
    approval.status = "approved";

    // Resume: r builds + commits, d builds + commits. Both completed.
    const resumed = await resumeBuildPipeline(approval.id, "ws_test", opts);
    expect(resumed.success).toBe(true);
    const byId2 = Object.fromEntries(resumed.nodes.map((n) => [n.id, n]));
    expect(byId2.r.status).toBe("completed");
    expect(byId2.r.commit).toBeTruthy();
    expect(byId2.d.status).toBe("completed");
    expect(byId2.d.commit).toBeTruthy();

    // Two new commits landed on main; no leftover worktrees.
    expect(await mainLogCount()).toBe(before + 2);
    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });

  it("refuses to resume an approval entity that is not approved", async () => {
    const store = makeStore();
    // Seed a still-pending gate directly.
    const { data } = await store.httpFn("POST", "/api/entity", {
      module: "pipelines",
      type: "pending_approval",
      status: "awaiting_approval",
      attrs: { node: "r", pipeline_run_entity_id: "ent_run", run_id: "build_x" },
    });
    const result = await resumeBuildPipeline(data.id, "ws_test", { repoRoot, httpFn: store.httpFn });
    expect(result.success).toBe(false);
    expect(result.error).toMatch(/not approved/);
  });
});
