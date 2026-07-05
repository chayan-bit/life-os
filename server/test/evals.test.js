import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import {
  appendHistory,
  exitCodeForRate,
  loadScenarios,
  PASS_THRESHOLD,
  runEval,
  runScenario,
  ScenariosSchema,
  scoreScenario,
} from "../evals/run.js";

// Mirrors agent.test.js's fake HTTP layer: records every call, canned
// responses so no turn ever touches the network. Used to prove dry-run makes
// ZERO write calls while still returning read data the loop needs.
function makeHttp(routes = []) {
  const calls = [];
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    for (const route of routes) {
      if (route.match(method, path)) return route.reply(method, path, body);
    }
    if (method === "GET" && path.includes("/api/entity/agent_config_")) {
      return { ok: false, status: 404, data: null };
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

// /api/memory/context is a POST-shaped READ (the context compiler query, no
// mutation - see memoryContext.js) - excluded here so this helper reflects
// actual domain writes (entity/event/configs), not HTTP verb alone.
const READ_ONLY_POST_PATHS = new Set(["/api/memory/context"]);
const writeCalls = (httpFn) =>
  httpFn.calls.filter((c) => (c.method === "POST" || c.method === "PATCH") && !READ_ONLY_POST_PATHS.has(c.path));

// A mock Agent SDK queryFn that walks scripted tool calls through the
// executor's own chokepoint (options._callTool), same shape as agent.test.js.
function makeQueryFn(script = {}) {
  return vi.fn(async function* ({ options }) {
    if (options.purpose === "plan") {
      yield {
        type: "result",
        structured_output: script.plan ?? { stages: [{ name: "step", tool: null, description: "do it" }] },
        usage: { input_tokens: 5, output_tokens: 5 },
      };
      return;
    }
    if (options.purpose === "verify") {
      yield {
        type: "result",
        structured_output: script.verify ?? { ok: true, issue: null, fixable: false, confidence: 1 },
        usage: { input_tokens: 3, output_tokens: 3 },
      };
      return;
    }
    for (const call of script.toolCalls ?? []) {
      await options._callTool(call.tool, call.args ?? {});
    }
    yield { type: "result", result: script.text ?? "done", usage: { input_tokens: 10, output_tokens: 8 } };
  });
}

describe("runAgentTurn - dry-run mode (issue #140)", () => {
  it("performs zero write httpFn calls for a turn with tool calls, while the ledger records them", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }] });

    const result = await runAgentTurn("show me my tasks", "ws_test", { httpFn, queryFn, dryRun: true });

    expect(result.success).toBe(true);
    expect(writeCalls(httpFn)).toHaveLength(0);
    expect(result.ledger).toEqual([{ tool: "entity.list", decision: "allowed", ms: expect.any(Number), ok: true }]);
  });

  it("records a gated tool call without enqueuing - no draft entity is created", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      toolCalls: [{ tool: "draft.create", args: { title: "announcement" } }],
    });

    const result = await runAgentTurn("draft a tweet", "ws_test", { httpFn, queryFn, dryRun: true });

    expect(writeCalls(httpFn)).toHaveLength(0);
    expect(result.ledger).toEqual([{ tool: "draft.create", decision: "gated", ms: expect.any(Number), ok: true }]);
    // Nothing pending - dry-run never actually drafts, so there is nothing
    // for a human to approve.
    expect(result.pendingApprovals).toBeUndefined();
  });

  it("records a forbidden tool call without writing an action.denied event", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "secret.read", args: {} }] });

    const result = await runAgentTurn("read a secret", "ws_test", { httpFn, queryFn, dryRun: true });

    expect(writeCalls(httpFn)).toHaveLength(0);
    expect(result.ledger.some((e) => e.tool === "secret.read" && e.decision === "forbidden")).toBe(true);
  });

  it("never persists the turn trace, memory ingest, or a lesson in dry-run", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: {} }] });

    await runAgentTurn("never do this again, always use tasks", "ws_test", { httpFn, queryFn, dryRun: true });

    expect(httpFn.calls.some((c) => c.path === "/api/event" && c.body?.type === "agent.turn")).toBe(false);
    expect(httpFn.calls.some((c) => c.path === "/api/memory/ingest")).toBe(false);
    expect(httpFn.calls.some((c) => c.path === "/api/event" && c.body?.type === "feedback.given")).toBe(false);
  });

  it("a normal (non-dry-run) turn still performs its usual writes - dry-run is opt-in only", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: {} }] });

    await runAgentTurn("show me my tasks", "ws_test", { httpFn, queryFn });

    expect(httpFn.calls.some((c) => c.path === "/api/event" && c.body?.type === "agent.turn")).toBe(true);
  });
});

describe("scoreScenario", () => {
  const scenario = { id: "s1", prompt: "p", expect_any: ["entity.list"], forbid: ["draft.create"] };

  it("passes when an expected tool was called and no forbidden tool was", () => {
    const result = scoreScenario(scenario, [{ tool: "entity.list", ok: true }]);
    expect(result).toEqual({ passed: true, matchedTools: ["entity.list"], forbiddenHit: null });
  });

  it("fails when a forbidden tool was called, even alongside an expected one", () => {
    const result = scoreScenario(scenario, [
      { tool: "entity.list", ok: true },
      { tool: "draft.create", ok: true },
    ]);
    expect(result.passed).toBe(false);
    expect(result.forbiddenHit).toBe("draft.create");
  });

  it("fails when no expected tool was called", () => {
    const result = scoreScenario(scenario, [{ tool: "search.query", ok: true }]);
    expect(result.passed).toBe(false);
    expect(result.matchedTools).toEqual([]);
  });

  it("an empty expect_any passes on a refusal (no forbidden tool called)", () => {
    const refusalScenario = { id: "s2", prompt: "p", expect_any: [], forbid: ["order.place"] };
    expect(scoreScenario(refusalScenario, []).passed).toBe(true);
    expect(scoreScenario(refusalScenario, [{ tool: "order.place" }]).passed).toBe(false);
  });
});

describe("runScenario + runEval", () => {
  it("runs a scenario through the real loop in dry-run and scores it", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: {} }] });
    const scenario = { id: "list", prompt: "show my tasks", expect_any: ["entity.list"], forbid: [] };

    const result = await runScenario(scenario, { httpFn, queryFn });

    expect(result).toEqual({ id: "list", passed: true, matchedTools: ["entity.list"], forbiddenHit: null, ledger: expect.any(Array) });
    expect(writeCalls(httpFn)).toHaveLength(0);
  });

  it("aggregates a pass rate and lists failing scenario ids", async () => {
    const httpFn = makeHttp();
    const scenarios = [
      { id: "pass1", prompt: "p1", expect_any: ["entity.list"], forbid: [] },
      { id: "fail1", prompt: "p2", expect_any: ["memory.network"], forbid: [] },
    ];
    const queryFnFor = (toolName) => makeQueryFn({ toolCalls: [{ tool: toolName, args: {} }] });
    const queryFn = vi.fn(async function* (req) {
      const wanted = req.prompt.includes("p1") ? "entity.list" : "search.query";
      yield* queryFnFor(wanted)(req);
    });

    const summary = await runEval(scenarios, { httpFn, queryFn });

    expect(summary.total).toBe(2);
    expect(summary.passed).toBe(1);
    expect(summary.rate).toBe(0.5);
    expect(summary.failures).toEqual(["fail1"]);
  });

  it("scores a scenario that throws as a failure instead of crashing the suite", async () => {
    const httpFn = makeHttp();
    const queryFn = vi.fn(async function* () {
      throw new Error("model unavailable");
    });
    const scenarios = [{ id: "boom", prompt: "p", expect_any: ["entity.list"], forbid: [] }];

    const summary = await runEval(scenarios, { httpFn, queryFn });

    expect(summary.total).toBe(1);
    expect(summary.passed).toBe(0);
    expect(summary.failures).toEqual(["boom"]);
  });
});

describe("appendHistory", () => {
  let tmpFile;

  afterEach(() => {
    if (tmpFile && fs.existsSync(tmpFile)) fs.unlinkSync(tmpFile);
  });

  it("appends one JSON line with the ts/total/passed/rate/failures shape", () => {
    tmpFile = path.join(os.tmpdir(), `strategy-eval-history-${Date.now()}.jsonl`);
    const summary = { total: 4, passed: 3, rate: 0.75, failures: ["s4"] };

    const record = appendHistory(tmpFile, summary);

    const lines = fs.readFileSync(tmpFile, "utf8").trim().split("\n");
    expect(lines).toHaveLength(1);
    const parsed = JSON.parse(lines[0]);
    expect(parsed).toEqual({ ts: record.ts, total: 4, passed: 3, rate: 0.75, failures: ["s4"] });
  });

  it("appends without truncating on a second call", () => {
    tmpFile = path.join(os.tmpdir(), `strategy-eval-history-${Date.now()}-2.jsonl`);
    appendHistory(tmpFile, { total: 1, passed: 1, rate: 1, failures: [] });
    appendHistory(tmpFile, { total: 1, passed: 0, rate: 0, failures: ["x"] });

    const lines = fs.readFileSync(tmpFile, "utf8").trim().split("\n");
    expect(lines).toHaveLength(2);
  });
});

describe("exitCodeForRate", () => {
  it("is 0 (pass) at or above the threshold", () => {
    expect(exitCodeForRate(PASS_THRESHOLD)).toBe(0);
    expect(exitCodeForRate(1)).toBe(0);
  });

  it("is 1 (fail) below the threshold", () => {
    expect(exitCodeForRate(PASS_THRESHOLD - 0.01)).toBe(1);
    expect(exitCodeForRate(0)).toBe(1);
  });
});

describe("scenarios.json", () => {
  it("validates against the scenario schema and has at least one scenario", () => {
    const scenarios = loadScenarios();
    expect(ScenariosSchema.safeParse(scenarios).success).toBe(true);
    expect(scenarios.length).toBeGreaterThan(0);
  });

  it("every scenario has a unique id and at least one of expect_any/forbid populated", () => {
    const scenarios = loadScenarios();
    const ids = scenarios.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const scenario of scenarios) {
      expect(scenario.expect_any.length + scenario.forbid.length).toBeGreaterThan(0);
    }
  });

  it("rejects a malformed scenarios file at load time", () => {
    const badPath = path.join(os.tmpdir(), `bad-scenarios-${Date.now()}.json`);
    fs.writeFileSync(badPath, JSON.stringify([{ id: "x" }]));

    expect(() => loadScenarios(badPath)).toThrow(/invalid scenarios file/);

    fs.unlinkSync(badPath);
  });
});
