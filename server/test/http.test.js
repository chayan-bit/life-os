// Finding 42 (test/CI audit): server/agent/http.js is the only real network
// path of the agent loop/build pipeline - every other test in this suite
// injects a fake httpFn (see agent.test.js/roleCaps.test.js's makeHttp), so
// createHttpFn's actual fetch wiring has never run. This file mocks the
// transport one layer down (global.fetch) instead of faking httpFn itself,
// so the real module - URL/header construction, JSON parsing, and the
// throw-vs-return-ok:false contract the retry ladder depends on - is what's
// under test.
import { afterEach, describe, expect, it, vi } from "vitest";
import { createHttpFn } from "../agent/http.js";
import { runTool } from "../agent/executor.js";
import { RETRY_BACKOFF_MS } from "../agent/recovery.js";

function fakeResponse(status, bodyText) {
  return {
    ok: status >= 200 && status < 300,
    status,
    text: async () => bodyText,
  };
}

// A ctx skeleton matching what the loop builds (mirrors roleCaps.test.js's
// makeCtx), except httpFn is the REAL createHttpFn wired to a mocked
// global.fetch - so runTool exercises http.js exactly as production does.
function makeCtx(httpFn, overrides = {}) {
  return {
    workspaceId: "ws_test",
    httpFn,
    dryRun: false,
    ledger: [],
    pendingApprovals: [],
    stepCount: 0,
    recoveryBudget: 3,
    recoveries: [],
    sleepFn: vi.fn(async () => {}),
    ...overrides,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("createHttpFn - request construction", () => {
  it("builds the URL from apiBase + path, JSON-encodes the body, and sets the workspace header", async () => {
    const fetchMock = vi.fn(async () => fakeResponse(200, JSON.stringify({ id: "e1" })));
    vi.stubGlobal("fetch", fetchMock);

    const httpFn = createHttpFn("ws_alpha", "http://api.internal:8080");
    const result = await httpFn("POST", "/api/entity", { module: "tasks", type: "task" });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.internal:8080/api/entity");
    expect(init.method).toBe("POST");
    expect(init.headers["Content-Type"]).toBe("application/json");
    expect(init.headers["X-Workspace-Id"]).toBe("ws_alpha");
    expect(init.body).toBe(JSON.stringify({ module: "tasks", type: "task" }));
    expect(result).toEqual({ ok: true, status: 200, data: { id: "e1" } });
  });

  it("omits the body entirely for a GET call with no payload", async () => {
    const fetchMock = vi.fn(async () => fakeResponse(200, "[]"));
    vi.stubGlobal("fetch", fetchMock);

    const httpFn = createHttpFn("ws_alpha", "http://api.internal:8080");
    await httpFn("GET", "/api/entity?module=tasks", undefined);

    const [, init] = fetchMock.mock.calls[0];
    expect(init.body).toBeUndefined();
  });

  it("defaults apiBase to LIFEOS_API_URL, read at import time, when no apiBase is passed", async () => {
    vi.resetModules();
    vi.stubEnv("LIFEOS_API_URL", "http://example.internal:9999");
    const fetchMock = vi.fn(async () => fakeResponse(200, "{}"));
    vi.stubGlobal("fetch", fetchMock);

    const { createHttpFn: freshCreateHttpFn } = await import("../agent/http.js");
    const httpFn = freshCreateHttpFn("ws_beta");
    await httpFn("GET", "/api/entity");

    expect(fetchMock.mock.calls[0][0]).toBe("http://example.internal:9999/api/entity");
  });
});

describe("createHttpFn - response parsing", () => {
  it("parses a JSON response body", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => fakeResponse(200, JSON.stringify([{ id: "t1" }]))));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    const result = await httpFn("GET", "/api/entity");

    expect(result).toEqual({ ok: true, status: 200, data: [{ id: "t1" }] });
  });

  it("falls back to the raw text when the response body is not valid JSON", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => fakeResponse(200, "not json{")));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    const result = await httpFn("GET", "/api/entity");

    expect(result).toEqual({ ok: true, status: 200, data: "not json{" });
  });

  it("treats an empty response body as null data", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => fakeResponse(204, "")));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    const result = await httpFn("DELETE", "/api/entity/e1");

    expect(result).toEqual({ ok: true, status: 204, data: null });
  });

  it("returns ok:false with the status and error body on a non-2xx response, without throwing", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => fakeResponse(500, JSON.stringify({ error: "upstream blip" }))));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    const result = await httpFn("GET", "/api/entity");

    expect(result).toEqual({ ok: false, status: 500, data: { error: "upstream blip" } });
  });

  it("surfaces a genuine network failure by rejecting, rather than swallowing it into a result", async () => {
    const networkError = new TypeError("fetch failed");
    vi.stubGlobal("fetch", vi.fn(async () => { throw networkError; }));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    await expect(httpFn("GET", "/api/entity")).rejects.toBe(networkError);
  });

  it("surfaces a timeout-shaped abort the same way as any other network failure", async () => {
    const abortError = new DOMException("The operation was aborted", "AbortError");
    vi.stubGlobal("fetch", vi.fn(async () => { throw abortError; }));
    const httpFn = createHttpFn("ws_test", "http://api.internal");

    await expect(httpFn("GET", "/api/entity")).rejects.toBe(abortError);
  });
});

// These exercise http.js as it is actually consumed: wired into runTool via
// ctx.httpFn (executor.js's httpWithRetry), with only the transport
// (global.fetch) mocked. Every other test file in this suite fakes httpFn
// itself, which proves the retry ladder's *logic* but never that the real
// module returns responses shaped the way that logic expects - this proves
// the seam.
describe("createHttpFn wired into the executor's retry ladder", () => {
  it("succeeds on the first attempt with a single fetch call", async () => {
    const fetchMock = vi.fn(async () => fakeResponse(200, JSON.stringify([{ id: "t1" }])));
    vi.stubGlobal("fetch", fetchMock);
    const ctx = makeCtx(createHttpFn("ws_test", "http://api.internal"));

    const result = await runTool(ctx, "entity.list", { module: "tasks" });

    expect(result.status).toBe("ok");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(ctx.recoveries).toEqual([]);
  });

  it("retries once on a transient 500 and recovers through the real network layer", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(fakeResponse(500, JSON.stringify({ error: "upstream blip" })))
      .mockResolvedValueOnce(fakeResponse(200, JSON.stringify([{ id: "t1" }])));
    vi.stubGlobal("fetch", fetchMock);
    const ctx = makeCtx(createHttpFn("ws_test", "http://api.internal"));

    const result = await runTool(ctx, "entity.list", { module: "tasks" });

    expect(result.status).toBe("ok");
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(ctx.sleepFn).toHaveBeenCalledWith(RETRY_BACKOFF_MS);
    expect(ctx.recoveries).toEqual([{ kind: "retry", tool: "entity.list", ok: true }]);
  });

  it("gives up after the single retry is exhausted and surfaces the failed status, without a third attempt", async () => {
    const fetchMock = vi.fn(async () => fakeResponse(500, JSON.stringify({ error: "still down" })));
    vi.stubGlobal("fetch", fetchMock);
    const ctx = makeCtx(createHttpFn("ws_test", "http://api.internal"));

    const result = await runTool(ctx, "entity.list", { module: "tasks" });

    expect(result.status).toBe("error");
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(ctx.recoveries).toEqual([{ kind: "retry", tool: "entity.list", ok: false }]);
  });

  it("surfaces a genuine network failure that outlives the retry as a rejected call, not a swallowed error result", async () => {
    const networkError = new TypeError("fetch failed");
    const fetchMock = vi.fn(async () => { throw networkError; });
    vi.stubGlobal("fetch", fetchMock);
    const ctx = makeCtx(createHttpFn("ws_test", "http://api.internal"));

    await expect(runTool(ctx, "entity.list", { module: "tasks" })).rejects.toBe(networkError);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
