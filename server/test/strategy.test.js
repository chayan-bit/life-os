import { describe, expect, it, vi } from "vitest";
import { chooseVariant, DEFAULT_EPSILON, leaderboard, loadOutcomes, recordOutcome } from "../agent/strategy.js";

function makeOutcome(group, variant, success) {
  return { group, variant, success };
}

describe("recordOutcome", () => {
  it("posts an agent.strategy.outcome event with the group/variant/success shape", async () => {
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: { id: "evt_1" } }));

    await recordOutcome(httpFn, "ws_test", "draft_tone", "casual", true);

    expect(httpFn).toHaveBeenCalledWith("POST", "/api/event", {
      type: "agent.strategy.outcome",
      actor: "agent",
      attrs: { group: "draft_tone", variant: "casual", success: true },
      workspace_id: "ws_test",
    });
  });

  it("is best-effort - a failing httpFn never throws", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("network down");
    });

    await expect(recordOutcome(httpFn, "ws_test", "draft_tone", "casual", false)).resolves.toBeUndefined();
  });
});

describe("loadOutcomes", () => {
  it("filters the type-scoped fetch down to the requested group", async () => {
    const httpFn = vi.fn(async (method, path) => {
      expect(method).toBe("GET");
      expect(path).toContain("type=agent.strategy.outcome");
      expect(path).toContain("workspace_id=ws_test");
      return {
        ok: true,
        status: 200,
        data: [
          { attrs: { group: "draft_tone", variant: "casual", success: true } },
          { attrs: { group: "other_group", variant: "x", success: true } },
          { attrs: { group: "draft_tone", variant: "formal", success: false } },
        ],
      };
    });

    const outcomes = await loadOutcomes(httpFn, "ws_test", "draft_tone");

    expect(outcomes).toEqual([
      { group: "draft_tone", variant: "casual", success: true },
      { group: "draft_tone", variant: "formal", success: false },
    ]);
  });

  it("returns an empty array on a failing httpFn instead of throwing", async () => {
    const httpFn = vi.fn(async () => {
      throw new Error("network down");
    });

    await expect(loadOutcomes(httpFn, "ws_test", "draft_tone")).resolves.toEqual([]);
  });

  it("returns an empty array when the response is not ok", async () => {
    const httpFn = vi.fn(async () => ({ ok: false, status: 500, data: null }));

    expect(await loadOutcomes(httpFn, "ws_test", "draft_tone")).toEqual([]);
  });
});

describe("chooseVariant", () => {
  it("explores any unseen variant first, in variants order", () => {
    const outcomes = [makeOutcome("g", "a", true), makeOutcome("g", "a", true)];
    const rng = () => 0.99; // would neither explore-by-epsilon nor matter

    expect(chooseVariant(outcomes, ["a", "b", "c"], { rng })).toBe("b");
  });

  it("explores uniformly at random when rng falls under epsilon", () => {
    const outcomes = [
      makeOutcome("g", "a", true),
      makeOutcome("g", "a", true),
      makeOutcome("g", "b", false),
      makeOutcome("g", "b", false),
    ];
    // First rng() call gates epsilon (< 0.1 -> explore); second picks the
    // uniform index: 0.5 * 2 variants = index 1 -> "b".
    const calls = [0.05, 0.5];
    const rng = () => calls.shift();

    expect(chooseVariant(outcomes, ["a", "b"], { epsilon: DEFAULT_EPSILON, rng })).toBe("b");
  });

  it("exploits the best success rate once all variants are seen and epsilon does not fire", () => {
    const outcomes = [
      makeOutcome("g", "a", true),
      makeOutcome("g", "a", false),
      makeOutcome("g", "b", true),
      makeOutcome("g", "b", true),
    ];
    const rng = () => 0.99; // above epsilon - exploit branch

    expect(chooseVariant(outcomes, ["a", "b"], { rng })).toBe("b");
  });

  it("breaks ties deterministically to the first variant in order", () => {
    const outcomes = [
      makeOutcome("g", "a", true),
      makeOutcome("g", "a", false),
      makeOutcome("g", "b", true),
      makeOutcome("g", "b", false),
    ];
    const rng = () => 0.99;

    expect(chooseVariant(outcomes, ["a", "b"], { rng })).toBe("a");
  });

  it("defaults rng to Math.random when not injected", () => {
    const outcomes = [makeOutcome("g", "a", true), makeOutcome("g", "b", true)];

    const result = chooseVariant(outcomes, ["a", "b"]);

    expect(["a", "b"]).toContain(result);
  });
});

describe("leaderboard", () => {
  it("aggregates plays/successes/rate per variant, sorted by rate desc then plays desc", () => {
    const outcomes = [
      makeOutcome("g", "a", true),
      makeOutcome("g", "a", false),
      makeOutcome("g", "b", true),
      makeOutcome("g", "b", true),
      makeOutcome("g", "c", true),
      makeOutcome("other", "z", true),
    ];

    const board = leaderboard(outcomes, "g");

    expect(board).toEqual([
      { variant: "b", plays: 2, successes: 2, rate: 1 },
      { variant: "c", plays: 1, successes: 1, rate: 1 },
      { variant: "a", plays: 2, successes: 1, rate: 0.5 },
    ]);
  });

  it("returns an empty leaderboard for a group with no logged outcomes", () => {
    expect(leaderboard([], "unseen_group")).toEqual([]);
  });
});

describe("simulated convergence", () => {
  it("shifts selection frequency toward the best variant over many rounds while still occasionally exploring", () => {
    const variants = ["a", "b"];
    const BEST_VARIANT = "b";
    const BEST_SUCCESS_RATE = 0.9;
    const WORST_SUCCESS_RATE = 0.2;
    const ROUNDS = 200;

    // A deterministic seeded PRNG (mulberry32) so the test is fully
    // reproducible - no reliance on Math.random.
    function mulberry32(seed) {
      let a = seed;
      return function rng() {
        a |= 0;
        a = (a + 0x6d2b79f5) | 0;
        let t = Math.imul(a ^ (a >>> 15), 1 | a);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
      };
    }
    const rng = mulberry32(42);
    const oracleRng = mulberry32(1337);
    const successRateFor = (variant) => (variant === BEST_VARIANT ? BEST_SUCCESS_RATE : WORST_SUCCESS_RATE);

    let outcomes = [];
    const counts = { a: 0, b: 0 };
    for (let round = 0; round < ROUNDS; round += 1) {
      const chosen = chooseVariant(outcomes, variants, { rng });
      counts[chosen] += 1;
      const success = oracleRng() < successRateFor(chosen);
      outcomes = [...outcomes, makeOutcome("conv", chosen, success)];
    }

    expect(counts[BEST_VARIANT]).toBeGreaterThan(counts[BEST_VARIANT === "a" ? "b" : "a"]);
    // Still exploring the non-best variant occasionally, not converged to 0.
    const worstCount = counts[BEST_VARIANT === "a" ? "b" : "a"];
    expect(worstCount).toBeGreaterThan(0);
  });
});
