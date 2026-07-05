import path from "node:path";
import { describe, expect, it } from "vitest";
import { createPreToolUseHook, isPathAllowed } from "../lib/preToolUseHook.js";

const TARGET = path.resolve("/repo/modules/foo");

describe("isPathAllowed", () => {
  it("allows the target directory itself", () => {
    expect(isPathAllowed(TARGET, TARGET)).toBe(true);
  });

  it("allows a file strictly under the target directory", () => {
    expect(isPathAllowed(TARGET, path.join(TARGET, "module.js"))).toBe(true);
    expect(isPathAllowed(TARGET, path.join(TARGET, "nested", "file.js"))).toBe(true);
  });

  it("denies a sibling module directory", () => {
    expect(isPathAllowed(TARGET, path.resolve("/repo/modules/bar/module.js"))).toBe(false);
  });

  it("denies a prefix-match trap (modules/foo_bar vs modules/foo)", () => {
    expect(isPathAllowed(TARGET, path.resolve("/repo/modules/foo_bar/module.js"))).toBe(false);
  });

  it("denies path traversal that resolves outside the target", () => {
    expect(isPathAllowed(TARGET, path.join(TARGET, "..", "..", "..", "etc", "passwd"))).toBe(false);
  });

  it("denies an absolute path elsewhere entirely", () => {
    expect(isPathAllowed(TARGET, "/etc/passwd")).toBe(false);
  });
});

describe("createPreToolUseHook", () => {
  it("returns {} (defer/allow) for a write inside the target dir", async () => {
    const hook = createPreToolUseHook(TARGET);
    const result = await hook({ tool_input: { file_path: path.join(TARGET, "module.js") } });
    expect(result).toEqual({});
  });

  it("denies with the documented hookSpecificOutput shape for an escape attempt", async () => {
    const hook = createPreToolUseHook(TARGET);
    const result = await hook({ tool_input: { file_path: "/etc/passwd" } });

    expect(result).toEqual({
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: "writes confined to the new module dir",
      },
    });
  });

  it("defers when there's no file_path to check (e.g. a Bash tool call)", async () => {
    const hook = createPreToolUseHook(TARGET);
    expect(await hook({ tool_input: { command: "echo hi" } })).toEqual({});
  });
});

// --- v2: per-tier glob scope (docs/SELF-EXTENSION-V2.md §3) ---
const ROOT = path.resolve("/repo");
const abs = (rel) => path.join(ROOT, rel);
const hookFor = (tier, params) => createPreToolUseHook({ tier, params, root: ROOT });

async function isDenied(hook, filePath) {
  const result = await hook({ tool_input: { file_path: filePath } });
  return result?.hookSpecificOutput?.permissionDecision === "deny";
}

describe("createPreToolUseHook - object (per-tier) scope", () => {
  it("T0 allows a write inside the module dir", async () => {
    const hook = hookFor("T0", { moduleId: "habits" });
    expect(await hook({ tool_input: { file_path: abs("modules/habits/module.js") } })).toEqual({});
  });

  it("T0 denies a write into frontend/ with a scope reason", async () => {
    const hook = hookFor("T0", { moduleId: "habits" });
    const result = await hook({ tool_input: { file_path: abs("frontend/src/core/renderers/GenericGraph.jsx") } });
    expect(result.hookSpecificOutput.permissionDecision).toBe("deny");
    expect(result.hookSpecificOutput.permissionDecisionReason).toMatch(/outside T0 write-scope/);
  });

  it("T1 allows its renderer but denies a write into modules/", async () => {
    const hook = hookFor("T1", { kind: "graph" });
    expect(await hook({ tool_input: { file_path: abs("frontend/src/core/renderers/GenericGraph.jsx") } })).toEqual({});
    expect(await isDenied(hook, abs("modules/x/module.js"))).toBe(true);
  });

  it("T3 denies a write into services/lifeos-vcs/", async () => {
    const hook = hookFor("T3", { crate: "lifeos-api" });
    expect(await isDenied(hook, abs("services/lifeos-vcs/src/history.rs"))).toBe(true);
  });

  it("defers a Bash tool call (no file_path) under a tier scope", async () => {
    const hook = hookFor("T0", { moduleId: "habits" });
    expect(await hook({ tool_input: { command: "echo hi" } })).toEqual({});
  });
});

describe("createPreToolUseHook - protected surfaces denied at EVERY tier", () => {
  const tiers = [
    ["T0", { moduleId: "habits" }],
    ["T1", { kind: "graph" }],
    ["T2", { name: "rMultiple" }],
    ["T3", { crate: "lifeos-api" }],
    ["T4", {}],
    ["T5", { crate: "lifeos-finance" }],
  ];
  const surfaces = [
    "server/lib/sandbox.js",
    "server/lib/preToolUseHook.js",
    "server/lib/tierScopes.js",
    "server/validators/registry.js",
    "server/agent/actionRegistry.js",
    "frontend/src/lib/capabilities.js",
    "frontend/src/lib/agentActions.js",
    "services/broker-guard/src/main.rs",
    "services/lifeos-api/src/routes/orders.rs",
    "infra/nango/docker-compose.yml",
    "migrations/0002_control_plane.sql",
    "services/lifeos-vcs/src/gc.rs",
    ".github/workflows/ci.yml",
    ".claude/settings.json",
  ];

  for (const [tier, params] of tiers) {
    for (const surface of surfaces) {
      it(`${tier} denies ${surface} with a protected-surface reason`, async () => {
        const hook = hookFor(tier, params);
        const result = await hook({ tool_input: { file_path: abs(surface) } });
        expect(result.hookSpecificOutput.permissionDecision).toBe("deny");
        expect(result.hookSpecificOutput.permissionDecisionReason).toMatch(/protected surface/);
      });
    }
  }
});

describe("createPreToolUseHook - traversal + absolute-path hardening", () => {
  it("denies a plain ../ escape out of the root", async () => {
    const hook = hookFor("T0", { moduleId: "x" });
    expect(await isDenied(hook, abs("modules/x/../../../etc/passwd"))).toBe(true);
  });

  it("denies an absolute path elsewhere entirely", async () => {
    const hook = hookFor("T0", { moduleId: "x" });
    expect(await isDenied(hook, "/etc/passwd")).toBe(true);
  });

  it("denies a traversal that resolves back onto a protected surface", async () => {
    const hook = hookFor("T0", { moduleId: "x" });
    const result = await hook({ tool_input: { file_path: abs("modules/x/../../server/lib/sandbox.js") } });
    expect(result.hookSpecificOutput.permissionDecision).toBe("deny");
    expect(result.hookSpecificOutput.permissionDecisionReason).toMatch(/protected surface/);
  });
});
