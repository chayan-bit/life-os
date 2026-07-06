// Issue #146: the executor's single chokepoint (runTool) consults ROLE_CAPS
// ONLY when a role is present in ctx. Absent a role, behavior is unchanged
// (owner reach); with a role, a classification the role may not execute is
// demoted to `forbidden` before any HTTP side effect fires.
import { describe, expect, it, vi } from "vitest";
import { runTool } from "../agent/executor.js";

// A ctx skeleton matching what the loop builds, with an injectable httpFn so no
// call ever touches the network.
function makeCtx(overrides = {}) {
  return {
    workspaceId: "ws_test",
    httpFn: vi.fn(async () => ({ ok: true, data: { id: "ent_x" } })),
    dryRun: false,
    ledger: [],
    pendingApprovals: [],
    stepCount: 0,
    recoveryBudget: 3,
    recoveries: [],
    ...overrides,
  };
}

describe("executor ROLE_CAPS consult (issue #146)", () => {
  it("runs an allowed tool when no role is present (backward compatible)", async () => {
    const ctx = makeCtx();
    const result = await runTool(ctx, "entity.create", { module: "tasks", type: "task" });
    expect(result.status).toBe("ok");
    expect(ctx.httpFn).toHaveBeenCalled();
  });

  it("forbids a viewer from an allowed write and never calls the API", async () => {
    const ctx = makeCtx({ role: "viewer" });
    const result = await runTool(ctx, "entity.create", { module: "tasks", type: "task" });
    expect(result.status).toBe("forbidden");
    // Only the best-effort action.denied ledger POST may fire - never the
    // entity write itself.
    expect(ctx.httpFn.mock.calls.every(([, path]) => path !== "/api/entity")).toBe(true);
  });

  it("lets an agent run allowed tools but forbids gated ones", async () => {
    const allowedCtx = makeCtx({ role: "agent" });
    const allowed = await runTool(allowedCtx, "entity.create", { module: "tasks", type: "task" });
    expect(allowed.status).toBe("ok");

    const gatedCtx = makeCtx({ role: "agent" });
    const gated = await runTool(gatedCtx, "draft.create", { title: "hi" });
    expect(gated.status).toBe("forbidden");
    expect(gatedCtx.pendingApprovals).toHaveLength(0);
  });

  it("lets an editor draft (gated) content", async () => {
    const ctx = makeCtx({ role: "editor" });
    const result = await runTool(ctx, "draft.create", { title: "hi" });
    expect(result.status).toBe("pending_approval");
  });
});
