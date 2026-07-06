import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { runAgentTurn } from "../agent/loop.js";
import { cacheKey, isCacheMode, looksActiony, probe, store } from "../agent/llmCache.js";

// A fake HTTP layer mirroring agent.test.js/toolRag.test.js's makeHttp:
// records every call and returns benign defaults so a turn never touches the
// network. Absent agent config -> 404 (gate treats this as "use defaults").
function makeHttp() {
  const calls = [];
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    if (method === "GET" && path.includes("/api/entity/agent_config_")) {
      return { ok: false, status: 404, data: null };
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

// A mock Agent SDK queryFn - mirrors agent.test.js/toolRag.test.js's
// makeQueryFn, driven by a per-role script.
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

const turnEvents = (httpFn) =>
  httpFn.calls.filter((c) => c.method === "POST" && c.path === "/api/event" && c.body?.type === "agent.turn");

const noRetrieval = { retrieveFn: vi.fn(async () => []), embedFn: vi.fn(async () => {}) };

beforeEach(() => {
  vi.unstubAllEnvs();
});

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("cacheKey", () => {
  it("is deterministic for identical inputs", () => {
    const req = { model: "m1", system: "s", prompt: "hello", params: { a: 1 } };
    expect(cacheKey(req)).toBe(cacheKey({ ...req }));
  });

  it("differs when model, prompt, or params differ", () => {
    const base = { model: "m1", system: "s", prompt: "hello", params: { a: 1 } };
    expect(cacheKey(base)).not.toBe(cacheKey({ ...base, model: "m2" }));
    expect(cacheKey(base)).not.toBe(cacheKey({ ...base, prompt: "goodbye" }));
    expect(cacheKey(base)).not.toBe(cacheKey({ ...base, params: { a: 2 } }));
  });
});

describe("looksActiony", () => {
  it("flags imperative mutation verbs", () => {
    expect(looksActiony("create a task for tomorrow")).toBe(true);
    expect(looksActiony("send an email to the team")).toBe(true);
    expect(looksActiony("please draft a reply")).toBe(true);
    expect(looksActiony("schedule the meeting")).toBe(true);
  });

  it("does not flag plain questions", () => {
    expect(looksActiony("what is the capital of France")).toBe(false);
    expect(looksActiony("summarize my week")).toBe(false);
  });
});

describe("isCacheMode", () => {
  it("is true only when ANTHROPIC_API_KEY is set", () => {
    vi.stubEnv("ANTHROPIC_API_KEY", "");
    expect(isCacheMode()).toBe(false);
    vi.stubEnv("ANTHROPIC_API_KEY", "sk-test");
    expect(isCacheMode()).toBe(true);
  });
});

describe("probe", () => {
  beforeEach(() => vi.stubEnv("ANTHROPIC_API_KEY", "sk-test"));

  it("returns hit:null without calling cacheGetFn when keyless", async () => {
    vi.stubEnv("ANTHROPIC_API_KEY", "");
    const cacheGetFn = vi.fn();
    const result = await probe({ workspace: "ws1", model: "m", prompt: "hi", params: {} }, { cacheGetFn });
    expect(result).toEqual({ hit: null });
    expect(cacheGetFn).not.toHaveBeenCalled();
  });

  it("passes workspace through to cacheGetFn and returns an exact hit", async () => {
    const cacheGetFn = vi.fn(async () => ({ hit: "exact", completion: "cached" }));
    const result = await probe({ workspace: "ws1", model: "m", prompt: "hi", params: {} }, { cacheGetFn });
    expect(result).toEqual({ hit: "exact", completion: "cached" });
    expect(cacheGetFn.mock.calls[0][0].workspace).toBe("ws1");
  });

  it("resolves to hit:null when cacheGetFn throws", async () => {
    const cacheGetFn = vi.fn(async () => {
      throw new Error("memvec down");
    });
    const result = await probe({ workspace: "ws1", model: "m", prompt: "hi", params: {} }, { cacheGetFn });
    expect(result).toEqual({ hit: null });
  });
});

describe("store", () => {
  beforeEach(() => vi.stubEnv("ANTHROPIC_API_KEY", "sk-test"));

  it("does not call cachePutFn when keyless", async () => {
    vi.stubEnv("ANTHROPIC_API_KEY", "");
    const cachePutFn = vi.fn();
    await store({ workspace: "ws1", model: "m", prompt: "hi", params: {} }, "answer", { cachePutFn });
    expect(cachePutFn).not.toHaveBeenCalled();
  });

  it("passes workspace, prompt, completion, and model through to cachePutFn", async () => {
    const cachePutFn = vi.fn(async () => {});
    await store({ workspace: "ws1", model: "haiku", prompt: "hi", params: {} }, "answer", { cachePutFn });
    expect(cachePutFn.mock.calls[0][0]).toMatchObject({
      workspace: "ws1",
      prompt: "hi",
      completion: "answer",
      model: "haiku",
    });
  });

  it("swallows a cachePutFn error", async () => {
    const cachePutFn = vi.fn(async () => {
      throw new Error("disk full");
    });
    await expect(
      store({ workspace: "ws1", model: "m", prompt: "hi", params: {} }, "answer", { cachePutFn }),
    ).resolves.toBeUndefined();
  });
});

describe("runAgentTurn - LLM cache wiring (issue #127)", () => {
  beforeEach(() => vi.stubEnv("ANTHROPIC_API_KEY", "sk-test"));

  it("exact-hit: skips the model entirely and stamps cache:'exact', tokens 0", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn();
    const cacheGetFn = vi.fn(async () => ({ hit: "exact", completion: "cached exact answer" }));
    const cachePutFn = vi.fn();

    const result = await runAgentTurn("what is on my plate today", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(result.text).toBe("cached exact answer");
    expect(queryFn).not.toHaveBeenCalled();
    const turn = turnEvents(httpFn)[0];
    expect(turn.body.attrs.cache).toBe("exact");
    expect(turn.body.tokens_in).toBe(0);
    expect(turn.body.tokens_out).toBe(0);
    expect(turn.body.tier).toBe("mac");
    expect(turn.body.outcome).toBe("completed");
  });

  it("semantic-hit-within-threshold: skips the model and stamps cache:'semantic'", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn();
    const cacheGetFn = vi.fn(async () => ({ hit: "semantic", completion: "cached paraphrase answer", distance: 0.03 }));
    const cachePutFn = vi.fn();

    const result = await runAgentTurn("summarize my open tasks please", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(result.text).toBe("cached paraphrase answer");
    expect(queryFn).not.toHaveBeenCalled();
    const turn = turnEvents(httpFn)[0];
    expect(turn.body.attrs.cache).toBe("semantic");
  });

  it("semantic-miss-above-threshold: a cache miss runs the normal turn", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "fresh answer" });
    const cacheGetFn = vi.fn(async () => ({ hit: null }));
    const cachePutFn = vi.fn(async () => {});

    const result = await runAgentTurn("what is on my plate today", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(result.text).toBe("fresh answer");
    expect(queryFn).toHaveBeenCalled();
    // A completed, tool-free, probe-eligible turn IS stored on a miss.
    expect(cachePutFn).toHaveBeenCalledTimes(1);
    const turn = turnEvents(httpFn)[0];
    expect(turn.body.attrs.cache).toBeUndefined();
  });

  it("tool-turn-bypass: a turn that used a tool is never stored, even if probe-eligible", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ toolCalls: [{ tool: "entity.list", args: { module: "tasks" } }] });
    const cacheGetFn = vi.fn(async () => ({ hit: null }));
    const cachePutFn = vi.fn(async () => {});

    const result = await runAgentTurn("what is on my plate today", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(cachePutFn).not.toHaveBeenCalled();
  });

  it("keyless-bypass: probe and store are never invoked without ANTHROPIC_API_KEY", async () => {
    vi.stubEnv("ANTHROPIC_API_KEY", "");
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "fresh answer" });
    const cacheGetFn = vi.fn();
    const cachePutFn = vi.fn();

    const result = await runAgentTurn("what is on my plate today", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(cacheGetFn).not.toHaveBeenCalled();
    expect(cachePutFn).not.toHaveBeenCalled();
    expect(queryFn).toHaveBeenCalled();
  });

  it("workspace isolation: cacheGetFn/cachePutFn are called with the turn's workspace", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "fresh answer" });
    const cacheGetFn = vi.fn(async () => ({ hit: null }));
    const cachePutFn = vi.fn(async () => {});

    await runAgentTurn("what is on my plate today", "ws_alpha", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(cacheGetFn.mock.calls[0][0].workspace).toBe("ws_alpha");
    expect(cachePutFn.mock.calls[0][0].workspace).toBe("ws_alpha");
  });

  it("does not probe an actiony prompt (imperative mutation request)", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "done" });
    const cacheGetFn = vi.fn();
    const cachePutFn = vi.fn();

    const result = await runAgentTurn("send a summary email to the team", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(cacheGetFn).not.toHaveBeenCalled();
    expect(queryFn).toHaveBeenCalled();
  });

  it("a cache probe error lets the turn proceed normally", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ text: "fresh answer" });
    const cacheGetFn = vi.fn(async () => {
      throw new Error("memvec unavailable");
    });
    const cachePutFn = vi.fn(async () => {});

    const result = await runAgentTurn("what is on my plate today", "ws_test", {
      queryFn,
      httpFn,
      toolRag: noRetrieval,
      cache: { cacheGetFn, cachePutFn },
    });

    expect(result.success).toBe(true);
    expect(result.text).toBe("fresh answer");
    expect(queryFn).toHaveBeenCalled();
  });
});

// Guarded python-side integration test (issue #127): only runs when python3
// has sentence-transformers + sqlite-vec installed, so CI stays green without
// those heavy deps installed. Round-trips cache-put/cache-get against a temp
// derived DB, including the cross-workspace-miss case.
const { execFileSync } = await import("node:child_process");
const os = await import("node:os");
const fs = await import("node:fs");
const path = await import("node:path");

function pythonDepsAvailable() {
  try {
    execFileSync("python3", ["-c", "import sentence_transformers, sqlite_vec"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

const HAS_PY_DEPS = pythonDepsAvailable();

// Finding 18 (test/CI audit): these are the only tests that ever exercise
// memvec.py's real cache-put/cache-get path, including the cross-workspace
// isolation assertion - a silent skip forever (no CI job ever installs
// sentence-transformers/sqlite-vec) means they have never actually run. The
// dedicated "py-integration" CI job (ci.yml) sets LIFEOS_REQUIRE_PY_DEPS=1
// after installing those deps, so a missing dep there is a real regression,
// not an environment quirk - fail hard instead of skipping. Every other
// runner (no env var set) keeps the original silent-skip behavior.
if (process.env.LIFEOS_REQUIRE_PY_DEPS === "1" && !HAS_PY_DEPS) {
  throw new Error(
    "LIFEOS_REQUIRE_PY_DEPS=1 but python3 is missing sentence-transformers and/or sqlite-vec - " +
      "the py-integration CI job installs these, so this must not silently skip.",
  );
}

describe.skipIf(!HAS_PY_DEPS)("memvec.py cache-put/cache-get (python integration)", () => {
  const memvecPath = path.join(process.cwd(), "memvec.py");

  function tmpDbPath() {
    return path.join(os.tmpdir(), `lifeos-cache-test-${Date.now()}-${Math.random().toString(36).slice(2)}.db`);
  }

  function run(args) {
    return execFileSync("python3", [memvecPath, ...args], { encoding: "utf8" });
  }

  it("round-trips an exact hit and misses across workspaces", () => {
    const db = tmpDbPath();
    run(["--db", db, "cache-put", "--workspace", "ws1", "--key", "k1", "--prompt", "hello there", "--completion", "hi!"]);

    const exact = JSON.parse(run(["--db", db, "cache-get", "--workspace", "ws1", "--key", "k1", "--prompt", "hello there"]));
    expect(exact).toEqual({ hit: "exact", completion: "hi!" });

    const crossTenant = JSON.parse(
      run(["--db", db, "cache-get", "--workspace", "ws2", "--key", "k1", "--prompt", "hello there"]),
    );
    expect(crossTenant.hit).toBeNull();

    fs.rmSync(db, { force: true });
  });

  it("finds a semantic hit for a paraphrase within threshold, scoped to workspace", () => {
    const db = tmpDbPath();
    run([
      "--db",
      db,
      "cache-put",
      "--workspace",
      "ws1",
      "--key",
      "k2",
      "--prompt",
      "what tasks are overdue",
      "--completion",
      "you have 3 overdue tasks",
    ]);

    const semantic = JSON.parse(
      run([
        "--db",
        db,
        "cache-get",
        "--workspace",
        "ws1",
        "--key",
        "different-key",
        "--prompt",
        "which tasks are overdue",
        "--threshold",
        "0.3",
      ]),
    );
    expect(semantic.hit).toBe("semantic");
    expect(semantic.completion).toBe("you have 3 overdue tasks");

    const crossTenant = JSON.parse(
      run([
        "--db",
        db,
        "cache-get",
        "--workspace",
        "ws2",
        "--key",
        "different-key",
        "--prompt",
        "which tasks are overdue",
        "--threshold",
        "0.3",
      ]),
    );
    expect(crossTenant.hit).toBeNull();

    fs.rmSync(db, { force: true });
  });
});
