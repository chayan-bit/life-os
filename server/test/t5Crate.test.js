// T5 validator tests (issue #137, docs/SELF-EXTENSION-V2.md §9 T5 row).
// Scope + additive Cargo.toml diff + reviewer sign-off checks run against a
// REAL scratch git repo fixture (git status/diff are never mocked - only
// cargo/clippy are, via the injectable opts.execFn, so vitest never shells
// real cargo).
import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { validateT5Crate } from "../validators/t5Crate.js";

const execFile = promisify(execFileCb);
const CRATE = "lifeos-finance";
const APPROVED = { approve: true, issues: [] };

let worktree;

async function git(args) {
  return execFile("git", args, { cwd: worktree });
}

const CARGO_TOML_INITIAL = `[workspace]
resolver = "2"
members = [
    "lifeos-api",
    "lifeos-vcs",
]
`;

async function initRepo() {
  worktree = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t5-worktree-"));
  await execFile("git", ["init", "-b", "main"], { cwd: worktree });
  await execFile("git", ["config", "user.email", "test@example.com"], { cwd: worktree });
  await execFile("git", ["config", "user.name", "Test"], { cwd: worktree });
  await fs.mkdir(path.join(worktree, "services"), { recursive: true });
  await fs.writeFile(path.join(worktree, "services", "Cargo.toml"), CARGO_TOML_INITIAL, "utf8");
  await git(["add", "."]);
  await git(["commit", "-m", "seed services workspace"]);
}

async function writeCrate() {
  const crateDir = path.join(worktree, "services", CRATE);
  await fs.mkdir(path.join(crateDir, "src"), { recursive: true });
  await fs.mkdir(path.join(crateDir, "tests"), { recursive: true });
  await fs.writeFile(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${CRATE}"\nversion = "0.1.0"\nedition = "2021"\n`, "utf8");
  await fs.writeFile(path.join(crateDir, "src", "lib.rs"), "pub fn add(a: i64, b: i64) -> i64 { a + b }\n", "utf8");
  await fs.writeFile(
    path.join(crateDir, "tests", "it.rs"),
    '#[test]\nfn ok() { let _ = std::env::temp_dir().join("scratch.db"); assert_eq!(1, 1); }\n',
    "utf8",
  );
}

async function registerAdditively() {
  const modPath = path.join(worktree, "services", "Cargo.toml");
  const updated = CARGO_TOML_INITIAL.replace('    "lifeos-vcs",\n', `    "lifeos-vcs",\n    "${CRATE}",\n`);
  await fs.writeFile(modPath, updated, "utf8");
}

function passingExecFn() {
  return async () => ({ stdout: "", stderr: "" });
}

beforeEach(async () => {
  await initRepo();
});

afterEach(async () => {
  await fs.rm(worktree, { recursive: true, force: true });
});

describe("validateT5Crate - requires params", () => {
  it("rejects when crate is missing", async () => {
    const result = await validateT5Crate({ worktreePath: worktree, params: {}, review: APPROVED });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/requires params\.crate/);
  });
});

describe("validateT5Crate - happy path", () => {
  it("passes a scoped crate + additive Cargo.toml + approved review with mocked cargo success", async () => {
    await writeCrate();
    await registerAdditively();

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      review: APPROVED,
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });

  it("passes even when Cargo.toml is left untouched (crate already a member)", async () => {
    await writeCrate();

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      review: APPROVED,
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(true);
  });
});

describe("validateT5Crate - scope", () => {
  it("rejects a build that touches a file outside services/<crate>/ or services/Cargo.toml", async () => {
    await writeCrate();
    await registerAdditively();
    await fs.writeFile(path.join(worktree, "services", "lifeos-api-sneaky.rs"), "// sneaky\n", "utf8");

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      review: APPROVED,
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/outside its scope/);
    expect(result.errors[0]).toMatch(/lifeos-api-sneaky\.rs/);
  });
});

describe("validateT5Crate - Cargo.toml additive-only diff", () => {
  it("rejects a Cargo.toml edit that removes an existing member", async () => {
    await writeCrate();
    const modPath = path.join(worktree, "services", "Cargo.toml");
    const destructive = CARGO_TOML_INITIAL.replace('    "lifeos-vcs",\n', `    "${CRATE}",\n`);
    await fs.writeFile(modPath, destructive, "utf8");

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      review: APPROVED,
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/removes or modifies/);
  });
});

describe("validateT5Crate - reviewer sign-off", () => {
  it("rejects a node without a recorded reviewer approval", async () => {
    await writeCrate();
    await registerAdditively();

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      review: { approve: false, issues: ["not good"] },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/reviewer sign-off/);
  });

  it("rejects a node with no review threaded at all", async () => {
    await writeCrate();
    await registerAdditively();

    const result = await validateT5Crate({
      worktreePath: worktree,
      params: { crate: CRATE },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/reviewer sign-off/);
  });
});

describe("validateT5Crate - cargo build/test/clippy gate (execFn DI)", () => {
  it("rejects when cargo build fails", async () => {
    await writeCrate();
    await registerAdditively();
    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "build") throw new Error("compile error");
      return { stdout: "", stderr: "" };
    };
    const result = await validateT5Crate({ worktreePath: worktree, params: { crate: CRATE }, review: APPROVED, opts: { execFn } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo build failed/);
  });

  it("rejects when cargo test fails", async () => {
    await writeCrate();
    await registerAdditively();
    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "test") throw new Error("assertion failed");
      return { stdout: "", stderr: "" };
    };
    const result = await validateT5Crate({ worktreePath: worktree, params: { crate: CRATE }, review: APPROVED, opts: { execFn } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo test failed/);
  });

  it("rejects when cargo clippy fails", async () => {
    await writeCrate();
    await registerAdditively();
    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "clippy") throw new Error("warnings found");
      return { stdout: "", stderr: "" };
    };
    const result = await validateT5Crate({ worktreePath: worktree, params: { crate: CRATE }, review: APPROVED, opts: { execFn } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo clippy failed/);
  });

  it("calls cargo build/test/clippy with the expected args, in order", async () => {
    await writeCrate();
    await registerAdditively();
    const calls = [];
    const execFn = async (cmd, args) => {
      calls.push([cmd, args]);
      return { stdout: "", stderr: "" };
    };
    const result = await validateT5Crate({ worktreePath: worktree, params: { crate: CRATE }, review: APPROVED, opts: { execFn } });
    expect(result.valid).toBe(true);
    expect(calls).toEqual([
      ["cargo", ["build", "-p", CRATE]],
      ["cargo", ["test", "-p", CRATE]],
      ["cargo", ["clippy", "-p", CRATE, "--", "-D", "warnings"]],
    ]);
  });
});
