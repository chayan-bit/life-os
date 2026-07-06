import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { RECOVERY_BUDGET, RETRY_BACKOFF_MS } from "../agent/recovery.js";

// Mirrors test/agent.test.js's fake HTTP layer: records every call, replies
// from a routed script, and defaults to a benign 200/404 so the gate and
// world-snapshot reads never fail on their own.
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

const turnEvents = (httpFn, type) =>
  httpFn.calls.filter((c) => c.method === "POST" && c.path === "/api/event" && c.body?.type === type);

const lastTurn = (httpFn) => turnEvents(httpFn, "agent.turn").at(-1)?.body;

describe("retry with backoff (ladder step 1)", () => {
  it("retries a transient 500 once and succeeds, recording one recovery", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("module=test_probe"),
        reply: (() => {
          let calls = 0;
          return () => {
            calls += 1;
            return calls === 1
              ? { ok: false, status: 500, data: { error: "upstream blip" } }
              : { ok: true, status: 200, data: [{ id: "t1" }] };
          };
        })(),
      },
    ]);
    const sleepFn = vi.fn(async () => {});
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      await options._callTool("entity.list", { module: "test_probe" });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    const result = await runAgentTurn("list my tasks", "ws_test", { queryFn, httpFn, sleepFn });

    expect(result.success).toBe(true);
    expect(sleepFn).toHaveBeenCalledWith(RETRY_BACKOFF_MS);
    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries).toEqual([{ kind: "retry", tool: "entity.list", ok: true }]);
    expect(turn.attrs.tool_calls[0].ok).toBe(true);
  });

  it("does not retry a 400 - it goes straight to argument repair", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "POST" && p === "/api/entity",
        reply: () => ({ ok: false, status: 400, data: { error: "title required" } }),
      },
    ]);
    const sleepFn = vi.fn(async () => {});
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      await options._callTool("entity.create", { module: "tasks", type: "task" });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    const result = await runAgentTurn("create a task", "ws_test", { queryFn, httpFn, sleepFn });

    expect(result.success).toBe(true);
    expect(sleepFn).not.toHaveBeenCalled();
    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries).toEqual([{ kind: "arg_repair", tool: "entity.create", ok: false }]);
  });
});

describe("argument repair (ladder step 2)", () => {
  it("attaches a schema-derived repair_hint and counts it once per tool per turn", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "POST" && p === "/api/entity",
        reply: () => ({ ok: false, status: 422, data: { error: "attrs must be an object" } }),
      },
    ]);
    let capturedHint = null;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      const r1 = await options._callTool("entity.create", { module: "tasks", type: "task" });
      capturedHint = r1;
      await options._callTool("entity.create", { module: "tasks", type: "task" });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("create a task", "ws_test", { queryFn, httpFn });

    expect(String(capturedHint?.repair_hint ?? capturedHint)).toMatch(/expected args/);
    const httpFnCallCount = httpFn.calls.filter((c) => c.method === "POST" && c.path === "/api/entity").length;
    expect(httpFnCallCount).toBe(2);
  });
});

describe("tool substitute (ladder step 3)", () => {
  it("offers a substitute for a read tool once its retry is exhausted", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.startsWith("/api/entity/ent_1"),
        reply: () => ({ ok: false, status: 503, data: { error: "still down" } }),
      },
    ]);
    const sleepFn = vi.fn(async () => {});
    let resultSeen = null;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      resultSeen = await options._callTool("entity.get", { id: "ent_1" });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("get that entity", "ws_test", { queryFn, httpFn, sleepFn });

    expect(String(resultSeen?.substitute_hint ?? resultSeen)).toMatch(/entity\.list/);
    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries.some((r) => r.kind === "substitute" && r.tool === "entity.get")).toBe(true);
  });

  it("never attaches a substitute hint to a write/gated tool", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "POST" && p === "/api/entity",
        reply: () => ({ ok: false, status: 500, data: { error: "down" } }),
      },
    ]);
    const sleepFn = vi.fn(async () => {});
    let resultSeen = null;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      resultSeen = await options._callTool("draft.create", { module: "social", type: "post", attrs: {} });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("draft a post", "ws_test", { queryFn, httpFn, sleepFn });

    expect(resultSeen).not.toHaveProperty("substitute_hint");
  });
});

describe("one replan (ladder step 4)", () => {
  it("replans once on a failed execute, preserves the completed ledger, and re-executes", async () => {
    const httpFn = makeHttp();
    let planCalls = 0;
    let executeCalls = 0;
    let secondPrompt = null;
    const queryFn = vi.fn(async function* ({ prompt, options }) {
      if (options.purpose === "plan") {
        planCalls += 1;
        yield {
          type: "result",
          structured_output: { stages: [{ name: "find", tool: "entity.list", description: "find tasks" }] },
          usage: { input_tokens: 5, output_tokens: 5 },
        };
        return;
      }
      if (options.purpose === "verify") {
        yield { type: "result", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      executeCalls += 1;
      if (executeCalls === 1) {
        await options._callTool("entity.list", { module: "tasks" });
        throw new Error("model dropped mid-turn");
      }
      secondPrompt = prompt;
      yield { type: "result", result: "recovered", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(true);
    expect(planCalls).toBe(2);
    expect(executeCalls).toBe(2);
    expect(secondPrompt).toMatch(/entity\.list/);
    expect(secondPrompt).toMatch(/model dropped mid-turn/);

    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries.some((r) => r.kind === "replan" && r.ok === true)).toBe(true);
  });

  it("never replans twice even if the replanned execute fails again", async () => {
    const httpFn = makeHttp();
    let planCalls = 0;
    let executeCalls = 0;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
        planCalls += 1;
        yield {
          type: "result",
          structured_output: { stages: [{ name: "find", tool: "entity.list", description: "find tasks" }] },
          usage: { input_tokens: 5, output_tokens: 5 },
        };
        return;
      }
      executeCalls += 1;
      throw new Error("still broken");
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("degraded");
    expect(planCalls).toBe(2);
    expect(executeCalls).toBe(2);
  });
});

describe("degrade + escalate (ladder step 5-6)", () => {
  it("degrades honestly with partial work once the recovery budget is exhausted", async () => {
    // A plan exists (a numbered-step prompt forces needsPlanning), so a
    // replan would normally be available - this exercises the budget check
    // specifically, not the "no plan" fallback.
    const httpFn = makeHttp([
      { match: (m, p) => m === "GET" && p.startsWith("/api/entity?"), reply: () => ({ ok: true, status: 200, data: [{ id: "t1" }] }) },
      { match: (m, p) => m === "POST" && p === "/api/entity", reply: () => ({ ok: false, status: 400, data: { error: "bad create" } }) },
      { match: (m, p) => m === "PATCH" && p.startsWith("/api/entity/"), reply: () => ({ ok: false, status: 400, data: { error: "bad update" } }) },
      { match: (m, p) => m === "POST" && p === "/api/edge", reply: () => ({ ok: false, status: 400, data: { error: "bad edge" } }) },
      { match: (m, p) => m === "POST" && p === "/api/configs", reply: () => ({ ok: false, status: 400, data: { error: "bad config" } }) },
    ]);
    const sleepFn = vi.fn(async () => {});
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
        yield {
          type: "result",
          structured_output: { stages: [{ name: "find", tool: "entity.list", description: "find tasks" }] },
          usage: { input_tokens: 5, output_tokens: 5 },
        };
        return;
      }
      // Burns the whole shared budget (RECOVERY_BUDGET = 4) on argument-repair
      // recoveries across four distinct tools, then a genuine unrecoverable
      // network throw with nothing left to spend on replan.
      await options._callTool("entity.list", { module: "tasks" });
      await options._callTool("entity.create", { module: "tasks", type: "t" });
      await options._callTool("entity.update", { id: "ent_x", patch: { status: "urgent" } });
      await options._callTool("edge.create", { src_id: "a", dst_id: "b", rel: "r" });
      await options._callTool("config.draft", { kind: "manual", payload: {} });
      throw new Error("provider unreachable");
    });

    const result = await runAgentTurn("1. list tasks\n2. tag them", "ws_test", { queryFn, httpFn, sleepFn });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("degraded");
    expect(result.text).toMatch(/entity\.list/);
    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries).toHaveLength(RECOVERY_BUDGET);
    expect(turnEvents(httpFn, "agent.escalation")).toHaveLength(1);
  });
});

describe("recovery ladder ordering (#156, group 3 of 3 - not variant-ized)", () => {
  // See the comment on SUBSTITUTES in recovery.js for the full rationale:
  // the retry-before-substitute sequencing is dispatched in executor.js
  // (owned by another worker per #156's concurrency split) and is gated on
  // mutually exclusive HTTP status branches, not a free choice of equally
  // valid orderings - so it is deliberately left out of the strategy
  // optimizer. This test locks in that the current fixed order is unchanged
  // by this issue's work: retry (and its backoff sleep) always fires before
  // the substitute hint is even considered.
  it("always retries (with backoff) before ever attaching a substitute hint, unchanged by #156", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.startsWith("/api/entity/ent_1"),
        reply: () => ({ ok: false, status: 503, data: { error: "still down" } }),
      },
    ]);
    const sleepFn = vi.fn(async () => {});
    let resultSeen = null;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      resultSeen = await options._callTool("entity.get", { id: "ent_1" });
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("get that entity", "ws_test", { queryFn, httpFn, sleepFn });

    expect(sleepFn).toHaveBeenCalledWith(RETRY_BACKOFF_MS);
    expect(String(resultSeen?.substitute_hint ?? resultSeen)).toMatch(/entity\.list/);
    const turn = lastTurn(httpFn);
    // The ledger is append-only in call order, so this array's order IS the
    // ladder's execution order: the "retry" entry (recorded unconditionally
    // by executor.js's httpWithRetry, win or lose) is pushed before the
    // "substitute" entry (recorded only once retry's own attempt is spent) -
    // proving retry always precedes substitute, exactly as it did before #156.
    expect(turn.attrs.recoveries).toEqual([
      { kind: "retry", tool: "entity.get", ok: false },
      { kind: "substitute", tool: "entity.get", ok: false },
    ]);
  });
});

describe("circuit breaker", () => {
  it("opens after two consecutive queryFn throws and stops calling the model", async () => {
    const httpFn = makeHttp();
    let callCount = 0;
    const queryFn = vi.fn(async function* ({ options }) {
      callCount += 1;
      if (options.purpose === "plan") {
        if (callCount === 1) {
          yield {
            type: "result",
            structured_output: { stages: [{ name: "find", tool: "entity.list", description: "find tasks" }] },
            usage: { input_tokens: 5, output_tokens: 5 },
          };
          return;
        }
        throw new Error("provider down (plan)");
      }
      throw new Error("provider down (execute)");
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("degraded");
    // plan (ok) + execute (throw #1) + replan's plan call (throw #2) = 3 real
    // calls; the breaker trips there so no further call (a replanned execute)
    // ever reaches the mock.
    expect(callCount).toBe(3);
    expect(turnEvents(httpFn, "agent.escalation")).toHaveLength(1);
    expect(turnEvents(httpFn, "agent.escalation")[0].body.attrs.breaker_open).toBe(true);
  });
});

describe("recovery budget", () => {
  it("caps total recoveries at RECOVERY_BUDGET across mixed failure kinds", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "POST" && p === "/api/entity",
        reply: () => ({ ok: false, status: 400, data: { error: "bad args" } }),
      },
    ]);
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose !== "execute") {
        yield { type: "result", result: "ok", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      for (let i = 0; i < RECOVERY_BUDGET + 3; i += 1) {
        await options._callTool("entity.create", { module: "tasks", type: `task_${i}` });
      }
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("create several tasks", "ws_test", { queryFn, httpFn });

    const turn = lastTurn(httpFn);
    expect(turn.attrs.recoveries.length).toBeLessThanOrEqual(RECOVERY_BUDGET);
  });
});
