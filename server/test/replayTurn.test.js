import { describe, expect, it, vi } from "vitest";
import { fetchTurnEvent, formatTurnReplay } from "../scripts/replayTurn.js";

const fixtureEvent = {
  run_id: "run_123",
  model: "claude-haiku-4-5",
  tokens_in: 18,
  tokens_out: 16,
  latency_ms: 842,
  outcome: "completed",
  error: null,
  gated: 0,
  attrs: {
    goal: "find my overdue tasks, tag them urgent, and draft a summary",
    plan: [
      { name: "find", tool: "entity.list", description: "find overdue tasks" },
      { name: "tag", tool: "entity.update", description: "tag them urgent" },
    ],
    tool_calls: [
      { tool: "entity.list", decision: "allowed", ms: 12, ok: true },
      { tool: "entity.update", decision: "allowed", ms: 8, ok: true },
    ],
    refined: false,
  },
};

describe("formatTurnReplay", () => {
  it("reconstructs goal, plan stages, tool calls, and outcome step-by-step", () => {
    const text = formatTurnReplay(fixtureEvent);

    expect(text).toContain("run_id       : run_123");
    expect(text).toContain("outcome      : completed");
    expect(text).toContain("tokens       : in 18  out 16");
    expect(text).toContain("1. find (entity.list) - find overdue tasks");
    expect(text).toContain("2. tag (entity.update) - tag them urgent");
    expect(text).toContain("1. entity.list  [allowed]  12ms  ok");
    expect(text).toContain("2. entity.update  [allowed]  8ms  ok");
  });

  it("marks a failed tool call and a null plan clearly", () => {
    const event = {
      ...fixtureEvent,
      attrs: {
        goal: "delete the vcs history",
        plan: null,
        tool_calls: [{ tool: "vcs.deleteVersion", decision: "forbidden", ms: 0, ok: false }],
        refined: false,
      },
    };
    const text = formatTurnReplay(event);
    expect(text).toContain("plan: (no plan - direct execution)");
    expect(text).toContain("1. vcs.deleteVersion  [forbidden]  0ms  FAILED");
  });
});

describe("fetchTurnEvent", () => {
  it("returns the event row when found", async () => {
    const fetchFn = vi.fn(async (url) => {
      expect(url).toContain("run_id=run_123");
      expect(url).toContain("type=agent.turn");
      return { ok: true, json: async () => [fixtureEvent] };
    });

    const event = await fetchTurnEvent("run_123", { fetchFn, apiBase: "http://x", workspaceId: "ws" });
    expect(event).toEqual(fixtureEvent);
  });

  it("returns null when no event matches the run_id", async () => {
    const fetchFn = vi.fn(async () => ({ ok: true, json: async () => [] }));
    const event = await fetchTurnEvent("run_missing", { fetchFn, apiBase: "http://x", workspaceId: "ws" });
    expect(event).toBeNull();
  });

  it("returns null on a non-ok HTTP response", async () => {
    const fetchFn = vi.fn(async () => ({ ok: false, json: async () => [] }));
    const event = await fetchTurnEvent("run_123", { fetchFn, apiBase: "http://x", workspaceId: "ws" });
    expect(event).toBeNull();
  });
});
