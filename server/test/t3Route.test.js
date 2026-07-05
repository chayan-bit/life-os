// T3 validator tests (issue #135, docs/SELF-EXTENSION-V2.md §9 T3 row).
// Scope + mod.rs additive-diff + scratch-DB checks run against a REAL scratch
// git repo fixture (git status/diff are never mocked - only cargo/clippy are,
// via the injectable opts.execFn, so vitest never shells real cargo).
import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { validateT3Route } from "../validators/t3Route.js";

const execFile = promisify(execFileCb);
const CRATE = "lifeos-api";
const NAME = "week";

let worktree;

async function git(args) {
  return execFile("git", args, { cwd: worktree });
}

const MOD_RS_INITIAL = `mod health;
mod entity;

use crate::state::AppState;
use axum::Router;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", axum::routing::get(health::health))
        .with_state(state)
}
`;

const ROUTE_RS = `use crate::error::ApiResult;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde_json::{json, Value};

pub async fn week(State(_state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "summary": "stub" })))
}
`;

const GOOD_TEST_RS = `use lifeos_api::{build_state_with_nango, config::Config, routes};

fn base_config(db_path: &str) -> Config {
    Config {
        db_path: db_path.to_string(),
        derived_db_path: format!("{db_path}.derived"),
        ..Default::default()
    }
}

#[tokio::test]
async fn week_returns_a_summary() {
    let db_path = std::env::temp_dir()
        .join("lifeos_week_test.db")
        .to_string_lossy()
        .to_string();
    let state = build_state_with_nango(base_config(&db_path), None).await.expect("state");
    let _router = routes::router(state);
}
`;

async function initRepo() {
  worktree = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t3-worktree-"));
  await execFile("git", ["init", "-b", "main"], { cwd: worktree });
  await execFile("git", ["config", "user.email", "test@example.com"], { cwd: worktree });
  await execFile("git", ["config", "user.name", "Test"], { cwd: worktree });
  await fs.mkdir(path.join(worktree, "services", CRATE, "src", "routes"), { recursive: true });
  await fs.mkdir(path.join(worktree, "services", CRATE, "tests"), { recursive: true });
  await fs.writeFile(path.join(worktree, "services", CRATE, "src", "routes", "mod.rs"), MOD_RS_INITIAL, "utf8");
  await git(["add", "."]);
  await git(["commit", "-m", "seed mod.rs"]);
}

async function writeRoute() {
  await fs.writeFile(path.join(worktree, "services", CRATE, "src", "routes", `${NAME}.rs`), ROUTE_RS, "utf8");
}

async function writeTest(source = GOOD_TEST_RS) {
  await fs.writeFile(path.join(worktree, "services", CRATE, "tests", `${NAME}_integration.rs`), source, "utf8");
}

async function registerAdditively() {
  const modPath = path.join(worktree, "services", CRATE, "src", "routes", "mod.rs");
  const updated = MOD_RS_INITIAL.replace(
    "mod entity;\n",
    `mod entity;\nmod ${NAME};\n`,
  ).replace(
    '.route("/api/health", axum::routing::get(health::health))\n',
    `.route("/api/health", axum::routing::get(health::health))\n        // --- generated (T3) ---\n        .route("/api/${NAME}", axum::routing::get(${NAME}::${NAME}))\n`,
  );
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

describe("validateT3Route - requires params", () => {
  it("rejects when crate or name is missing", async () => {
    const result = await validateT3Route({ worktreePath: worktree, params: {} });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/params\.crate and params\.name/);
  });
});

describe("validateT3Route - happy path", () => {
  it("passes a scoped route + additive mod.rs + scratch-DB test with mocked cargo/clippy success", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });

  it("passes even when mod.rs is left untouched (route need not always register)", async () => {
    await writeRoute();
    await writeTest();

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(true);
  });
});

describe("validateT3Route - scope", () => {
  it("rejects a build that touches a file outside the three allowed paths", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();
    await fs.writeFile(path.join(worktree, "services", CRATE, "src", "routes", "other.rs"), "// sneaky\n", "utf8");

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/outside its scope/);
    expect(result.errors[0]).toMatch(/other\.rs/);
  });
});

describe("validateT3Route - mod.rs additive-only diff", () => {
  it("rejects a mod.rs edit that removes an existing route registration", async () => {
    await writeRoute();
    await writeTest();
    const modPath = path.join(worktree, "services", CRATE, "src", "routes", "mod.rs");
    const destructive = MOD_RS_INITIAL.replace(
      '.route("/api/health", axum::routing::get(health::health))\n',
      `.route("/api/${NAME}", axum::routing::get(${NAME}::${NAME}))\n`,
    ).replace("mod entity;\n", `mod entity;\nmod ${NAME};\n`);
    await fs.writeFile(modPath, destructive, "utf8");

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/removes or modifies/);
  });
});

describe("validateT3Route - integration test scratch-DB check", () => {
  it("rejects when the integration test file is missing", async () => {
    await writeRoute();
    await registerAdditively();

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/missing integration test file/);
  });

  it("rejects a test that references the canonical lifeos.db path", async () => {
    await writeRoute();
    await registerAdditively();
    await writeTest(GOOD_TEST_RS.replace('.join("lifeos_week_test.db")', '.join("lifeos.db")'));

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/canonical DB path/);
  });

  it("rejects a test hardcoding a home-relative ~/ path", async () => {
    await writeRoute();
    await registerAdditively();
    await writeTest(GOOD_TEST_RS.replace("std::env::temp_dir()", '"~/lifeos-scratch"'));

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/canonical DB path/);
  });

  it("rejects a test hardcoding an absolute /Users path", async () => {
    await writeRoute();
    await registerAdditively();
    await writeTest(GOOD_TEST_RS.replace("std::env::temp_dir()", '"/Users/chayan/scratch"'));

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/canonical DB path/);
  });

  it("rejects a test that never constructs a scratch DB via temp_dir()", async () => {
    await writeRoute();
    await registerAdditively();
    await writeTest(GOOD_TEST_RS.replace(/std::env::temp_dir\(\)/, '"/some/other/path".into()'));

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn: passingExecFn() },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/temp_dir\(\) scratch-db pattern/);
  });
});

describe("validateT3Route - cargo build/test/clippy gate (execFn DI)", () => {
  it("rejects when cargo build fails", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();

    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "build") throw new Error("compile error");
      return { stdout: "", stderr: "" };
    };

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo build failed/);
  });

  it("rejects when cargo test fails", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();

    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "test") throw new Error("assertion failed");
      return { stdout: "", stderr: "" };
    };

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo test failed/);
  });

  it("rejects when cargo clippy fails", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();

    const execFn = async (cmd, args) => {
      if (cmd === "cargo" && args[0] === "clippy") throw new Error("warnings found");
      return { stdout: "", stderr: "" };
    };

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/cargo clippy failed/);
  });

  it("calls cargo build/test/clippy with the expected args, in order", async () => {
    await writeRoute();
    await writeTest();
    await registerAdditively();

    const calls = [];
    const execFn = async (cmd, args) => {
      calls.push([cmd, args]);
      return { stdout: "", stderr: "" };
    };

    const result = await validateT3Route({
      worktreePath: worktree,
      params: { crate: CRATE, name: NAME },
      opts: { execFn },
    });

    expect(result.valid).toBe(true);
    expect(calls).toEqual([
      ["cargo", ["build", "-p", CRATE]],
      ["cargo", ["test", "-p", CRATE, "--test", `${NAME}_integration`]],
      ["cargo", ["clippy", "-p", CRATE, "--", "-D", "warnings"]],
    ]);
  });
});
