// End-to-end integration tests for T2 self-authored tools (issue #134): a
// real tool file is dropped into the REAL server/agent/tools/generated/ dir
// (cleaned up in afterEach), the module graph is reset so actionRegistry.js's
// top-level merge re-runs, then the merged REGISTRY/classify/executor
// chokepoint are exercised exactly as production would. Complements
// generatedToolsLoader.test.js (pure loader unit tests against a fixture dir)
// and t2Tool.test.js (the build-time validator) - this file proves the wiring
// between the three actually holds together.
import fs from "node:fs/promises";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const GENERATED_DIR = path.resolve(import.meta.dirname, "..", "agent", "tools", "generated");

const READ_TOOL_SOURCE = [
  'import { z } from "zod";',
  "",
  "const inputSchema = z.object({ tradeId: z.string() });",
  "",
  "export default {",
  '  name: "z_test_read",',
  '  description: "Test-only read tool over a trade entity by id.",',
  '  classification: "allowed",',
  "  inputSchema,",
  '  example: { tradeId: "ent_trade_1" },',
  "  request: ({ args }) => ({ method: \"GET\", path: `/api/entity/${args.tradeId}` }),",
  "};",
  "",
].join("\n");

const GATED_TOOL_SOURCE = [
  'import { z } from "zod";',
  "",
  "const inputSchema = z.object({ text: z.string() });",
  "",
  "export default {",
  '  name: "z_test_gated",',
  '  description: "Test-only gated draft tool.",',
  '  classification: "gated",',
  "  inputSchema,",
  '  example: { text: "hello" },',
  '  request: ({ args }) => ({ method: "POST", path: "/api/entity", body: { attrs: args } }),',
  "};",
  "",
].join("\n");

// A dynamic-route tool: the path it hits depends entirely on the CALLER'S
// args, not the fixed `example` used at load time. This is what proves the
// executor re-validates isRouteAllowed at call time rather than trusting the
// load-time dry-run - a real generated tool could be written this way (e.g. a
// generic "read any allowed entity" tool), and the model's own args are what
// must be checked, not the author's example.
const DYNAMIC_ROUTE_TOOL_SOURCE = [
  'import { z } from "zod";',
  "",
  "const inputSchema = z.object({ path: z.string() });",
  "",
  "export default {",
  '  name: "z_test_dynamic",',
  '  description: "Test-only tool whose route depends on caller-supplied args.",',
  '  classification: "allowed",',
  "  inputSchema,",
  '  example: { path: "/api/entity" },',
  "  request: ({ args }) => ({ method: \"GET\", path: args.path }),",
  "};",
  "",
].join("\n");

async function writeGeneratedFile(name, source) {
  await fs.writeFile(path.join(GENERATED_DIR, name), source, "utf8");
}

async function removeGeneratedFile(name) {
  await fs.rm(path.join(GENERATED_DIR, name), { force: true });
}

// Fresh imports of the two modules under test after a filesystem change, so
// actionRegistry.js's top-level `loadGeneratedTools()` re-runs against the
// current directory contents.
async function freshImports() {
  vi.resetModules();
  const actionRegistry = await import("../agent/actionRegistry.js");
  const executor = await import("../agent/executor.js");
  return { actionRegistry, executor };
}

const WRITTEN_FILES = ["z_test_read.js", "z_test_gated.js", "z_test_dynamic.js"];

afterEach(async () => {
  await Promise.all(WRITTEN_FILES.map(removeGeneratedFile));
  vi.resetModules();
});

describe("generated tools merge into the real REGISTRY and are classify()-able", () => {
  beforeEach(async () => {
    await writeGeneratedFile("z_test_read.js", READ_TOOL_SOURCE);
    await writeGeneratedFile("z_test_gated.js", GATED_TOOL_SOURCE);
  });

  it("REGISTRY contains both generated tools with the right classification, and classify() agrees", async () => {
    const { actionRegistry } = await freshImports();

    expect(actionRegistry.REGISTRY.z_test_read).toBeTruthy();
    expect(actionRegistry.REGISTRY.z_test_read.classification).toBe("allowed");
    expect(actionRegistry.classify("z_test_read")).toBe("allowed");

    expect(actionRegistry.REGISTRY.z_test_gated).toBeTruthy();
    expect(actionRegistry.REGISTRY.z_test_gated.classification).toBe("gated");
    expect(actionRegistry.classify("z_test_gated")).toBe("gated");

    // Hand-written tools are untouched.
    expect(actionRegistry.classify("entity.create")).toBe("allowed");
  });
});

describe("executor chokepoint - a generated 'allowed' tool executes through the real HTTP chokepoint", () => {
  beforeEach(async () => {
    await writeGeneratedFile("z_test_read.js", READ_TOOL_SOURCE);
  });

  it("calls httpFn via the descriptor's own request() and records a ledger entry", async () => {
    const { executor } = await freshImports();
    const httpCalls = [];
    const httpFn = vi.fn(async (method, path_) => {
      httpCalls.push({ method, path: path_ });
      return { ok: true, status: 200, data: { id: "ent_trade_1", attrs: { pnl: 200, risk: 100 } } };
    });
    const ctx = { httpFn, workspaceId: "ws_test", ledger: [], stepCount: 0, pendingApprovals: [] };

    const result = await executor.runTool(ctx, "z_test_read", { tradeId: "ent_trade_1" });

    expect(result.status).toBe("ok");
    expect(httpCalls).toEqual([{ method: "GET", path: "/api/entity/ent_trade_1" }]);
    expect(ctx.ledger).toHaveLength(1);
    expect(ctx.ledger[0]).toMatchObject({ tool: "z_test_read", decision: "allowed", ok: true });
  });
});

describe("executor chokepoint - a generated 'gated' tool enqueues approval and never executes", () => {
  beforeEach(async () => {
    await writeGeneratedFile("z_test_gated.js", GATED_TOOL_SOURCE);
  });

  it("writes a pending_approval draft entity only - the descriptor's own request() is never called", async () => {
    const { executor } = await freshImports();
    const httpCalls = [];
    const httpFn = vi.fn(async (method, path_, body) => {
      httpCalls.push({ method, path: path_, body });
      return { ok: true, status: 200, data: { id: "ent_draft_1" } };
    });
    const ctx = { httpFn, workspaceId: "ws_test", ledger: [], stepCount: 0, pendingApprovals: [] };

    const result = await executor.runTool(ctx, "z_test_gated", { text: "hello" });

    expect(result.status).toBe("pending_approval");
    expect(ctx.pendingApprovals).toHaveLength(1);
    // Exactly one HTTP call - the draft write - not the descriptor's own route.
    expect(httpCalls).toHaveLength(1);
    expect(httpCalls[0]).toMatchObject({ method: "POST", path: "/api/entity" });
    expect(httpCalls[0].body.status).toBe("pending_approval");
    expect(ctx.ledger[0]).toMatchObject({ tool: "z_test_gated", decision: "gated" });
  });
});

describe("executor chokepoint - re-validates isRouteAllowed at call time, never trusts load time alone", () => {
  beforeEach(async () => {
    await writeGeneratedFile("z_test_dynamic.js", DYNAMIC_ROUTE_TOOL_SOURCE);
  });

  it("allows a call whose runtime args resolve to an allowed route", async () => {
    const { executor } = await freshImports();
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: [] }));
    const ctx = { httpFn, workspaceId: "ws_test", ledger: [], stepCount: 0, pendingApprovals: [] };

    const result = await executor.runTool(ctx, "z_test_dynamic", { path: "/api/entity" });

    expect(result.status).toBe("ok");
    expect(httpFn).toHaveBeenCalledWith("GET", "/api/entity", undefined);
  });

  it("refuses a call whose runtime args resolve to a forbidden route, even though load-time's example was fine", async () => {
    const { executor } = await freshImports();
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: [] }));
    const ctx = { httpFn, workspaceId: "ws_test", ledger: [], stepCount: 0, pendingApprovals: [] };

    // The tool loaded fine (its `example` targets /api/entity), but the
    // model's actual call now points at a forbidden path - the executor must
    // catch this itself, not rely on the one-time load check.
    const result = await executor.runTool(ctx, "z_test_dynamic", { path: "/api/configs" });

    expect(result.status).toBe("forbidden");
    expect(httpFn).not.toHaveBeenCalled();
    expect(ctx.ledger[0]).toMatchObject({ tool: "z_test_dynamic", decision: "allowed", ok: false });
  });
});
