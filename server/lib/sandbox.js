// Layer C (docs/SELF-EXTENSION.md §2, docs/SELF-EXTENSION-V2.md §3) - the macOS
// Seatbelt kernel backstop for any Bash child the agent spawns. Read/Edit/Write
// bypass Seatbelt entirely (that's why Layer B's PreToolUse hook is the real
// guarantee for file writes) - this only confines what a shell subprocess can
// touch. `failIfUnavailable: true` makes the build refuse to run rather than
// silently proceed unsandboxed if Seatbelt can't initialize.
//
// `allowWriteDirs` is parameterized by tier (docs/SELF-EXTENSION-V2.md §3):
// callers pass the tier's top-level writable dirs (see `scopeDirs` in
// tierScopes.js), defaulting to Tier 0's `./modules` so pre-v2 callers are
// unchanged. Seatbelt takes directories, not globs, so the per-file glob
// precision lives in Layer B; this is the coarse kernel fence around it.
export function buildSandboxConfig(allowWriteDirs = ["./modules"]) {
  return {
    sandbox: {
      enabled: true,
      failIfUnavailable: true,
      allowUnsandboxedCommands: false,
      filesystem: { allowWrite: allowWriteDirs },
      credentials: {
        files: [
          { path: "~/.aws", mode: "deny" },
          { path: "~/.ssh", mode: "deny" },
        ],
        envVars: [
          { name: "GITHUB_TOKEN", mode: "deny" },
          { name: "NPM_TOKEN", mode: "deny" },
        ],
      },
    },
  };
}
