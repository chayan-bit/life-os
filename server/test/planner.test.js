import { describe, expect, it, vi } from "vitest";
import {
  choosePlannerVariant,
  generatePlan,
  needsPlanning,
  PLANNER_PROMPT_GROUP,
  PLANNER_PROMPT_VARIANTS,
  planJsonSchema,
  PlanSchema,
} from "../agent/planner.js";

// Minimal fake HTTP layer mirroring the other agent/test files: records every
// call, replies from an optional route list, defaults to a benign 200/[].
function makeHttp(routes = []) {
  const calls = [];
  const httpFn = vi.fn(async (method, path, body) => {
    calls.push({ method, path, body });
    for (const route of routes) {
      if (route.match(method, path, body)) return route.reply(method, path, body);
    }
    if (method === "GET") return { ok: true, status: 200, data: [] };
    return { ok: true, status: 200, data: { id: `ent_${calls.length}` } };
  });
  httpFn.calls = calls;
  return httpFn;
}

function makeQueryFn(plan) {
  const prompts = [];
  const queryFn = vi.fn(async function* ({ prompt }) {
    prompts.push(prompt);
    yield {
      type: "result",
      structured_output: plan ?? { stages: [{ name: "step", tool: null, description: "do it" }] },
      usage: { input_tokens: 5, output_tokens: 5 },
    };
  });
  queryFn.prompts = prompts;
  return queryFn;
}

describe("needsPlanning heuristic", () => {
  it("is unchanged by the #156 strategy wiring - still a pure deterministic check", () => {
    expect(needsPlanning("find my overdue tasks, tag them urgent, and draft a summary")).toBe(true);
    expect(needsPlanning("what is the CAP theorem?")).toBe(false);
  });
});

describe("choosePlannerVariant (#156, group: planner.prompt)", () => {
  function makeOutcomeHttp(outcomeRows) {
    return makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("type=agent.strategy.outcome"),
        reply: () => ({ ok: true, status: 200, data: outcomeRows }),
      },
    ]);
  }

  it("picks the cold (unseen) variant deterministically when the other has a logged play", async () => {
    const httpFn = makeOutcomeHttp([{ attrs: { group: PLANNER_PROMPT_GROUP, variant: "stages", success: true } }]);

    const variant = await choosePlannerVariant({ httpFn, workspaceId: "ws_test" });

    expect(variant).toBe("checklist");
  });

  it("defaults to the first declared variant when the group has no logged outcomes yet", async () => {
    const httpFn = makeOutcomeHttp([]);

    const variant = await choosePlannerVariant({ httpFn, workspaceId: "ws_test" });

    expect(variant).toBe(PLANNER_PROMPT_VARIANTS[0]);
  });
});

describe("generatePlan - planner.prompt variant wiring (#156)", () => {
  it("chooses a variant once, stamps it on ctx.plannerVariant, and reuses it on a second call in the same turn", async () => {
    const httpFn = makeHttp([
      {
        match: (m, p) => m === "GET" && p.includes("type=agent.strategy.outcome"),
        reply: () => ({
          ok: true,
          status: 200,
          data: [{ attrs: { group: PLANNER_PROMPT_GROUP, variant: "stages", success: true } }],
        }),
      },
    ]);
    const queryFn = makeQueryFn();
    const ctx = { httpFn, workspaceId: "ws_test", queryFn };

    await generatePlan("do the thing", "world snapshot", ctx);
    expect(ctx.plannerVariant).toBe("checklist");

    const outcomeCallsAfterFirst = httpFn.calls.filter((c) => c.path.includes("type=agent.strategy.outcome")).length;
    await generatePlan("do the thing again", "world snapshot", ctx);

    expect(ctx.plannerVariant).toBe("checklist");
    // No second chooseVariant lookup - the already-chosen variant is reused
    // for the rest of the turn (e.g. a replan's own generatePlan call).
    const outcomeCallsAfterSecond = httpFn.calls.filter((c) => c.path.includes("type=agent.strategy.outcome")).length;
    expect(outcomeCallsAfterSecond).toBe(outcomeCallsAfterFirst);
  });

  it("both variants request the identical stages/tool/description JSON contract - no behavior regression", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn();

    await generatePlan("goal", "snapshot", { httpFn, workspaceId: "ws_a", queryFn, plannerVariant: "stages" });
    const stagesPrompt = queryFn.prompts[0];
    await generatePlan("goal", "snapshot", { httpFn, workspaceId: "ws_b", queryFn, plannerVariant: "checklist" });
    const checklistPrompt = queryFn.prompts[1];

    expect(stagesPrompt).not.toBe(checklistPrompt);
    for (const p of [stagesPrompt, checklistPrompt]) {
      expect(p).toMatch(/name/);
      expect(p).toMatch(/tool/);
      expect(p).toMatch(/description/);
    }
  });

  it("still throws on malformed structured output regardless of variant", async () => {
    const httpFn = makeHttp();
    const queryFn = makeQueryFn({ notAPlan: true });

    await expect(
      generatePlan("goal", "snapshot", { httpFn, workspaceId: "ws_test", queryFn, plannerVariant: "checklist" }),
    ).rejects.toThrow(/planner structured output invalid/);
  });
});

describe("PlanSchema / planJsonSchema", () => {
  it("stay identical across both prompt variants (schema itself never varies by variant)", () => {
    expect(planJsonSchema.required).toEqual(["stages"]);
    expect(() => PlanSchema.parse({ stages: [{ name: "a", tool: null, description: "b" }] })).not.toThrow();
  });
});
