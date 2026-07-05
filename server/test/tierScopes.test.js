import path from "node:path";
import { describe, expect, it } from "vitest";
import {
  TIER_SCOPES,
  PROTECTED_SURFACES,
  toRepoRelative,
  isProtectedPath,
  isWriteAllowed,
  evaluateWrite,
  scopeDirs,
} from "../lib/tierScopes.js";

const ROOT = path.resolve("/repo");
const abs = (rel) => path.join(ROOT, rel);

describe("toRepoRelative", () => {
  it("returns a POSIX repo-relative path for a file inside the root", () => {
    expect(toRepoRelative(ROOT, abs("modules/foo/module.js"))).toBe("modules/foo/module.js");
  });

  it("returns null for the root itself (no file to write)", () => {
    expect(toRepoRelative(ROOT, ROOT)).toBe(null);
  });

  it("returns null for a traversal that escapes the root", () => {
    expect(toRepoRelative(ROOT, abs("modules/foo/../../../etc/passwd"))).toBe(null);
  });

  it("returns null for an absolute path elsewhere entirely", () => {
    expect(toRepoRelative(ROOT, "/etc/passwd")).toBe(null);
  });

  it("normalizes an in-root traversal back to a clean relative path", () => {
    expect(toRepoRelative(ROOT, abs("modules/foo/../foo/module.js"))).toBe("modules/foo/module.js");
  });
});

describe("TIER_SCOPES glob shapes", () => {
  it("T0 confines to the one module dir", () => {
    expect(TIER_SCOPES.T0({ moduleId: "habits" })).toEqual(["modules/habits/**"]);
  });

  it("T1 targets the single Generic<Kind> renderer + its registration", () => {
    expect(TIER_SCOPES.T1({ kind: "graph" })).toEqual([
      "frontend/src/core/renderers/GenericGraph.jsx",
      "frontend/src/core/ModuleManifestPage.jsx",
    ]);
  });

  it("T5 targets the new crate dir + the workspace Cargo.toml", () => {
    expect(TIER_SCOPES.T5({ crate: "lifeos-finance" })).toEqual([
      "services/lifeos-finance/**",
      "services/Cargo.toml",
    ]);
  });
});

describe("isWriteAllowed - per-tier allow", () => {
  it("T0 allows a write inside its module dir", () => {
    expect(isWriteAllowed("T0", { moduleId: "habits" }, abs("modules/habits/module.js"), ROOT)).toBe(true);
  });

  it("T0 denies a write into a sibling module", () => {
    expect(isWriteAllowed("T0", { moduleId: "habits" }, abs("modules/tasks/module.js"), ROOT)).toBe(false);
  });

  it("T0 denies a write into frontend/", () => {
    expect(isWriteAllowed("T0", { moduleId: "habits" }, abs("frontend/src/core/renderers/GenericGraph.jsx"), ROOT)).toBe(false);
  });

  it("T1 allows its renderer but denies a write into modules/", () => {
    expect(isWriteAllowed("T1", { kind: "graph" }, abs("frontend/src/core/renderers/GenericGraph.jsx"), ROOT)).toBe(true);
    expect(isWriteAllowed("T1", { kind: "graph" }, abs("modules/x/module.js"), ROOT)).toBe(false);
  });

  it("T3 allows its crate routes but denies a write into lifeos-vcs", () => {
    expect(isWriteAllowed("T3", { crate: "lifeos-api" }, abs("services/lifeos-api/src/routes/week.rs"), ROOT)).toBe(true);
    expect(isWriteAllowed("T3", { crate: "lifeos-api" }, abs("services/lifeos-vcs/src/history.rs"), ROOT)).toBe(false);
  });

  it("fails closed for an unknown tier", () => {
    expect(isWriteAllowed("T9", {}, abs("modules/x/module.js"), ROOT)).toBe(false);
  });

  it("fails closed for a path that escapes the root", () => {
    expect(isWriteAllowed("T0", { moduleId: "habits" }, "/etc/passwd", ROOT)).toBe(false);
  });
});

describe("isWriteAllowed - every protected surface denied at EVERY tier", () => {
  const tierCases = [
    ["T0", { moduleId: "habits" }],
    ["T1", { kind: "graph" }],
    ["T2", { moduleId: "habits", crate: "lifeos-cli" }],
    ["T3", { crate: "lifeos-api" }],
    ["T4", {}],
    ["T5", { crate: "lifeos-finance" }],
  ];
  const protectedProbes = [
    "server/lib/sandbox.js",
    "server/lib/preToolUseHook.js",
    "server/lib/tierScopes.js",
    "server/validators/registry.js",
    "server/agent/actionRegistry.js",
    "frontend/src/lib/capabilities.js",
    "frontend/src/lib/capabilityMatrix.js",
    "frontend/src/lib/agentActions.js",
    "frontend/src/lib/actionPlanCompiler.js",
    "services/broker-guard/src/main.rs",
    "services/lifeos-api/src/routes/orders.rs",
    "services/lifeos-api/src/place_order.rs",
    "infra/nango/docker-compose.yml",
    "migrations/0002_control_plane.sql",
    "migrations/0011_workspace_envelope_key.sql",
    "services/lifeos-vcs/src/gc.rs",
    ".github/workflows/ci.yml",
    ".claude/settings.json",
  ];

  for (const [tier, params] of tierCases) {
    for (const probe of protectedProbes) {
      it(`${tier} denies protected surface ${probe}`, () => {
        expect(isWriteAllowed(tier, params, abs(probe), ROOT)).toBe(false);
      });
    }
  }
});

describe("evaluateWrite - reason naming", () => {
  it("names the protected surface that fired (deny wins over a tier allow)", () => {
    // T4's scope names migrations/**, yet a protected migration still denies.
    const verdict = evaluateWrite({ tier: "T4", params: {}, root: ROOT }, abs("migrations/0002_control_plane.sql"));
    expect(verdict.allowed).toBe(false);
    expect(verdict.reason).toMatch(/protected surface/);
    expect(verdict.reason).toMatch(/0002_control_plane\.sql/);
  });

  it("allows an additive migration that is not protected", () => {
    const verdict = evaluateWrite({ tier: "T4", params: {}, root: ROOT }, abs("migrations/0099_habit_streak.sql"));
    expect(verdict.allowed).toBe(true);
  });

  it("reports an outside-scope denial with the tier named", () => {
    const verdict = evaluateWrite({ tier: "T0", params: { moduleId: "habits" }, root: ROOT }, abs("frontend/src/App.jsx"));
    expect(verdict.allowed).toBe(false);
    expect(verdict.reason).toMatch(/outside T0 write-scope/);
  });

  it("denies a traversal that lexically escapes into a protected surface", () => {
    const verdict = evaluateWrite(
      { tier: "T0", params: { moduleId: "x" }, root: ROOT },
      abs("modules/x/../../server/lib/sandbox.js"),
    );
    expect(verdict.allowed).toBe(false);
    // Resolves back inside the root to a protected surface, so it's caught.
    expect(verdict.reason).toMatch(/protected surface/);
  });
});

describe("isProtectedPath", () => {
  it("matches broker-guard anywhere", () => {
    expect(isProtectedPath("services/broker-guard/src/lib.rs")).toBe(true);
  });

  it("does not match an ordinary generated file", () => {
    expect(isProtectedPath("modules/habits/module.js")).toBe(false);
  });

  it("has a non-empty protected list", () => {
    expect(PROTECTED_SURFACES.length).toBeGreaterThan(0);
  });
});

describe("scopeDirs - Seatbelt allowWrite derivation", () => {
  it("derives the static prefix dir for a glob scope", () => {
    expect(scopeDirs("T0", { moduleId: "habits" })).toEqual(["./modules/habits"]);
  });

  it("collapses concrete files to their parent dir and dedupes", () => {
    expect(scopeDirs("T1", { kind: "graph" })).toEqual([
      "./frontend/src/core/renderers",
      "./frontend/src/core",
    ]);
  });

  it("derives crate dir + services for T5", () => {
    expect(scopeDirs("T5", { crate: "lifeos-finance" })).toEqual([
      "./services/lifeos-finance",
      "./services",
    ]);
  });

  it("returns [] for an unknown tier", () => {
    expect(scopeDirs("T9", {})).toEqual([]);
  });
});
