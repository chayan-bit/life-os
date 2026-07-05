import { describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { CORE_TOOLS, TOOL_RAG_K, indexTools, retrieveTools } from "../agent/toolRag.js";
import { loadGeneratedTools } from "../agent/tools/generated/index.js";
import fs from "node:fs/promises";
import path from "node:path";

// A minimal fake registry - mirrors the shape toolRag.js reads (name ->
// { description }), independent of the real REGISTRY so tests can prove
// narrowing at a scale (50+) beyond the real catalog's size.
function makeRegistry(names) {
  const registry = {};
  for (const name of names) registry[name] = { description: `${name} does things` };
  return registry;
}

const REAL_CORE_SUBSET = ["search.query", "entity.list", "entity.get", "entity.create", "entity.update"];

function bigRegistry() {
  const synthetic = Array.from({ length: 50 }, (_, i) => `tool.synthetic_${i}`);
  return makeRegistry([...REAL_CORE_SUBSET, "event.append", "draft.create", ...synthetic]);
}

// A fake HTTP layer mirroring agent.test.js's makeHttp: records calls, lets a
// test seed an existing digest-marker entity, and defaults reads to empty.
function makeHttp({ digestEntity = null } = {}) {
  const calls = [];
  let stored = digestEntity;
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    if (method === "GET" && path.includes("/api/entity/agent_config_")) {
      return { ok: false, status: 404, data: null };
    }
    if (method === "GET" && path.startsWith("/api/entity?module=agent&type=tool_index_digest")) {
      return { ok: true, status: 200, data: stored ? [stored] : [] };
    }
    if (method === "POST" && path === "/api/entity" && body?.type === "tool_index_digest") {
      stored = { id: "ent_digest_1", attrs: body.attrs };
      return { ok: true, status: 200, data: stored };
    }
    if (method === "PATCH" && stored && path === `/api/entity/${stored.id}`) {
      stored = { ...stored, attrs: body.attrs };
      return { ok: true, status: 200, data: stored };
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

describe("retrieveTools", () => {
  it("narrows 50+ registered tools to core + top-K for a mocked retrieveFn", async () => {
    const registry = bigRegistry();
    const retrieveFn = vi.fn(async (_prompt, k) =>
      Array.from({ length: k }, (_, i) => `tool:tool.synthetic_${i}`),
    );

    const { tools, fallback } = await retrieveTools("do something", registry, { retrieveFn });

    expect(fallback).toBe(false);
    expect(tools.length).toBe(REAL_CORE_SUBSET.length + 2 + TOOL_RAG_K);
    for (const core of CORE_TOOLS) {
      if (registry[core]) expect(tools).toContain(core);
    }
    expect(tools).toContain("tool.synthetic_0");
    expect(tools).not.toContain("tool.synthetic_49");
    expect(tools.length).toBeLessThan(Object.keys(registry).length);
  });

  it("falls back to the full catalog with fallback=true when retrieveFn throws", async () => {
    const registry = bigRegistry();
    const retrieveFn = vi.fn(async () => {
      throw new Error("memvec unavailable");
    });

    const { tools, fallback } = await retrieveTools("do something", registry, { retrieveFn });

    expect(fallback).toBe(true);
    expect(tools).toEqual(Object.keys(registry));
  });

  it("falls back to the full catalog with fallback=true on an empty index", async () => {
    const registry = bigRegistry();
    const retrieveFn = vi.fn(async () => []);

    const { tools, fallback } = await retrieveTools("do something", registry, { retrieveFn });

    expect(fallback).toBe(true);
    expect(tools).toEqual(Object.keys(registry));
  });

  it("preserves registry order and drops hits for names no longer in the registry", async () => {
    const registry = makeRegistry(["a.one", "b.two", "c.three"]);
    const retrieveFn = vi.fn(async () => ["tool:c.three", "tool:stale.gone", "tool:a.one"]);

    const { tools } = await retrieveTools("q", registry, { retrieveFn, k: 2 });

    expect(tools).toEqual(["a.one", "c.three"]);
  });
});

describe("indexTools", () => {
  it("embeds every tool and writes a digest marker when none exists", async () => {
    const httpFn = makeHttp();
    const registry = makeRegistry(["a.one", "b.two"]);
    const embedFn = vi.fn(async () => {});

    const result = await indexTools(registry, { httpFn, workspaceId: "ws_test", embedFn });

    expect(result.reembedded).toBe(true);
    expect(embedFn).toHaveBeenCalledTimes(2);
    const created = httpFn.calls.find((c) => c.method === "POST" && c.body?.type === "tool_index_digest");
    expect(created).toBeTruthy();
    expect(created.body.attrs.digest).toBe(result.digest);
  });

  it("skips re-embedding when the registry digest is unchanged", async () => {
    const httpFn = makeHttp();
    const registry = makeRegistry(["a.one", "b.two"]);
    const embedFn = vi.fn(async () => {});

    const first = await indexTools(registry, { httpFn, workspaceId: "ws_test", embedFn });
    expect(first.reembedded).toBe(true);
    embedFn.mockClear();

    const second = await indexTools(registry, { httpFn, workspaceId: "ws_test", embedFn });

    expect(second.reembedded).toBe(false);
    expect(second.digest).toBe(first.digest);
    expect(embedFn).not.toHaveBeenCalled();
  });

  it("re-embeds when the registry content changes (digest changes)", async () => {
    const httpFn = makeHttp();
    const embedFn = vi.fn(async () => {});

    const first = await indexTools(makeRegistry(["a.one", "b.two"]), {
      httpFn,
      workspaceId: "ws_test",
      embedFn,
    });
    embedFn.mockClear();

    const second = await indexTools(makeRegistry(["a.one", "b.two", "c.three"]), {
      httpFn,
      workspaceId: "ws_test",
      embedFn,
    });

    expect(second.reembedded).toBe(true);
    expect(second.digest).not.toBe(first.digest);
    expect(embedFn).toHaveBeenCalledTimes(3);
  });

  it("never throws when embedFn fails - returns reembedded:false with an error", async () => {
    const httpFn = makeHttp();
    const embedFn = vi.fn(async () => {
      throw new Error("memvec exploded");
    });

    const result = await indexTools(makeRegistry(["a.one"]), { httpFn, workspaceId: "ws_test", embedFn });

    expect(result.reembedded).toBe(false);
    expect(result.error).toContain("memvec exploded");
  });

  // Issue #134: a newly installed T2 generated tool must change the digest
  // and trigger a re-embed, with no extra wiring beyond loadGeneratedTools()
  // feeding into the SAME registry indexTools already hashes (server/agent/
  // actionRegistry.js merges generated tools into REGISTRY at module load).
  it("a registry with a generated tool produces a different digest than without, and triggers re-embed", async () => {
    const httpFn = makeHttp();
    const embedFn = vi.fn(async () => {});
    const baseRegistry = makeRegistry(["entity.create", "entity.list"]);

    const withoutGenerated = await indexTools(baseRegistry, { httpFn, workspaceId: "ws_test", embedFn });
    embedFn.mockClear();

    const fixtureDir = await fs.mkdtemp(path.join(path.resolve(import.meta.dirname), ".tmp-toolrag-generated-"));
    try {
      await fs.writeFile(
        path.join(fixtureDir, "rMultiple.js"),
        [
          'import { z } from "zod";',
          "",
          "export default {",
          '  name: "rMultiple",',
          '  description: "Compute the R-multiple for a closed trade entity.",',
          '  classification: "allowed",',
          "  inputSchema: z.object({ tradeId: z.string() }),",
          '  example: { tradeId: "ent_trade_1" },',
          "  request: ({ args }) => ({ method: \"GET\", path: `/api/entity/${args.tradeId}` }),",
          "};",
          "",
        ].join("\n"),
        "utf8",
      );
      const { tools: generatedTools } = await loadGeneratedTools({ dir: fixtureDir });
      const registryWithGenerated = { ...baseRegistry, ...generatedTools };

      const withGenerated = await indexTools(registryWithGenerated, { httpFn: makeHttp(), workspaceId: "ws_test", embedFn });

      expect(withGenerated.digest).not.toBe(withoutGenerated.digest);
      expect(withGenerated.reembedded).toBe(true);
      expect(embedFn).toHaveBeenCalledWith("rMultiple", registryWithGenerated.rMultiple.description, expect.anything());
    } finally {
      await fs.rm(fixtureDir, { recursive: true, force: true });
    }
  });
});

// A mock Agent SDK queryFn - mirrors agent.test.js's makeQueryFn, driven by a
// per-role script, so the loop-integration test proves tools_offered /
// toolrag_fallback land on the agent.turn event without touching the network
// or a real memvec subprocess.
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

describe("runAgentTurn - Tool-RAG wiring", () => {
  it("records tools_offered and toolrag_fallback on the agent.turn event for a successful retrieval", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }] });
    const retrieveFn = vi.fn(async () => ["tool:entity.list", "tool:search.query"]);
    const embedFn = vi.fn(async () => {});

    const result = await runAgentTurn("list my tasks", "ws_test", {
      queryFn,
      httpFn,
      toolRag: { retrieveFn, embedFn },
    });

    expect(result.success).toBe(true);
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.toolrag_fallback).toBe(false);
    expect(typeof turn.body.attrs.tools_offered).toBe("number");
    expect(turn.body.attrs.tools_offered).toBeGreaterThan(0);
  });

  it("records toolrag_fallback:true and still completes the turn when retrieval fails", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }] });
    const retrieveFn = vi.fn(async () => {
      throw new Error("memvec down");
    });
    const embedFn = vi.fn(async () => {});

    const result = await runAgentTurn("list my tasks", "ws_test", {
      queryFn,
      httpFn,
      toolRag: { retrieveFn, embedFn },
    });

    expect(result.success).toBe(true);
    const turn = turnEvents(httpFn, "agent.turn")[0];
    expect(turn.body.attrs.toolrag_fallback).toBe(true);
  });
});
