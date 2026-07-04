import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { classify, PROTECTED_TOOLS } from "../agent/actionRegistry.js";
import { needsPlanning } from "../agent/planner.js";
import { MAX_STEPS } from "../agent/executor.js";

// A fake HTTP layer: records every call and returns canned responses so no
// turn ever touches the network (mirrors the injectable-httpFn contract).
function makeHttp(routes = []) {
  const calls = [];
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    for (const route of routes) {
      if (route.match(method, path)) return route.reply(method, path, body);
    }
    // Absent agent config -> 404 (gate treats this as "use defaults").
    if (method === "GET" && path.includes("/api/entity/agent_config_")) {
      return { ok: false, status: 404, data: null };
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

// A mock Agent SDK queryFn driven by a per-role script. `execute` walks the
// scripted tool calls through the executor's own chokepoint (options._callTool),
// exactly as the real in-process SDK would dispatch them.
function makeQueryFn(script = {}) {
  return vi.fn(async function* ({ options }) {
    if (options.purpose === "plan") {
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        structured_output: script.plan ?? { stages: [{ name: "step", tool: null, description: "do it" }] },
        usage: { input_tokens: 5, output_tokens: 5 },
      };
      return;
    }
    if (options.purpose === "verify") {
      yield {
        type: "result",
        subtype: "success",
        is_error: false,
        structured_output: script.verify ?? { ok: true, issue: null, fixable: false },
        usage: { input_tokens: 3, output_tokens: 3 },
      };
      return;
    }
    for (const call of script.toolCalls ?? []) {
      await options._callTool(call.tool, call.args ?? {});
    }
    yield {
      type: "result",
      subtype: "success",
      is_error: false,
      result: script.text ?? "done",
      usage: { input_tokens: 10, output_tokens: 8 },
    };
  });
}

const turnEvents = (httpFn, type) =>
  httpFn.calls.filter((c) => c.method === "POST" && c.path === "/api/event" && c.body?.type === type);

describe("actionRegistry.classify", () => {
  it("classifies protected + unknown names as forbidden, others by registry", () => {
    for (const name of PROTECTED_TOOLS) expect(classify(name)).toBe("forbidden");
    expect(classify("totally.unknown")).toBe("forbidden");
    expect(classify("entity.create")).toBe("allowed");
    expect(classify("draft.create")).toBe("gated");
  });

  it("registers no trading/order tool of any kind", () => {
    for (const name of ["order.place", "order.modify", "trade.execute", "broker.order"]) {
      expect(classify(name)).toBe("forbidden");
    }
  });
});

describe("needsPlanning heuristic", () => {
  it("is true for a multi-imperative request and false for a single ask", () => {
    expect(needsPlanning("find my overdue tasks, tag them urgent, and draft a summary")).toBe(true);
    expect(needsPlanning("what is the CAP theorem?")).toBe(false);
    expect(needsPlanning("delete the vcs history")).toBe(false);
  });
});

describe("runAgentTurn - happy multi-step path", () => {
  it("plans, persists the DAG, executes >=2 tools, verifies, records one agent.turn", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      plan: {
        stages: [
          { name: "find", tool: "entity.list", description: "find overdue tasks" },
          { name: "tag", tool: "entity.update", description: "tag them urgent" },
        ],
      },
      toolCalls: [
        { tool: "entity.list", args: { module: "tasks", status: "overdue" } },
        { tool: "entity.update", args: { id: "ent_x", patch: { status: "urgent" } } },
      ],
      text: "Tagged 2 tasks urgent.",
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");

    // The plan was persisted as a pipeline_run entity (reused DAG shape).
    const planPost = httpFn.calls.find(
      (c) => c.method === "POST" && c.path === "/api/entity" && c.body?.module === "pipelines",
    );
    expect(planPost).toBeTruthy();
    expect(planPost.body.type).toBe("pipeline_run");
    expect(planPost.body.attrs.stages).toHaveLength(2);

    // Both tool calls actually rode existing routes.
    expect(httpFn.calls.some((c) => c.method === "GET" && c.path.startsWith("/api/entity?"))).toBe(true);
    expect(httpFn.calls.some((c) => c.method === "PATCH" && c.path.startsWith("/api/entity/ent_x"))).toBe(true);

    // Exactly one agent.turn flight-recorder row with a 2-entry ledger.
    const turns = turnEvents(httpFn, "agent.turn");
    expect(turns).toHaveLength(1);
    expect(turns[0].body.attrs.tool_calls).toHaveLength(2);
    expect(turns[0].body.attrs.tool_calls.every((t) => t.decision === "allowed" && t.ok === true)).toBe(true);
  });
});

describe("runAgentTurn - forbidden refusal", () => {
  it("refuses a protected tool visibly, logs action.denied, performs no mutation", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      toolCalls: [{ tool: "vcs.deleteVersion", args: { id: "v1" } }],
      text: "cannot do that",
    });

    const result = await runAgentTurn("delete the vcs history", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(turnEvents(httpFn, "action.denied")).toHaveLength(1);

    // No entity mutation happened (no POST /api/entity at all this turn).
    expect(httpFn.calls.some((c) => c.method === "POST" && c.path === "/api/entity")).toBe(false);

    const ledger = turnEvents(httpFn, "agent.turn")[0].body.attrs.tool_calls;
    expect(ledger).toEqual([expect.objectContaining({ tool: "vcs.deleteVersion", decision: "forbidden", ok: false })]);
  });
});

describe("runAgentTurn - agent.turn Observe stamps (#125)", () => {
  it("stamps tier/tokens_in/tokens_out/gated/error on a completed deliberate turn, marked as an eval boundary", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      plan: { stages: [{ name: "find", tool: "entity.list", description: "find overdue tasks" }] },
      toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }],
      text: "done",
    });

    await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", { queryFn, httpFn });

    const turn = turnEvents(httpFn, "agent.turn")[0].body;
    expect(turn.tier).toBe("mac");
    expect(turn.tokens_in).toBeGreaterThan(0);
    expect(turn.tokens_out).toBeGreaterThan(0);
    expect(turn.gated).toBe(0);
    expect(turn.error).toBeNull();
    // A deliberate (planned) completed turn is the natural eval boundary.
    expect(turn.attrs.stage).toBe("eval");
  });

  it("stamps gated:1 on a turn that ends awaiting_approval, with no eval-boundary stamp", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      toolCalls: [{ tool: "draft.create", args: { module: "social", type: "post", attrs: { text: "launch!" } } }],
      text: "drafted",
    });

    await runAgentTurn("draft a tweet about the launch", "ws_test", { queryFn, httpFn });

    const turn = turnEvents(httpFn, "agent.turn")[0].body;
    expect(turn.gated).toBe(1);
    expect(turn.outcome).toBe("awaiting_approval");
    expect(turn.attrs.stage).toBeNull();
  });

  it("writes exactly one agent.turn event even when the refine round fires", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      verify: { ok: false, issue: "too short", fixable: true },
      text: "revised answer",
    });

    const result = await runAgentTurn("what is the CAP theorem?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    const turns = turnEvents(httpFn, "agent.turn");
    expect(turns).toHaveLength(1);
    expect(turns[0].body.attrs.refined).toBe(true);
  });
});

describe("runAgentTurn - gated enqueue", () => {
  it("writes a pending_approval draft and does not execute the outward effect", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({
      toolCalls: [{ tool: "draft.create", args: { module: "social", type: "post", attrs: { text: "launch!" } } }],
      text: "drafted",
    });

    const result = await runAgentTurn("draft a tweet about the launch", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("awaiting_approval");
    expect(result.pendingApprovals).toHaveLength(1);

    const draftPost = httpFn.calls.find(
      (c) => c.method === "POST" && c.path === "/api/entity" && c.body?.module === "social",
    );
    expect(draftPost).toBeTruthy();
    expect(draftPost.body.status).toBe("pending_approval");

    // Gated turns skip the verify pass (nothing to verify until approval).
    expect(queryFn.mock.calls.every(([{ options }]) => options.purpose !== "verify")).toBe(true);
  });
});

describe("runAgentTurn - step budget exhaustion", () => {
  it("halts at MAX_STEPS and escalates rather than looping", async () => {
    const httpFn = makeHttp();
    const overrun = Array.from({ length: MAX_STEPS + 2 }, () => ({
      tool: "entity.list",
      args: { module: "tasks" },
    }));
    const queryFn = makeQueryFn({ toolCalls: overrun, text: "kept going" });

    const result = await runAgentTurn("list everything", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("step_budget_exhausted");
    expect(turnEvents(httpFn, "agent.step_budget_exhausted")).toHaveLength(1);
  });
});

describe("runAgentTurn - gate fails closed", () => {
  it("refuses gate_unavailable and never calls the model when config fetch throws", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("/api/entity/agent_config_"),
        reply: () => {
          throw new Error("connection refused");
        },
      },
    ]);
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list" }] });

    const result = await runAgentTurn("do anything", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("gate_unavailable");
    expect(queryFn).not.toHaveBeenCalled();
  });
});

describe("runAgentTurn - kill switch", () => {
  it("refuses when the kill switch is on and never calls the model", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("/api/entity/agent_config_"),
        reply: () => ({ ok: true, status: 200, data: { attrs: { killSwitch: true } } }),
      },
    ]);
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list" }] });

    const result = await runAgentTurn("do anything", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("kill_switch");
    expect(queryFn).not.toHaveBeenCalled();
  });
});
