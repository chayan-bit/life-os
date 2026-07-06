// Finding 43 (test/CI audit): buildSandboxConfig is the kernel backstop for
// every Bash child a build agent spawns (docs/SELF-EXTENSION.md/-V2.md §3) -
// it had no direct test locking in its fail-closed shape. This asserts the
// exact contract scaffold.js/build/node.js/build/t5Subsystem.js rely on:
// sandboxing is on and fails closed rather than silently running
// unsandboxed, the credential deny-list is present, and allowWriteDirs is
// passed through verbatim (the per-tier scoping in tierScopes.js is tested
// separately in tierScopes.test.js).
import { describe, expect, it } from "vitest";
import { buildSandboxConfig } from "../lib/sandbox.js";

describe("buildSandboxConfig - fail-closed shape", () => {
  it("enables the sandbox and refuses to run unsandboxed if Seatbelt is unavailable", () => {
    const config = buildSandboxConfig();

    expect(config.sandbox.enabled).toBe(true);
    expect(config.sandbox.failIfUnavailable).toBe(true);
    expect(config.sandbox.allowUnsandboxedCommands).toBe(false);
  });

  it("denies the credential paths and env vars a build agent must never read", () => {
    const config = buildSandboxConfig();

    expect(config.sandbox.credentials.files).toEqual(
      expect.arrayContaining([
        { path: "~/.aws", mode: "deny" },
        { path: "~/.ssh", mode: "deny" },
      ]),
    );
    expect(config.sandbox.credentials.envVars).toEqual(
      expect.arrayContaining([
        { name: "GITHUB_TOKEN", mode: "deny" },
        { name: "NPM_TOKEN", mode: "deny" },
      ]),
    );
  });

  it("defaults allowWriteDirs to Tier 0's ./modules when no scope is passed", () => {
    const config = buildSandboxConfig();

    expect(config.sandbox.filesystem.allowWrite).toEqual(["./modules"]);
  });

  it("passes a caller-supplied tier scope through verbatim, not the T0 default", () => {
    const t2Scope = ["server/agent/tools/generated/reading.save.js"];

    const config = buildSandboxConfig(t2Scope);

    expect(config.sandbox.filesystem.allowWrite).toEqual(t2Scope);
    expect(config.sandbox.filesystem.allowWrite).not.toEqual(["./modules"]);
  });

  it("scopes a multi-file tier (e.g. T1's renderer + registration files) to exactly those paths", () => {
    const t1Scope = [
      "frontend/src/core/renderers/GenericBoard.jsx",
      "frontend/src/core/ModuleManifestPage.jsx",
      "frontend/src/core/rendererKinds.js",
    ];

    const config = buildSandboxConfig(t1Scope);

    expect(config.sandbox.filesystem.allowWrite).toHaveLength(3);
    expect(config.sandbox.filesystem.allowWrite).toEqual(t1Scope);
  });

  it("still fails closed and keeps the credential deny-list regardless of the write scope passed in", () => {
    const config = buildSandboxConfig(["services/lifeos-api/src/routes/reading.rs"]);

    expect(config.sandbox.failIfUnavailable).toBe(true);
    expect(config.sandbox.credentials.envVars).toEqual(
      expect.arrayContaining([{ name: "GITHUB_TOKEN", mode: "deny" }]),
    );
  });
});
