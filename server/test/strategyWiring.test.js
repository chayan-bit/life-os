// Issue #156: end-to-end proof that runAgentTurn (loop.js) actually emits
// the strategy.js outcome event for the planner.prompt decision group once a
// turn's fate (replanned vs not, degraded vs not) is known. The rag.rewrite
// group's own outcome emission is already covered at the correctiveRag.js
// unit level (test/correctiveRag.test.js) since it never needs loop.js to
// know anything extra; this file only covers the piece that genuinely lives
// in loop.js - reading `ctx.plannerVariant`/`ctx.replanned`/`outcome` after
// the turn resolves.
import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { PLANNER_PROMPT_GROUP } from "../agent/planner.js";

function makeHttp(routes = []) {
  const calls = [];
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    for (const route of routes) {
      if (route.match(method, path, body)) return route.reply(method, path, body);
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

const strategyOutcomes = (httpFn, group) =>
  httpFn.calls
    .filter((c) => c.method === "POST" && c.path === "/api/event" && c.body?.type === "agent.strategy.outcome")
    .map((c) => c.body.attrs)
    .filter((attrs) => attrs.group === group);

describe("runAgentTurn - planner.prompt outcome emission (#156, group 2 of 3)", () => {
  it("records success=true when the plan executes to completion without a replan", async () => {
    const httpFn = makeHttp();
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
        yield {
          type: "result",
          structured_output: {
            stages: [
              { name: "find", tool: "entity.list", description: "find overdue tasks" },
              { name: "tag", tool: "entity.update", description: "tag them urgent" },
            ],
          },
          usage: { input_tokens: 5, output_tokens: 5 },
        };
        return;
      }
      if (options.purpose === "verify") {
        yield { type: "result", structured_output: { ok: true, issue: null, fixable: false }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      await options._callTool("entity.list", { module: "tasks", status: "overdue" });
      await options._callTool("entity.update", { id: "ent_x", patch: { status: "urgent" } });
      yield { type: "result", result: "Tagged 2 tasks urgent.", usage: { input_tokens: 10, output_tokens: 8 } };
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");
    const outcomes = strategyOutcomes(httpFn, PLANNER_PROMPT_GROUP);
    expect(outcomes).toHaveLength(1);
    expect(outcomes[0].success).toBe(true);
    expect(["stages", "checklist"]).toContain(outcomes[0].variant);
  });

  it("records success=false when the turn needed a replan even though it eventually recovered", async () => {
    const httpFn = makeHttp();
    let executeCalls = 0;
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
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
      yield { type: "result", result: "recovered", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(true);
    const outcomes = strategyOutcomes(httpFn, PLANNER_PROMPT_GROUP);
    expect(outcomes).toHaveLength(1);
    expect(outcomes[0].success).toBe(false);
  });

  it("records success=false on a degraded turn", async () => {
    const httpFn = makeHttp();
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
        yield {
          type: "result",
          structured_output: { stages: [{ name: "find", tool: "entity.list", description: "find tasks" }] },
          usage: { input_tokens: 5, output_tokens: 5 },
        };
        return;
      }
      throw new Error("still broken");
    });

    const result = await runAgentTurn("find my overdue tasks, tag them urgent, and draft a summary", "ws_test", {
      queryFn,
      httpFn,
    });

    expect(result.success).toBe(false);
    expect(result.outcome).toBe("degraded");
    const outcomes = strategyOutcomes(httpFn, PLANNER_PROMPT_GROUP);
    expect(outcomes).toHaveLength(1);
    expect(outcomes[0].success).toBe(false);
  });

  it("never records a planner.prompt outcome on a single-step turn that never planned", async () => {
    const httpFn = makeHttp();
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "verify") {
        yield { type: "result", structured_output: { ok: true, issue: null, fixable: false, confidence: 1 }, usage: { input_tokens: 1, output_tokens: 1 } };
        return;
      }
      yield { type: "result", result: "42", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    const result = await runAgentTurn("what is 6*7?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(strategyOutcomes(httpFn, PLANNER_PROMPT_GROUP)).toHaveLength(0);
  });

  it("never records a planner.prompt outcome on a dry run", async () => {
    const httpFn = makeHttp();
    const queryFn = vi.fn(async function* ({ options }) {
      if (options.purpose === "plan") {
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
      yield { type: "result", result: "done", usage: { input_tokens: 5, output_tokens: 5 } };
    });

    await runAgentTurn("find tasks, tag them, and summarize", "ws_test", { queryFn, httpFn, dryRun: true });

    expect(strategyOutcomes(httpFn, PLANNER_PROMPT_GROUP)).toHaveLength(0);
  });
});
