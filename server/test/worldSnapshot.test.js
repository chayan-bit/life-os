import { describe, expect, it, vi } from "vitest";
import { buildWorldSnapshot } from "../agent/worldSnapshot.js";

// Seeds one canned response per query the snapshot makes, keyed by a path
// substring, mirroring agent.test.js's routing-fake style.
function makeHttp(seeds) {
  return vi.fn(async (method, path) => {
    for (const [match, data] of seeds) {
      if (path.includes(match)) return { ok: true, status: 200, data };
    }
    return { ok: true, status: 200, data: [] };
  });
}

const NOW_SECS = Date.UTC(2026, 6, 5) / 1000 + 3600; // 2026-07-05, mid-day UTC

describe("buildWorldSnapshot", () => {
  it("assembles counts for each category from seeded entity/job responses", async () => {
    const httpFn = makeHttp([
      [
        "module=tasks",
        [
          { id: "t1", status: "open", attrs: { due: "2026-07-01" } }, // overdue
          { id: "t2", status: "open", attrs: {} }, // undated, not due
          { id: "t3", status: "done", attrs: { due: "2026-07-01" } }, // done, excluded
        ],
      ],
      [
        "module=trading",
        [
          { id: "tr1", attrs: { closed_at: null } }, // open
          { id: "tr2", attrs: { closed_at: 12345 } }, // closed
        ],
      ],
      ["status=pending_approval", [{ id: "d1" }, { id: "d2" }]],
      ["/api/jobs", [{ id: "j1" }]],
    ]);

    const block = await buildWorldSnapshot({ httpFn, nowSecs: NOW_SECS });

    expect(block).toContain("open tasks: 2");
    expect(block).toContain("open trades: 1");
    expect(block).toContain("drafts / pending approvals: 2");
    expect(block).toContain("pending jobs: 1");
    expect(block).toContain("tasks due today or overdue: 1");
  });

  it("treats a JSON-string attrs blob the same as an object", async () => {
    const httpFn = makeHttp([
      ["module=tasks", [{ id: "t1", status: "open", attrs: JSON.stringify({ due: "2026-07-01" }) }]],
    ]);

    const block = await buildWorldSnapshot({ httpFn, nowSecs: NOW_SECS });

    expect(block).toContain("tasks due today or overdue: 1");
  });

  it("returns null when the underlying reads throw, so the turn proceeds without it", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("api down");
    });

    await expect(buildWorldSnapshot({ httpFn, nowSecs: NOW_SECS })).resolves.toBeNull();
  });

  it("degrades a single failed category to zero rather than failing the whole snapshot", async () => {
    const httpFn = vi.fn(async (method, path) => {
      if (path.includes("module=tasks")) throw new Error("tasks route down");
      return { ok: true, status: 200, data: [] };
    });

    const block = await buildWorldSnapshot({ httpFn, nowSecs: NOW_SECS });

    expect(block).toContain("open tasks: 0");
  });
});
