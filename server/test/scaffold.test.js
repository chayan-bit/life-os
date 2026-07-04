import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { scaffoldModule } from "../scaffold.js";

const execFile = promisify(execFileCb);
const REAL_TEMPLATE = path.resolve(import.meta.dirname, "..", "..", "modules", "_template");

let repoRoot;

async function git(args, cwd = repoRoot) {
  return execFile("git", args, { cwd });
}

beforeEach(async () => {
  repoRoot = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-scaffold-repo-"));
  await git(["init", "-b", "main"]);
  await git(["config", "user.email", "test@example.com"]);
  await git(["config", "user.name", "Test"]);

  await fs.mkdir(path.join(repoRoot, "modules"), { recursive: true });
  await fs.cp(REAL_TEMPLATE, path.join(repoRoot, "modules", "_template"), { recursive: true });
  await git(["add", "modules"]);
  await git(["commit", "-m", "seed _template"]);
});

afterEach(async () => {
  await fs.rm(repoRoot, { recursive: true, force: true });
});

const VALID_MANIFEST = {
  id: "add_a_reading_list_module",
  name: "Reading List",
  icon: "BookOpen",
  color: "var(--neo-yellow)",
  entityTypes: [
    {
      id: "item",
      label: "Item",
      plural: "Items",
      icon: "FileText",
      attrs: { name: { type: "text", required: true } },
    },
  ],
  views: [{ id: "all", label: "All Items", kind: "list", type: "item" }],
  botCommands: [{ cmd: "add", help: "Add a reading list item" }],
  agentTools: [{ name: "reading_list.add", gated: false }],
};

const VALID_MODULE_SOURCE = `osRegisterModule({
  id: "add_a_reading_list_module",
  name: "Reading List",
  icon: "BookOpen",
  color: "var(--neo-yellow)",
  entityTypes: {
    item: {
      label: "Item",
      plural: "Items",
      icon: "FileText",
      attrs: { name: { type: "text", required: true } },
    },
  },
  views: [{ id: "all", label: "All Items", kind: "list", type: "item" }],
  botCommands: [{ cmd: "add", help: "Add a reading list item", handler: "handleAdd" }],
  agentTools: [{ name: "reading_list.add", schema: {}, impl: "handleAdd", gated: false }],
});
`;

// A benign mock agent that behaves like a well-behaved real one: it edits
// modules/<id>/module.js (seeded from _template by scaffold.js's own copy
// step) to satisfy the request, then reports success with a schema-valid
// structured_output manifest (issue #73) - which Validator 1 (#74) then
// re-loads and checks against the file the "agent" actually wrote.
async function* benignQuery(params) {
  await fs.writeFile(path.join(params.options.cwd, "modules", "add_a_reading_list_module", "module.js"), VALID_MODULE_SOURCE, "utf8");
  yield { type: "result", subtype: "success", is_error: false, structured_output: VALID_MANIFEST };
}

describe("scaffoldModule - happy path", () => {
  it("commits the scaffolded module to main and cleans up the worktree", async () => {
    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: (params) => benignQuery(params),
      // Validator 2 (#75) boots the real app stack - covered on its own in
      // renderSmoke.test.js; this suite only exercises scaffold.js's
      // orchestration, so a stub keeps it fast and independent of a local
      // cargo/frontend build being present.
      validateRenderSmoke: async () => ({ valid: true, errors: [] }),
    });

    expect(result).toEqual({
      success: true,
      moduleId: "add_a_reading_list_module",
      workspaceId: "ws_test",
      manifest: VALID_MANIFEST,
    });

    const installed = await fs.readFile(path.join(repoRoot, "modules", "add_a_reading_list_module", "module.js"), "utf8");
    expect(installed).toContain("osRegisterModule");

    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1); // only the main worktree remains
  });
});

describe("scaffoldModule - manifest entity persistence (issue #121)", () => {
  it("persists the real object-shaped module.js manifest, not the array-shaped structured-output summary", async () => {
    const calls = [];
    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: (params) => benignQuery(params),
      validateRenderSmoke: async () => ({ valid: true, errors: [] }),
      persistManifestEntity: async (apiBase, moduleId, manifest, opts) => {
        calls.push({ apiBase, moduleId, manifest, opts });
      },
    });

    expect(result.success).toBe(true);
    expect(calls).toHaveLength(1);
    expect(calls[0].moduleId).toBe("add_a_reading_list_module");
    expect(calls[0].opts).toEqual({ workspaceId: "ws_test" });
    // The real module.js manifest has an object-keyed entityTypes (matching
    // the 14 static day-1 modules ModuleManifestPage.jsx already renders),
    // not VALID_MANIFEST's array-shaped entityTypes (the structured-output
    // summary format, docs/SELF-EXTENSION.md §3).
    expect(calls[0].manifest.entityTypes).toEqual({
      item: {
        label: "Item",
        plural: "Items",
        icon: "FileText",
        attrs: { name: { type: "text", required: true } },
      },
    });
  });

  it("does not fail the install when manifest persistence throws", async () => {
    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: (params) => benignQuery(params),
      validateRenderSmoke: async () => ({ valid: true, errors: [] }),
      persistManifestEntity: async () => {
        throw new Error("simulated API outage");
      },
    });

    expect(result.success).toBe(true);
  });
});

describe("scaffoldModule - render smoke validation (issue #75)", () => {
  it("aborts and merges nothing when Validator 2 reports the module doesn't render cleanly", async () => {
    const { stdout: before } = await git(["log", "--oneline", "main"]);

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: (params) => benignQuery(params),
      validateRenderSmoke: async (moduleId, manifest) => {
        expect(moduleId).toBe("add_a_reading_list_module");
        expect(manifest).toEqual(VALID_MANIFEST);
        return { valid: false, errors: ["console/page errors during render: TypeError: boom"] };
      },
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/Render smoke validation failed/);
    expect(result.error).toMatch(/TypeError: boom/);

    const { stdout: after } = await git(["log", "--oneline", "main"]);
    expect(after).toBe(before);

    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });
});

describe("scaffoldModule - structured output validation", () => {
  it("aborts and merges nothing when structured_output fails ModuleManifest validation", async () => {
    async function* invalidManifestQuery() {
      yield { type: "result", subtype: "success", is_error: false, structured_output: { id: "add_a_reading_list_module" } };
    }

    const { stdout: before } = await git(["log", "--oneline", "main"]);

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: () => invalidManifestQuery(),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/failed ModuleManifest validation/);

    const { stdout: after } = await git(["log", "--oneline", "main"]);
    expect(after).toBe(before);
  });

  it("aborts when the manifest id disagrees with the pre-agent directory slug", async () => {
    async function* mismatchedIdQuery() {
      yield { type: "result", subtype: "success", is_error: false, structured_output: { ...VALID_MANIFEST, id: "something_else" } };
    }

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: () => mismatchedIdQuery(),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/does not match target module id/);
  });

  it("aborts when the SDK exhausts structured-output retries", async () => {
    async function* exhaustedRetriesQuery() {
      yield { type: "result", subtype: "error_max_structured_output_retries", is_error: true };
    }

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: () => exhaustedRetriesQuery(),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/did not complete successfully/);
  });
});

describe("scaffoldModule - structural validation (issue #74)", () => {
  it("aborts and merges nothing when the written module.js is structurally invalid", async () => {
    const brokenSource = VALID_MODULE_SOURCE.replace('type: "item"', 'type: "nonexistent"');
    async function* brokenQuery(params) {
      await fs.writeFile(path.join(params.options.cwd, "modules", "add_a_reading_list_module", "module.js"), brokenSource, "utf8");
      yield { type: "result", subtype: "success", is_error: false, structured_output: VALID_MANIFEST };
    }

    const { stdout: before } = await git(["log", "--oneline", "main"]);

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: (params) => brokenQuery(params),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/Structural validation failed/);
    expect(result.error).toMatch(/nonexistent/);

    const { stdout: after } = await git(["log", "--oneline", "main"]);
    expect(after).toBe(before);

    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });

  it("aborts when the agent leaves module.js unedited (id still the template placeholder)", async () => {
    // A no-op "agent" that reports success without ever touching the
    // seeded file - structural validation must still catch the id/dirname
    // mismatch even though structured output and the hook both look fine.
    async function* noopQuery() {
      yield { type: "result", subtype: "success", is_error: false, structured_output: VALID_MANIFEST };
    }

    const result = await scaffoldModule("add a reading list module", "ws_test", {
      repoRoot,
      queryFn: () => noopQuery(),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/does not match its own directory/);
  });
});

describe("scaffoldModule - escape attempt", () => {
  it("aborts, merges nothing, and removes the worktree when the hook denies a write", async () => {
    // Simulates a compromised/misbehaving agent trying to write outside the
    // module dir - invokes the real PreToolUse hook it was given, exactly
    // as the SDK would when a tool_use targets an out-of-bounds file_path.
    async function* escapingQuery(params) {
      const hook = params.options.hooks.PreToolUse[0].hooks[0];
      await hook({ tool_input: { file_path: "/etc/passwd" } });
      yield { type: "result", subtype: "success", is_error: false };
    }

    const { stdout: before } = await git(["log", "--oneline", "main"]);

    const result = await scaffoldModule("try to escape the sandbox", "ws_test", {
      repoRoot,
      queryFn: (params) => escapingQuery(params),
    });

    expect(result.success).toBe(false);
    expect(result.error).toMatch(/PreToolUse hook denied/);

    const { stdout: after } = await git(["log", "--oneline", "main"]);
    expect(after).toBe(before); // nothing merged

    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1); // discarded, not left behind
  });
});

describe("scaffoldModule - SDK error", () => {
  it("discards the worktree and merges nothing when the SDK call throws", async () => {
    const { stdout: before } = await git(["log", "--oneline", "main"]);

    const result = await scaffoldModule("this will blow up", "ws_test", {
      repoRoot,
      queryFn: () => {
        throw new Error("network unreachable");
      },
    });

    expect(result.success).toBe(false);

    const { stdout: after } = await git(["log", "--oneline", "main"]);
    expect(after).toBe(before);

    const { stdout: worktrees } = await git(["worktree", "list"]);
    expect(worktrees.split("\n").filter(Boolean)).toHaveLength(1);
  });
});
