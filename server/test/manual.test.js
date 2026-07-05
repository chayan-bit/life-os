import { describe, expect, it, vi } from "vitest";
import { fetchActiveManual } from "../agent/manual.js";

describe("fetchActiveManual", () => {
  it("returns the labeled block for the active agent_manual config", async () => {
    const httpFn = vi.fn(async () => ({
      ok: true,
      status: 200,
      data: {
        configs: [
          { id: "cfg_1", kind: "agent_manual", payload: { text: "Always lead with the TLDR." }, status: "promoted" },
          { id: "cfg_0", kind: "agent_manual", payload: { text: "stale draft" }, status: "draft" },
        ],
        active: { agent_manual: "cfg_1" },
      },
    }));

    const block = await fetchActiveManual(httpFn, "ws_test");

    expect(httpFn).toHaveBeenCalledWith("GET", expect.stringContaining("/api/configs?"));
    const [, path] = httpFn.mock.calls[0];
    expect(path).toContain("kind=agent_manual");
    expect(path).toContain("workspace_id=ws_test");
    expect(block).toContain("## Operating manual");
    expect(block).toContain("Always lead with the TLDR.");
    expect(block).not.toContain("stale draft");
  });

  it("returns null cleanly when no manual has ever been promoted", async () => {
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: { configs: [], active: {} } }));

    const block = await fetchActiveManual(httpFn, "ws_test");

    expect(block).toBeNull();
  });

  it("returns null on a fetch failure, never throwing", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("lifeos-api unavailable");
    });

    await expect(fetchActiveManual(httpFn, "ws_test")).resolves.toBeNull();
  });

  it("returns null on a non-ok response", async () => {
    const httpFn = vi.fn(async () => ({ ok: false, status: 500, data: null }));

    const block = await fetchActiveManual(httpFn, "ws_test");

    expect(block).toBeNull();
  });
});
