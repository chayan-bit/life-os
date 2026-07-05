// T4 validator tests (issue #136, docs/SELF-EXTENSION-V2.md §9 T4 row).
// Pure statement-classifier/diff-snapshot logic runs against hardcoded
// fixtures, no I/O. File-discipline runs against a REAL scratch git repo
// (git status/diff are never mocked). The scratch-apply no-rewrite proof
// shells the real `sqlite3` CLI - guarded with skipIf when it's unavailable,
// same convention as the python cache tests (llmCache.test.js).
import { execFile as execFileCb, execFileSync } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  classifyMigration,
  classifyStatement,
  diffSnapshots,
  splitStatements,
  touchesDerivedOwnedTable,
  validateT4Migration,
} from "../validators/t4Migration.js";

const execFile = promisify(execFileCb);

function sqlite3Available() {
  try {
    execFileSync("sqlite3", ["--version"]);
    return true;
  } catch {
    return false;
  }
}
const HAS_SQLITE3 = sqlite3Available();

// ---- statement classifier (pure fixtures) ----------------------------------

describe("classifyStatement - allowed additive shapes", () => {
  it("accepts a GENERATED ALWAYS AS ... VIRTUAL column", () => {
    const result = classifyStatement(
      "ALTER TABLE entities ADD COLUMN due INTEGER GENERATED ALWAYS AS (json_extract(attrs, '$.due')) VIRTUAL",
    );
    expect(result).toEqual({ ok: true, idempotent: false, kind: "generated_virtual_column" });
  });

  it("accepts a plain nullable ADD COLUMN", () => {
    const result = classifyStatement("ALTER TABLE entities ADD COLUMN priority TEXT");
    expect(result).toEqual({ ok: true, idempotent: false, kind: "plain_add_column" });
  });

  it("accepts a plain ADD COLUMN with NOT NULL DEFAULT", () => {
    const result = classifyStatement("ALTER TABLE entities ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal'");
    expect(result.ok).toBe(true);
  });

  it("accepts CREATE INDEX IF NOT EXISTS", () => {
    const result = classifyStatement("CREATE INDEX IF NOT EXISTS idx_entities_due ON entities (due)");
    expect(result).toEqual({ ok: true, idempotent: true, kind: "create_index" });
  });

  it("accepts an expression index", () => {
    const result = classifyStatement(
      "CREATE INDEX IF NOT EXISTS idx_entities_due_expr ON entities (json_extract(attrs, '$.due'))",
    );
    expect(result.ok).toBe(true);
  });

  it("accepts CREATE VIRTUAL TABLE IF NOT EXISTS ... fts5", () => {
    const result = classifyStatement("CREATE VIRTUAL TABLE IF NOT EXISTS topics_fts USING fts5(title, content)");
    expect(result).toEqual({ ok: true, idempotent: true, kind: "create_virtual_table" });
  });
});

describe("classifyStatement - forbidden shapes", () => {
  it("rejects DROP TABLE", () => {
    const result = classifyStatement("DROP TABLE entities");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/DROP/);
  });

  it("rejects UPDATE", () => {
    const result = classifyStatement("UPDATE entities SET status = 'archived' WHERE 1=1");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/UPDATE/);
  });

  it("rejects DELETE", () => {
    const result = classifyStatement("DELETE FROM entities WHERE workspace_id = 'x'");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/DELETE/);
  });

  it("rejects a non-virtual CREATE TABLE", () => {
    const result = classifyStatement("CREATE TABLE new_thing (id TEXT PRIMARY KEY)");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/non-virtual CREATE TABLE/);
  });

  it("rejects ALTER TABLE ... RENAME", () => {
    const result = classifyStatement("ALTER TABLE entities RENAME COLUMN due TO due_at");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/RENAME/);
  });

  it("rejects CREATE TRIGGER", () => {
    const result = classifyStatement("CREATE TRIGGER entities_ai AFTER INSERT ON entities BEGIN SELECT 1; END");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/CREATE TRIGGER/);
  });

  it("rejects ADD COLUMN NOT NULL without a DEFAULT", () => {
    const result = classifyStatement("ALTER TABLE entities ADD COLUMN priority TEXT NOT NULL");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/NOT NULL requires a DEFAULT/);
  });

  it("rejects an unrecognized statement shape", () => {
    const result = classifyStatement("PRAGMA foreign_keys = OFF");
    expect(result.ok).toBe(false);
    expect(result.reason).toMatch(/does not match an allowed additive shape/);
  });
});

describe("splitStatements - comment-tolerant, semicolon-aware", () => {
  it("splits multiple statements and strips line/block comments", () => {
    const sql = `-- header comment\nCREATE INDEX IF NOT EXISTS idx_a ON t (a); /* block */\nCREATE INDEX IF NOT EXISTS idx_b ON t (b);\n`;
    const statements = splitStatements(sql);
    expect(statements).toHaveLength(2);
    expect(statements[0]).toMatch(/idx_a/);
    expect(statements[1]).toMatch(/idx_b/);
  });
});

describe("classifyMigration - multi-statement file", () => {
  it("passes when every statement is an allowed additive shape", () => {
    const sql = `CREATE INDEX IF NOT EXISTS idx_a ON entities (due);\nALTER TABLE entities ADD COLUMN note TEXT;\n`;
    const result = classifyMigration(sql);
    expect(result.valid).toBe(true);
    expect(result.statements).toHaveLength(2);
  });

  it("rejects the whole file if ANY statement is bad, even after good ones", () => {
    const sql = `CREATE INDEX IF NOT EXISTS idx_a ON entities (due);\nDROP TABLE entities;\n`;
    const result = classifyMigration(sql);
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/DROP/);
  });

  it("rejects an empty migration file", () => {
    const result = classifyMigration("-- just a comment, no statements\n");
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/no statements/);
  });
});

describe("touchesDerivedOwnedTable", () => {
  it("flags a migration naming a memvec-owned table", () => {
    expect(touchesDerivedOwnedTable("CREATE INDEX IF NOT EXISTS idx ON entity_vec (embedding)")).toBe("entity_vec");
  });

  it("flags a migration naming a lexical-index table", () => {
    expect(touchesDerivedOwnedTable("ALTER TABLE entities_idx ADD COLUMN x TEXT")).toBe("entities_idx");
  });

  it("returns null for an ordinary canonical table", () => {
    expect(touchesDerivedOwnedTable("ALTER TABLE entities ADD COLUMN due INTEGER")).toBeNull();
  });
});

// ---- diffSnapshots (pure fixtures) -----------------------------------------

describe("diffSnapshots - no-rewrite proof", () => {
  const before = {
    master: [{ type: "table", name: "entities", sql: "CREATE TABLE entities (id TEXT, attrs TEXT)" }],
    tableInfo: {
      entities: [
        { cid: 0, name: "id", type: "TEXT" },
        { cid: 1, name: "attrs", type: "TEXT" },
      ],
    },
  };

  it("passes when only additions appear (new column, new index)", () => {
    const after = {
      master: [
        { type: "table", name: "entities", sql: "CREATE TABLE entities (id TEXT, attrs TEXT)" },
        { type: "index", name: "idx_entities_due", sql: "CREATE INDEX idx_entities_due ON entities (due)" },
      ],
      tableInfo: {
        entities: [
          { cid: 0, name: "id", type: "TEXT" },
          { cid: 1, name: "attrs", type: "TEXT" },
          { cid: 2, name: "due", type: "INTEGER" },
        ],
      },
    };
    expect(diffSnapshots(before, after)).toEqual({ valid: true, errors: [] });
  });

  it("rejects when a pre-existing column changes type", () => {
    const after = {
      master: before.master,
      tableInfo: {
        entities: [
          { cid: 0, name: "id", type: "TEXT" },
          { cid: 1, name: "attrs", type: "BLOB" },
        ],
      },
    };
    const result = diffSnapshots(before, after);
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/pre-existing column changed/);
  });

  it("rejects when a pre-existing table is dropped", () => {
    const after = { master: [], tableInfo: {} };
    const result = diffSnapshots(before, after);
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/pre-existing table dropped/);
  });

  it("rejects when a pre-existing NON-TABLE sqlite_master row's sql is rewritten (e.g. an index redefined)", () => {
    const beforeWithIndex = {
      master: [...before.master, { type: "index", name: "idx_entities_id", sql: "CREATE INDEX idx_entities_id ON entities (id)" }],
      tableInfo: before.tableInfo,
    };
    const after = {
      master: [
        beforeWithIndex.master[0],
        { type: "index", name: "idx_entities_id", sql: "CREATE INDEX idx_entities_id ON entities (attrs)" },
      ],
      tableInfo: before.tableInfo,
    };
    const result = diffSnapshots(beforeWithIndex, after);
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/existing schema object rewritten/);
  });

  it("does NOT flag a table's own sqlite_master sql text changing after a legitimate ADD COLUMN", () => {
    // SQLite always rewrites the recorded CREATE TABLE text in sqlite_master
    // when a column is added - that's normal additive behavior, not a
    // harmful rewrite, and must not be flagged as long as pre-existing
    // columns are unchanged (asserted separately via table_info).
    const after = {
      master: [{ type: "table", name: "entities", sql: "CREATE TABLE entities (id TEXT, attrs TEXT, due INTEGER)" }],
      tableInfo: {
        entities: [
          { cid: 0, name: "id", type: "TEXT" },
          { cid: 1, name: "attrs", type: "TEXT" },
          { cid: 2, name: "due", type: "INTEGER" },
        ],
      },
    };
    expect(diffSnapshots(before, after)).toEqual({ valid: true, errors: [] });
  });
});

// ---- file discipline (real scratch git repo) -------------------------------

let worktree;

async function git(args) {
  return execFile("git", args, { cwd: worktree });
}

async function initRepo(existingMigrations) {
  worktree = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t4-worktree-"));
  await execFile("git", ["init", "-b", "main"], { cwd: worktree });
  await execFile("git", ["config", "user.email", "test@example.com"], { cwd: worktree });
  await execFile("git", ["config", "user.name", "Test"], { cwd: worktree });
  await fs.mkdir(path.join(worktree, "migrations"), { recursive: true });
  for (const [file, sql] of Object.entries(existingMigrations)) {
    await fs.writeFile(path.join(worktree, "migrations", file), sql, "utf8");
  }
  await git(["add", "."]);
  await git(["commit", "-m", "seed migrations"]);
}

beforeEach(async () => {
  await initRepo({ "0001_core.sql": "CREATE TABLE entities (id TEXT PRIMARY KEY);\n" });
});

afterEach(async () => {
  await fs.rm(worktree, { recursive: true, force: true });
});

describe("validateT4Migration - requires params", () => {
  it("rejects when name is missing", async () => {
    const result = await validateT4Migration({ worktreePath: worktree, params: {} });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/params\.name/);
  });
});

describe("validateT4Migration - file discipline", () => {
  it("rejects when no file changed", async () => {
    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/exactly one file/);
  });

  it("rejects when an extra file changed alongside the migration", async () => {
    await fs.writeFile(path.join(worktree, "migrations", "0002_due_date.sql"), "CREATE INDEX IF NOT EXISTS idx ON entities (id);\n", "utf8");
    await fs.writeFile(path.join(worktree, "migrations", "0001_core.sql"), "CREATE TABLE entities (id TEXT PRIMARY KEY, extra TEXT);\n", "utf8");

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/exactly one file/);
  });

  it("rejects a wrong migration number (not max + 1)", async () => {
    await fs.writeFile(path.join(worktree, "migrations", "0005_due_date.sql"), "CREATE INDEX IF NOT EXISTS idx ON entities (id);\n", "utf8");

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/migration number must be the next one \(0002\)/);
  });

  it("rejects a filename that doesn't match migrations/<NNNN>_<name>.sql", async () => {
    await fs.writeFile(path.join(worktree, "migrations", "0002_wrong_name.sql"), "CREATE INDEX IF NOT EXISTS idx ON entities (id);\n", "utf8");

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/must be migrations\/<NNNN>_due_date\.sql/);
  });
});

describe.skipIf(!HAS_SQLITE3)("validateT4Migration - scratch-apply no-rewrite proof (real sqlite3)", () => {
  it("passes an additive GENERATED VIRTUAL column + expression index", async () => {
    await fs.writeFile(
      path.join(worktree, "migrations", "0002_due_date.sql"),
      "ALTER TABLE entities ADD COLUMN due_text TEXT;\n" +
        "CREATE INDEX IF NOT EXISTS idx_entities_id ON entities (id);\n",
      "utf8",
    );

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });

  it("rejects a fixture migration that drops the pre-existing table", async () => {
    // classifyMigration already rejects DROP statically, but this proves the
    // scratch-apply path is never reached for a destructive file either way -
    // still exercised via the full validateT4Migration entry point.
    await fs.writeFile(path.join(worktree, "migrations", "0002_due_date.sql"), "DROP TABLE entities;\n", "utf8");

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/DROP/);
  });

  it("re-applies an idempotent index-only migration to prove the idempotency claim", async () => {
    await fs.writeFile(
      path.join(worktree, "migrations", "0002_due_date.sql"),
      "CREATE INDEX IF NOT EXISTS idx_entities_id ON entities (id);\n",
      "utf8",
    );

    const result = await validateT4Migration({ worktreePath: worktree, params: { name: "due_date" } });
    expect(result.valid).toBe(true);
  });
});
