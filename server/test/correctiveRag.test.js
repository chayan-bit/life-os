import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { classify, REGISTRY } from "../agent/actionRegistry.js";
import {
  buildAbstentionResponse,
  correctiveRetrieve,
  gradeRecall,
  isQuestionTurn,
  rewriteQuery,
} from "../agent/correctiveRag.js";

// Mirrors agent.test.js / memoryContext.test.js's fake HTTP layer: records
// every call, replies via an optional route list (matched on method + path +
// body so a rewritten-query re-fetch can be told apart from the first),
// defaults to a canned success so no turn ever touches the network.
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

// A mock Agent SDK queryFn that captures every prompt and walks any scripted
// tool calls through the executor's own chokepoint. Handles plan/verify/
// rewrite structured-output purposes plus the free-text execute purpose.
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
        structured_output: script.verify ?? { ok: true, issue: null, fixable: false, confidence: 1 },
        usage: { input_tokens: 3, output_tokens: 3 },
      };
      return;
    }
    if (options.purpose === "rewrite") {
      yield {
        type: "result",
        structured_output: script.rewrite ?? { query: "sharper query" },
        usage: { input_tokens: 2, output_tokens: 2 },
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

describe("gradeRecall", () => {
  it("grades a recall with >= 2 memories as sufficient", () => {
    expect(gradeRecall({ outcome: "recalled", memories: [{ id: "m1" }, { id: "m2" }] })).toBe("sufficient");
  });

  it("grades a recall with fewer memories as weak", () => {
    expect(gradeRecall({ outcome: "recalled", memories: [{ id: "m1" }] })).toBe("weak");
  });

  it("grades an abstained recall as weak", () => {
    expect(gradeRecall({ outcome: "abstained", top_activation: 0.1, threshold: 0.3 })).toBe("weak");
  });

  it("grades a skipped recall as none", () => {
    expect(gradeRecall({ outcome: "skipped", reason: "no memory needed" })).toBe("none");
  });

  it("grades a null recall as none", () => {
    expect(gradeRecall(null)).toBe("none");
  });
});

describe("isQuestionTurn", () => {
  it("treats a plain question as a question turn", () => {
    expect(isQuestionTurn("what did I decide about the launch date?")).toBe(true);
  });

  it("treats an imperative mutation request as not a question turn", () => {
    expect(isQuestionTurn("create a task to follow up tomorrow")).toBe(false);
  });
});

describe("rewriteQuery", () => {
  it("returns the model's sharpened query", async () => {
    const queryFn = makeQueryFn({ rewrite: { query: "launch date decision Q3" } });

    const result = await rewriteQuery(queryFn, "what did we decide?", {});

    expect(result.query).toBe("launch date decision Q3");
    expect(queryFn.prompts[0]).toContain("what did we decide?");
  });

  it("falls back to the original prompt on malformed structured output", async () => {
    const queryFn = makeQueryFn({ rewrite: { notAQuery: true } });

    const result = await rewriteQuery(queryFn, "original question", {});

    expect(result.query).toBe("original question");
  });
});

describe("correctiveRetrieve", () => {
  it("sufficient context: no rewrite call, citation instruction present, no web suggestion", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({
          ok: true,
          status: 200,
          data: {
            context: "- (src=ev1) launch is in Q3",
            recall: { outcome: "recalled", memories: [{ id: "m1" }, { id: "m2" }] },
          },
        }),
      },
    ]);
    const queryFn = makeQueryFn();

    const result = await correctiveRetrieve({ httpFn, workspaceId: "ws_test", queryFn }, {}, "when does launch ship?");

    expect(result.rag).toEqual({ grade: "sufficient", rewritten: false, regraded: null, web_suggested: false });
    expect(result.block).toContain("(src=ev1)");
    expect(result.block).toContain("cite sources inline");
    expect(result.block).not.toContain("web.scrape");
    expect(queryFn).not.toHaveBeenCalled();
  });

  it("weak context: rewrites once, re-retrieves, and upgrades to sufficient on re-grade", async () => {
    let contextCalls = 0;
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: (m, p, body) => {
          contextCalls += 1;
          if (body.query === "sharper query") {
            return {
              ok: true,
              status: 200,
              data: {
                context: "- (src=ev2) launch is in Q3",
                recall: { outcome: "recalled", memories: [{ id: "m1" }, { id: "m2" }] },
              },
            };
          }
          return { ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } };
        },
      },
    ]);
    const queryFn = makeQueryFn({ rewrite: { query: "sharper query" } });

    const result = await correctiveRetrieve({ httpFn, workspaceId: "ws_test", queryFn }, {}, "when does it ship?");

    expect(contextCalls).toBe(2);
    expect(queryFn).toHaveBeenCalledTimes(1);
    expect(result.rag).toEqual({ grade: "none", rewritten: true, regraded: "sufficient", web_suggested: false });
    expect(result.block).toContain("(src=ev2)");
    expect(result.block).not.toContain("web.scrape");
  });

  it("still-weak after rewrite: suggests the web fallback exactly once, rewrite stays bounded", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } }),
      },
    ]);
    const queryFn = makeQueryFn({ rewrite: { query: "still no hits query" } });

    const result = await correctiveRetrieve({ httpFn, workspaceId: "ws_test", queryFn }, {}, "who won the match?");

    expect(queryFn).toHaveBeenCalledTimes(1);
    expect(result.rag).toEqual({ grade: "none", rewritten: true, regraded: "none", web_suggested: true });
    expect(result.block).toContain("web.scrape");
    expect(result.block).toContain("cite sources inline");
  });

  it("a rewrite failure degrades to the original result instead of throwing", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } }),
      },
    ]);
    const queryFn = vi.fn(async function* () {
      throw new Error("model unavailable");
    });

    const result = await correctiveRetrieve({ httpFn, workspaceId: "ws_test", queryFn }, {}, "anything?");

    expect(result.rag.web_suggested).toBe(true);
    expect(result.block).toContain("web.scrape");
  });
});

describe("buildAbstentionResponse", () => {
  it("includes the critic's issue as a clarifying question", () => {
    const text = buildAbstentionResponse("which project's launch date do you mean");

    expect(text).toContain("which project's launch date do you mean");
    expect(text.toLowerCase()).toContain("clarify");
  });

  it("falls back to a generic clarifier when no issue is given", () => {
    const text = buildAbstentionResponse(null);

    expect(text.toLowerCase()).toContain("clarify");
  });
});

describe("web.scrape tool", () => {
  it("is registered as allowed and external (untrusted-wrapped)", () => {
    expect(REGISTRY["web.scrape"]).toBeTruthy();
    expect(REGISTRY["web.scrape"].classification).toBe("allowed");
    expect(REGISTRY["web.scrape"].external).toBe(true);
    expect(REGISTRY["web.scrape"].route).toEqual({ method: "POST", path: "/api/browser/scrape" });
    expect(classify("web.scrape")).toBe("allowed");
  });
});

describe("runAgentTurn - corrective RAG integration", () => {
  it("sufficient memory: cites sources, stamps rag attrs, never rewrites", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({
          ok: true,
          status: 200,
          data: {
            context: "- (src=ev1) launch is in Q3",
            recall: { outcome: "recalled", memories: [{ id: "m1" }, { id: "m2" }] },
          },
        }),
      },
    ]);
    const queryFn = makeQueryFn({ text: "launch is in Q3 (src=ev1)" });

    const result = await runAgentTurn("when does launch ship?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");
    expect(queryFn.prompts.some((p) => p.includes("cite sources inline"))).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes("web.scrape"))).toBe(false);
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.rag).toEqual({ grade: "sufficient", rewritten: false, regraded: null, web_suggested: false });
  });

  it("weak-then-weak memory: rewrites, re-retrieves, offers web.scrape, stamps rag attrs", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } }),
      },
    ]);
    const queryFn = makeQueryFn({ rewrite: { query: "sharper query" }, text: "not sure, no memory found" });

    const result = await runAgentTurn("who won the match?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(queryFn.prompts.some((p) => p.includes("web.scrape"))).toBe(true);
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.rag).toEqual({ grade: "none", rewritten: true, regraded: "none", web_suggested: true });
  });

  it("abstains with a clarifying question on weak context + low critic confidence, no tool calls", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } }),
      },
    ]);
    const queryFn = makeQueryFn({
      rewrite: { query: "still no hits" },
      text: "maybe it was yesterday, not totally sure",
      verify: { ok: false, issue: "which day are you asking about", fixable: false, confidence: 0.2 },
    });

    const result = await runAgentTurn("what day was that again?", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("abstained");
    expect(result.text).toContain("which day are you asking about");
    expect(result.text).not.toContain("maybe it was yesterday");
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.outcome).toBe("abstained");
  });

  it("never abstains on a turn that already executed a tool successfully", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => p === "/api/memory/context",
        reply: () => ({ ok: true, status: 200, data: { context: null, recall: { outcome: "skipped" } } }),
      },
    ]);
    const queryFn = makeQueryFn({
      rewrite: { query: "still no hits" },
      toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }],
      text: "here is what I found",
      verify: { ok: true, issue: null, fixable: false, confidence: 0.1 },
    });

    const result = await runAgentTurn("what is on my plate right now", "ws_test", { queryFn, httpFn });

    expect(result.success).toBe(true);
    expect(result.outcome).toBe("completed");
    expect(result.text).toBe("here is what I found");
  });
});
