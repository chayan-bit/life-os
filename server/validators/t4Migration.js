// T4 validator (issue #136, docs/SELF-EXTENSION-V2.md §9 T4 row) - the
// no-rewrite proof for a generated additive migration.
//
// Given a worktree + params {name}, this proves:
//  1. file discipline: exactly ONE new file, migrations/<NNNN>_<name>.sql,
//     numbered current-max + 1; nothing else in the worktree changed.
//  2. statement shape: every statement in the file is one of the allowed
//     additive shapes (GENERATED ALWAYS ... VIRTUAL column, plain nullable
//     ADD COLUMN, CREATE INDEX IF NOT EXISTS, CREATE VIRTUAL TABLE IF NOT
//     EXISTS ... fts5(...)) - an explicit destructive denylist (DROP/DELETE/
//     UPDATE/TRUNCATE/RENAME/CREATE TRIGGER/non-virtual CREATE TABLE) is
//     checked first and always wins.
//  3. scratch-apply no-rewrite proof: applies every EXISTING migration file
//     (numeric order) to a fresh scratch sqlite DB via the `sqlite3` CLI
//     (shelled through the injectable opts.execFn, same DI seam as
//     t3Route.js), snapshots `sqlite_master` + `PRAGMA table_info` for every
//     table, applies the new migration, and re-snapshots: no pre-existing
//     `sqlite_master` row's `sql` changed and no pre-existing column
//     name/type/position changed. If every statement is idempotent
//     (CREATE INDEX/VIRTUAL TABLE IF NOT EXISTS - a plain ADD COLUMN is NOT,
//     SQLite errors on a repeat ADD COLUMN, which is exactly what the
//     runtime's `add_column_if_missing` guard in lifeos-api/src/db.rs exists
//     for), the migration is applied a second time to prove that
//     idempotency claim too.
//  4. derived-ownership check, HONESTLY SCOPED: `services/lifeos-derived`
//     does not exist as a crate/DB in this repo today - the derived DB's own
//     DDL (migrations/0003_derived.sql, 0018_derived_memory.sql) is baked
//     into `lifeos-api/src/db.rs` as `include_str!` constants applied to a
//     PHYSICALLY SEPARATE file (bootstrap_derived), not read generically
//     from `migrations/`. A T4 build's write-scope is therefore narrowed to
//     canonical `migrations/*_<name>.sql` files only (tierScopes.js), and
//     there is no practical rebuild entry point this validator can drive
//     without booting the full Rust API. What IS checked here: the new
//     migration's statement text never names a memvec-owned table
//     (`entity_vec`, `entity_vec_meta`, `llm_cache` - server/memvec.py's own
//     schema) or the Rust-owned derived-index tables (`entities_idx`,
//     `entities_fts`, `memory_idx`, `memory_fts`) - a migration naming any
//     of those is rejected outright, since those tables are REBUILT, never
//     migrated. This is a static-text check, not a live rebuild-convergence
//     run - stated honestly rather than invented.
//
// Fail-closed: an uninspectable diff, an unparseable statement, a scratch DB
// that cannot be built, or any rewrite/ambiguity all reject.
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";

const defaultExecFile = promisify(execFileCb);

const MIGRATIONS_DIR = "migrations";
const NUMBER_WIDTH = 4;

// Tables owned by a rebuild path (memvec.py's semantic index or lifeos-api's
// lexical index), never by a migration - see the module doc's §4 note.
const DERIVED_OWNED_TABLES = ["entity_vec", "entity_vec_meta", "llm_cache", "entities_idx", "entities_fts", "memory_idx", "memory_fts"];

// ---- statement splitting (semicolon-aware, comment-tolerant) --------------

function stripComments(sql) {
  return sql.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "");
}

// Splits on `;` outside of a single-quoted string literal. Good enough for
// the additive DDL shapes this validator allows (no semicolons appear inside
// a GENERATED expression or an index expression in any migration we author).
export function splitStatements(sql) {
  const cleaned = stripComments(sql);
  const statements = [];
  let current = "";
  let inString = false;
  for (const ch of cleaned) {
    if (ch === "'") inString = !inString;
    if (ch === ";" && !inString) {
      const trimmed = current.trim();
      if (trimmed.length > 0) statements.push(trimmed);
      current = "";
      continue;
    }
    current += ch;
  }
  const trimmed = current.trim();
  if (trimmed.length > 0) statements.push(trimmed);
  return statements;
}

// ---- statement classification ---------------------------------------------

const DESTRUCTIVE_DENYLIST = [
  { re: /\bDROP\b/i, label: "DROP" },
  { re: /\bDELETE\b/i, label: "DELETE" },
  { re: /\bUPDATE\b/i, label: "UPDATE" },
  { re: /\bTRUNCATE\b/i, label: "TRUNCATE" },
  { re: /\bRENAME\b/i, label: "RENAME" },
  { re: /\bCREATE\s+TRIGGER\b/i, label: "CREATE TRIGGER" },
];

const GENERATED_VIRTUAL_COLUMN = /^ALTER\s+TABLE\s+\S+\s+ADD\s+COLUMN\s+\S+[\s\S]*\bGENERATED\s+ALWAYS\s+AS\s*\([\s\S]*\)\s*VIRTUAL\b/i;
const PLAIN_ADD_COLUMN = /^ALTER\s+TABLE\s+\S+\s+ADD\s+COLUMN\s+\S+\s+\S+/i;
const CREATE_INDEX = /^CREATE\s+INDEX\s+IF\s+NOT\s+EXISTS\s+\S+\s+ON\s+\S+\s*\(/i;
const CREATE_VIRTUAL_FTS_TABLE = /^CREATE\s+VIRTUAL\s+TABLE\s+IF\s+NOT\s+EXISTS\s+\S+\s+USING\s+fts5\s*\(/i;
const NOT_NULL = /\bNOT\s+NULL\b/i;
const HAS_DEFAULT = /\bDEFAULT\b/i;

// Classifies one SQL statement against the allowed additive shapes. Returns
// { ok: true, idempotent } on a pass, or { ok: false, reason } on a reject.
// Fail-closed: anything not positively matched is rejected, never allowed by
// omission.
export function classifyStatement(stmt) {
  const s = stmt.trim();
  for (const { re, label } of DESTRUCTIVE_DENYLIST) {
    if (re.test(s)) return { ok: false, reason: `destructive statement (${label}) rejected: ${s.slice(0, 120)}` };
  }
  if (/^CREATE\s+TABLE\b/i.test(s) && !/^CREATE\s+VIRTUAL\s+TABLE\b/i.test(s)) {
    return { ok: false, reason: `non-virtual CREATE TABLE is not an additive shape: ${s.slice(0, 120)}` };
  }
  if (GENERATED_VIRTUAL_COLUMN.test(s)) {
    return { ok: true, idempotent: false, kind: "generated_virtual_column" };
  }
  if (CREATE_INDEX.test(s)) {
    return { ok: true, idempotent: true, kind: "create_index" };
  }
  if (CREATE_VIRTUAL_FTS_TABLE.test(s)) {
    return { ok: true, idempotent: true, kind: "create_virtual_table" };
  }
  if (PLAIN_ADD_COLUMN.test(s)) {
    if (NOT_NULL.test(s) && !HAS_DEFAULT.test(s)) {
      return { ok: false, reason: `ADD COLUMN with NOT NULL requires a DEFAULT: ${s.slice(0, 120)}` };
    }
    return { ok: true, idempotent: false, kind: "plain_add_column" };
  }
  return { ok: false, reason: `statement does not match an allowed additive shape: ${s.slice(0, 120)}` };
}

// Classifies every statement in a migration file's text. Rejects the whole
// file on the first bad statement (fail closed: ambiguity anywhere means
// reject the file, not just the offending statement).
export function classifyMigration(sql) {
  const statements = splitStatements(sql);
  if (statements.length === 0) {
    return { valid: false, errors: ["migration file has no statements"], statements: [] };
  }
  const classified = [];
  for (const stmt of statements) {
    const result = classifyStatement(stmt);
    if (!result.ok) return { valid: false, errors: [result.reason], statements: [] };
    classified.push({ stmt, ...result });
  }
  return { valid: true, errors: [], statements: classified };
}

// True if the migration text names a table this validator has decided is
// derived-owned (rebuilt, never migrated) - see the module doc's §4 note.
export function touchesDerivedOwnedTable(sql) {
  return DERIVED_OWNED_TABLES.find((table) => new RegExp(`\\b${table}\\b`, "i").test(sql)) ?? null;
}

// ---- file discipline --------------------------------------------------------

function changedPathsFromPorcelain(stdout) {
  const tokens = stdout.split("\0").filter((t) => t.length > 0);
  const paths = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const entry = tokens[i];
    const status = entry.slice(0, 2);
    paths.push(entry.slice(3));
    if (status[0] === "R" || status[0] === "C") i += 1; // consume the origin token
  }
  return paths;
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Reads the migrations dir (excluding `excludeRel`) and returns the sorted
// list of leading 4-digit numbers already in use.
async function existingMigrationNumbers(worktreePath, excludeRel) {
  const dir = path.join(worktreePath, MIGRATIONS_DIR);
  let entries;
  try {
    entries = await fs.readdir(dir);
  } catch {
    return [];
  }
  const numbers = [];
  for (const entry of entries) {
    if (path.posix.join(MIGRATIONS_DIR, entry) === excludeRel) continue;
    const m = entry.match(/^(\d{4})_/);
    if (m) numbers.push(parseInt(m[1], 10));
  }
  return numbers;
}

// Scope + numbering check: exactly one new file, migrations/<NNNN>_<name>.sql,
// numbered current-max + 1, nothing else changed.
async function checkFileDiscipline({ worktreePath, name }) {
  let stdout;
  try {
    ({ stdout } = await defaultExecFile("git", ["status", "--porcelain", "-uall", "-z"], { cwd: worktreePath }));
  } catch (error) {
    return { valid: false, errors: [`could not inspect the build diff: ${error.message}`] };
  }
  const changed = changedPathsFromPorcelain(stdout);
  if (changed.length !== 1) {
    return { valid: false, errors: [`T4 build must change exactly one file, found ${changed.length}: ${changed.join(", ") || "(none)"}`] };
  }
  const rel = changed[0];
  const fileRe = new RegExp(`^${MIGRATIONS_DIR}/(\\d{4})_${escapeRegExp(name)}\\.sql$`);
  const m = rel.match(fileRe);
  if (!m) {
    return {
      valid: false,
      errors: [`T4 build's only changed file must be ${MIGRATIONS_DIR}/<NNNN>_${name}.sql, found: ${rel}`],
    };
  }
  const newNumber = parseInt(m[1], 10);
  const existing = await existingMigrationNumbers(worktreePath, rel);
  const maxExisting = existing.length > 0 ? Math.max(...existing) : 0;
  const expected = maxExisting + 1;
  if (newNumber !== expected) {
    return {
      valid: false,
      errors: [`migration number must be the next one (${String(expected).padStart(NUMBER_WIDTH, "0")}), found ${m[1]}`],
    };
  }
  return { valid: true, errors: [], relPath: rel, existingNumbers: existing };
}

// ---- scratch-apply no-rewrite proof ----------------------------------------

async function runSqliteJson(execFn, dbPath, sql) {
  const { stdout } = await execFn("sqlite3", ["-json", dbPath, sql]);
  const trimmed = stdout.trim();
  if (trimmed.length === 0) return [];
  return JSON.parse(trimmed);
}

async function applyMigrationFile(execFn, dbPath, absFilePath) {
  await execFn("sqlite3", [dbPath, `.read ${absFilePath}`]);
}

async function snapshotSchema(execFn, dbPath) {
  const master = await runSqliteJson(execFn, dbPath, "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name");
  const tableInfo = {};
  for (const row of master) {
    if (row.type !== "table") continue;
    tableInfo[row.name] = await runSqliteJson(execFn, dbPath, `PRAGMA table_info(${row.name})`);
  }
  return { master, tableInfo };
}

// Pure diff: no pre-existing NON-TABLE sqlite_master row's `sql` may change
// (an index/virtual-table's recorded DDL never legitimately changes once
// created), and no pre-existing column (name/type/position) may change.
// Table rows are deliberately excluded from the sql-text check here - a
// legitimate `ADD COLUMN` always rewrites the table's recorded CREATE TABLE
// text in sqlite_master (that's SQLite's normal, additive behavior, not a
// "rewrite" in the harmful sense) - the table_info column check below is
// what actually proves a pre-existing table's columns are untouched. New
// objects/columns are fine - that IS the additive change under test. Pure +
// fixture-testable, independent of any sqlite exec.
export function diffSnapshots(before, after) {
  const errors = [];
  const beforeMaster = new Map(before.master.filter((r) => r.type !== "table").map((r) => [`${r.type}:${r.name}`, r.sql]));
  for (const row of after.master) {
    if (row.type === "table") continue;
    const key = `${row.type}:${row.name}`;
    if (beforeMaster.has(key) && beforeMaster.get(key) !== row.sql) {
      errors.push(`existing schema object rewritten: ${row.name}`);
    }
  }
  for (const [table, beforeCols] of Object.entries(before.tableInfo)) {
    const afterCols = after.tableInfo[table];
    if (!afterCols) {
      errors.push(`pre-existing table dropped: ${table}`);
      continue;
    }
    for (let i = 0; i < beforeCols.length; i += 1) {
      const b = beforeCols[i];
      const a = afterCols[i];
      if (!a || a.name !== b.name || a.type !== b.type) {
        errors.push(`pre-existing column changed in ${table} at position ${i}: ${b.name}`);
      }
    }
  }
  return { valid: errors.length === 0, errors };
}

// Builds a scratch DB by applying every existing migration (numeric order),
// snapshots it, applies the new migration, re-snapshots, and diffs. If every
// statement in the new migration is idempotent (CREATE ... IF NOT EXISTS
// shapes only), re-applies it once more to prove that claim too - a plain ADD
// COLUMN is skipped here since SQLite hard-errors on a repeat ADD COLUMN by
// design (the runtime's add_column_if_missing guard covers that case, not a
// second raw apply).
async function scratchApplyNoRewrite({ worktreePath, relPath, existingNumbers, execFn, statements }) {
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t4-scratch-"));
  const dbPath = path.join(tmpDir, "scratch.db");
  try {
    const migrationsDir = path.join(worktreePath, MIGRATIONS_DIR);
    const existingFiles = (await fs.readdir(migrationsDir))
      .filter((f) => f !== path.basename(relPath) && /^\d{4}_.+\.sql$/.test(f))
      .sort();
    for (const file of existingFiles) {
      await applyMigrationFile(execFn, dbPath, path.join(migrationsDir, file));
    }
    const before = await snapshotSchema(execFn, dbPath);

    const newFileAbs = path.join(worktreePath, relPath);
    await applyMigrationFile(execFn, dbPath, newFileAbs);
    const after = await snapshotSchema(execFn, dbPath);

    const diff = diffSnapshots(before, after);
    if (!diff.valid) return diff;

    const allIdempotent = statements.every((s) => s.idempotent);
    if (allIdempotent) {
      await applyMigrationFile(execFn, dbPath, newFileAbs);
      const reapplied = await snapshotSchema(execFn, dbPath);
      const reDiff = diffSnapshots(after, reapplied);
      if (!reDiff.valid) {
        return { valid: false, errors: reDiff.errors.map((e) => `idempotent re-apply changed schema: ${e}`) };
      }
    }
    return { valid: true, errors: [] };
  } catch (error) {
    return { valid: false, errors: [`scratch-apply failed: ${error.message}`] };
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
  }
}

// ---- entry point -------------------------------------------------------------

// validateT4Migration({ worktreePath, params: { name }, opts }) -> { valid, errors }
// opts.execFn is DI for the sqlite3 CLI ONLY - git status always shells the
// real git, mirroring t3Route.js's DI boundary.
export async function validateT4Migration({ worktreePath, params = {}, opts = {} } = {}) {
  const { name } = params;
  if (!name) {
    return { valid: false, errors: ["T4 validator requires params.name"] };
  }
  const execFn = opts.execFn ?? defaultExecFile;

  const discipline = await checkFileDiscipline({ worktreePath, name });
  if (!discipline.valid) return discipline;

  const absFilePath = path.join(worktreePath, discipline.relPath);
  let sql;
  try {
    sql = await fs.readFile(absFilePath, "utf8");
  } catch (error) {
    return { valid: false, errors: [`could not read migration file: ${error.message}`] };
  }

  const owned = touchesDerivedOwnedTable(sql);
  if (owned) {
    return { valid: false, errors: [`migration touches a derived-owned table ('${owned}') - derived state is rebuilt, never migrated`] };
  }

  const classified = classifyMigration(sql);
  if (!classified.valid) return classified;

  return scratchApplyNoRewrite({
    worktreePath,
    relPath: discipline.relPath,
    existingNumbers: discipline.existingNumbers,
    execFn,
    statements: classified.statements,
  });
}

export const t4MigrationValidator = { name: "t4Migration", run: validateT4Migration };
