import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { getValidators } from "../validators/registry.js";

const execFile = promisify(execFileCb);

function validatorNames(tier) {
  return getValidators(tier).map((v) => v.name);
}

function protectedSurfaceRun(tier = "T0") {
  return getValidators(tier).find((v) => v.name === "protectedSurface").run;
}

describe("getValidators - registry dispatch", () => {
  it("resolves T0 to protectedSurface + structural + render-smoke, in order", () => {
    expect(validatorNames("T0")).toEqual(["protectedSurface", "structural", "renderSmoke"]);
  });

  it("puts protectedSurface first at every tier", () => {
    for (const tier of ["T0", "T1", "T2", "T3", "T4", "T5"]) {
      expect(validatorNames(tier)[0]).toBe("protectedSurface");
    }
  });

  it("fails closed for an unknown tier (rejecting placeholder only)", async () => {
    const validators = getValidators("T9");
    expect(validators.map((v) => v.name)).toEqual(["notImplemented"]);
    const result = await validators[0].run();
    expect(result.valid).toBe(false);
  });

  it("T5 resolves to protectedSurface + a placeholder that fails closed (last remaining placeholder)", async () => {
    expect(validatorNames("T5")).toEqual(["protectedSurface", "notImplemented"]);
    const placeholder = getValidators("T5").find((v) => v.name === "notImplemented");
    const result = await placeholder.run();
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toBe("validator not yet implemented for T5");
  });

  it("T1 resolves to protectedSurface + t1Render (issue #133 - no longer a placeholder)", () => {
    expect(validatorNames("T1")).toEqual(["protectedSurface", "t1Render"]);
  });

  it("T2 resolves to protectedSurface + t2Tool (issue #134 - no longer a placeholder)", () => {
    expect(validatorNames("T2")).toEqual(["protectedSurface", "t2Tool"]);
  });

  it("T3 resolves to protectedSurface + t3Route (issue #135 - no longer a placeholder)", () => {
    expect(validatorNames("T3")).toEqual(["protectedSurface", "t3Route"]);
  });

  it("T4 resolves to protectedSurface + t4Migration (issue #136 - no longer a placeholder)", () => {
    expect(validatorNames("T4")).toEqual(["protectedSurface", "t4Migration"]);
  });
});

describe("protectedSurfaceValidator - diff inspection against a scratch repo", () => {
  let repo;

  async function git(args, cwd = repo) {
    return execFile("git", args, { cwd });
  }

  async function writeFileMkdir(rel, contents) {
    const full = path.join(repo, rel);
    await fs.mkdir(path.dirname(full), { recursive: true });
    await fs.writeFile(full, contents, "utf8");
  }

  beforeEach(async () => {
    repo = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-validator-repo-"));
    await git(["init", "-b", "main"]);
    await git(["config", "user.email", "test@example.com"]);
    await git(["config", "user.name", "Test"]);
    await writeFileMkdir("README.md", "seed\n");
    await git(["add", "README.md"]);
    await git(["commit", "-m", "seed"]);
  });

  afterEach(async () => {
    await fs.rm(repo, { recursive: true, force: true });
  });

  it("passes a clean diff that only adds a module file", async () => {
    await writeFileMkdir("modules/habits/module.js", "osRegisterModule({});\n");
    const result = await protectedSurfaceRun()({ worktreePath: repo, baseRef: "main" });
    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });

  it("rejects an untracked write to a never-generable surface", async () => {
    await writeFileMkdir("server/lib/sandbox.js", "// tampered\n");
    const result = await protectedSurfaceRun()({ worktreePath: repo, baseRef: "main" });
    expect(result.valid).toBe(false);
    expect(result.errors.join(" ")).toMatch(/server\/lib\/sandbox\.js/);
  });

  it("rejects a committed change touching a protected migration (diff path)", async () => {
    await writeFileMkdir("migrations/0002_control_plane.sql", "-- edited\n");
    await git(["add", "."]);
    await git(["commit", "-m", "touch protected"]);
    const result = await protectedSurfaceRun()({ worktreePath: repo, baseRef: "main~1" });
    expect(result.valid).toBe(false);
    expect(result.errors.join(" ")).toMatch(/0002_control_plane\.sql/);
  });

  it("fails closed when the diff cannot be inspected (not a git dir)", async () => {
    const notGit = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-notgit-"));
    try {
      const result = await protectedSurfaceRun()({ worktreePath: notGit, baseRef: "main" });
      expect(result.valid).toBe(false);
    } finally {
      await fs.rm(notGit, { recursive: true, force: true });
    }
  });
});
