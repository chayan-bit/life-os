import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { fetchMemoryContext, ingestTurnOutcome } from "../agent/memoryContext.js";

// Mirrors agent.test.js's fake HTTP layer: records every call, replies via an
// optional route list, and defaults reads/writes to a canned success so no
// turn ever touches the network.
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
    if (path === "/api/memory/context") {
      return { ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } };
    }
    if (path === "/api/memory/ingest") {
      return { ok: true, status: 200, data: { event_id: "evt_1" } };
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

// A mock Agent SDK queryFn that captures every prompt it is asked to run and
// walks any scripted tool calls through the executor's own chokepoint.
function makeQueryFn(script = {}) {
  const prompts = [];
  const queryFn = vi.fn(async function* ({ prompt, options }) {
    prompts.push(prompt);
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
        structured_output: script.verify ?? { ok: true, issue: null, fixable: false },
        usage: { input_tokens: 3, output_tokens: 3 },
      };
      return;
    }
    for (const call of script.toolCalls ?? []) {
      await options._callTool(call.tool, call.args ?? {});
    }
    yield { type: "result", result: script.text ?? "done", usage: { input_tokens: 10, output_tokens: 8 } };
  });
  queryFn.prompts = prompts;
  return queryFn;
}

const turnEvents = (httpFn, type) =>
  httpFn.calls.filter((c) => c.method === "POST" && c.path === "/api/event" && c.body?.type === type);

describe("fetchMemoryContext", () => {
  it("posts the workspace-scoped query and returns the compiler's labeled block", async () => {
    const httpFn = vi.fn(async () => ({
      ok: true,
      status: 200,
      data: { context: "compiled working memory", recall: { top_activation: 0.9 } },
    }));

    const { block, recall } = await fetchMemoryContext(httpFn, "ws_test", "what did I say about the launch?");

    expect(httpFn).toHaveBeenCalledWith("POST", "/api/memory/context", {
      query: "what did I say about the launch?",
      workspace_id: "ws_test",
      recent_turns: [],
      budget_tokens: 2000,
      top_k: 8,
    });
    expect(block).toContain("## Memory (activation recall)");
    expect(block).toContain("compiled working memory");
    expect(recall).toEqual({ top_activation: 0.9 });
  });

  it("returns a null block on any error, never throwing", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("connection refused");
    });

    const result = await fetchMemoryContext(httpFn, "ws_test", "goal");

    expect(result).toEqual({ block: null, recall: null });
  });

  it("returns a null block when the compiler has nothing to say", async () => {
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: { context: "", recall: null } }));

    const result = await fetchMemoryContext(httpFn, "ws_test", "goal");

    expect(result.block).toBeNull();
  });
});

describe("ingestTurnOutcome", () => {
  it("posts a bounded goal->outcome content string with source 'agent'", async () => {
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: { event_id: "e1" } }));
    const longText = "x".repeat(1000);

    await ingestTurnOutcome(httpFn, "ws_test", "draft a tweet", "completed", longText);

    expect(httpFn).toHaveBeenCalledWith(
      "POST",
      "/api/memory/ingest",
      expect.objectContaining({ source: "agent", workspace_id: "ws_test" }),
    );
    const [, , body] = httpFn.mock.calls[0];
    expect(body.content.startsWith("draft a tweet -> completed:")).toBe(true);
    expect(body.content.length).toBeLessThan(600);
  });

  it("never throws when the ingest call fails", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("api down");
    });

    await expect(ingestTurnOutcome(httpFn, "ws_test", "goal", "completed", "text")).resolves.toBeUndefined();
  });
});

describe("runAgentTurn - memory injection", () => {
  it("injects the compiler's labeled block into the prompt and stamps memory_injected: true", async () => {
    const SEEDED_FACT = "SEEDED-FACT-user-prefers-dark-mode-42";
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: `Known fact: ${SEEDED_FACT}`, recall: { hits: 1 } } }),
      },
    ]);
    const queryFn = makeQueryFn({ text: "used the fact" });

    const result = await runAgentTurn("what do I prefer?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes(SEEDED_FACT))).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes("## Memory (activation recall)"))).toBe(true);

    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.memory_injected).toBe(true);
  });

  it("completes normally with memory_injected: false when the memory route fails", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => {
          throw new Error("lifeos-memory unavailable");
        },
      },
    ]);
    const queryFn = makeQueryFn({ text: "done anyway" });

    const result = await runAgentTurn("do something", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.memory_injected).toBe(false);
  });

  it("calls ingest post-turn with a bounded content string, best-effort", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "the result text" });

    await runAgentTurn("do something", "ws_test", { queryFn, httpFn });

    const ingestCall = httpFn.calls.find((c) => c.path === "/api/memory/ingest");
    expect(ingestCall).toBeTruthy();
    expect(ingestCall.body.source).toBe("agent");
    expect(ingestCall.body.content).toContain("do something -> completed");
    expect(ingestCall.body.content).toContain("the result text");
  });

  it("ingest failure does not affect the turn result", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/ingest",
        reply: () => {
          throw new Error("ingest down");
        },
      },
    ]);
    const queryFn = makeQueryFn({ text: "done" });

    const result = await runAgentTurn("do something", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");
  });

  it("recalls a distilled rule via the existing memory-context passthrough (issue #128)", async () => {
    // No new recall path is built for lessons/skills - a rule that made it
    // into memory_rules (via feedback.given -> HeuristicPolicyLearner ->
    // memory.rule.added, at the next sleep cycle) arrives folded into the
    // SAME compiled context block `fetchMemoryContext` already passes
    // through verbatim.
    const RULE_TEXT = "lesson: always keep drafts under 80 words";
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: `Rules:\n- ${RULE_TEXT}`, recall: { hits: 1 } } }),
      },
    ]);
    const queryFn = makeQueryFn({ text: "kept it short" });

    const result = await runAgentTurn("draft a launch summary", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes(RULE_TEXT))).toBe(true);
  });

  it("injects the active operating manual alongside memory/world context (issue #128)", async () => {
    const MANUAL_TEXT = "MANUAL-TEXT-lead-with-the-tldr-77";
    const httpFn = makeHttp([
      {
        match: (m, p) => p.startsWith("/api/configs"),
        reply: () => ({
          ok: true,
          status: 200,
          data: {
            configs: [{ id: "cfg_1", kind: "agent_manual", payload: { text: MANUAL_TEXT }, status: "promoted" }],
            active: { agent_manual: "cfg_1" },
          },
        }),
      },
    ]);
    const queryFn = makeQueryFn({ text: "followed the manual" });

    const result = await runAgentTurn("what should I do next?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes(MANUAL_TEXT))).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes("## Operating manual"))).toBe(true);
  });

  it("omits the manual block cleanly when no manual has ever been promoted", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "no manual yet" });

    const result = await runAgentTurn("what should I do next?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes("## Operating manual"))).toBe(false);
  });

  it("gate-refused turn makes no memory calls at all", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("/api/entity/agent_config_"),
        reply: () => ({ ok: true, status: 200, data: { attrs: { killSwitch: true } } }),
      },
    ]);
    const queryFn = makeQueryFn({ text: "should not run" });

    const result = await runAgentTurn("do anything", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("kill_switch");
    expect(httpFn.calls.some((c) => c.path === "/api/memory/context")).toBe(false);
    expect(httpFn.calls.some((c) => c.path === "/api/memory/ingest")).toBe(false);
    expect(queryFn).not.toHaveBeenCalled();
  });
});
