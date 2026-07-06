// Mechanical parity gate between this file's drizzle tables and the real SQL
// schema (finding 14 of the correctness audit): schema.ts's header claims it
// is "hand-synced" to migrations/*.sql, but nothing ever checked that - three
// drifts (plans/subscriptions surviving migrations/0013_remove_billing.sql's
// DROP, users missing 0007's password_hash, events missing 0015/0016's
// caused_by_event_id/schema_version) shipped unnoticed. This applies every
// migrations/*.sql file, in filename order, to a throwaway in-memory libSQL
// DB, then asserts every column a drizzle table declares actually exists on
// the corresponding real table (a name-level subset check - drizzle is
// allowed to omit columns of a table it declares, e.g. it never modeled every
// column of every migration, but it must never claim a column that isn't
// really there).
//
// This package has no test runner of its own (no vitest devDependency, no
// "test" script) - the Worker's already does, with the exact `describe`/`it`
// shape used below, so this runs there. Run it with an explicit root
// override so Vitest's file-discovery glob (relative to its own root)
// reaches this file, which lives one directory up:
//
//   cd worker && npx vitest run --root .. db/schema.test.ts
//
// Module resolution still works normally under that invocation: Node/Vite
// resolve bare imports (`@libsql/client`, `drizzle-orm`) from this file's own
// real directory, i.e. db/node_modules, exactly as `npm run build`/`check` do
// from within this package. The `@ts-expect-error` below is only needed for
// this package's OWN `tsc -p tsconfig.json` (build/check): db has no vitest
// devDependency of its own (adding one just for this file's types isn't
// worth the extra install), and Vitest itself doesn't type-check test files
// before running them (it transforms via esbuild), so this costs nothing at
// either the build step or the actual test run.
// @ts-expect-error - no vitest devDependency in this package (see note above);
// resolved fine at runtime by whichever project's Vitest actually runs this.
import { describe, expect, it } from "vitest";
import { createClient, type Client } from "@libsql/client";
import { getTableColumns, getTableName, type Table } from "drizzle-orm";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as schema from "./schema.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const MIGRATIONS_DIR = join(HERE, "..", "migrations");

// Every drizzle table this schema module exports - kept as a literal list
// (rather than reflecting over the module's exports) so a new table gets
// covered by this gate the moment someone adds it above, with an explicit
// reminder here rather than silent, maybe-partial coverage.
const TABLES: Table[] = [
  schema.workspaces,
  schema.entities,
  schema.edges,
  schema.events,
  schema.annotations,
  schema.jobs,
  schema.moduleRequests,
  schema.users,
  schema.memberships,
  schema.connections,
];

function migrationFiles(): string[] {
  return readdirSync(MIGRATIONS_DIR)
    .filter((f) => f.endsWith(".sql"))
    .sort();
}

async function applyMigrations(client: Client): Promise<void> {
  for (const file of migrationFiles()) {
    const sqlText = readFileSync(join(MIGRATIONS_DIR, file), "utf8");
    await client.executeMultiple(sqlText);
  }
}

async function realTableNames(client: Client): Promise<Set<string>> {
  const result = await client.execute("SELECT name FROM sqlite_master WHERE type = 'table'");
  return new Set(result.rows.map((row) => String(row.name)));
}

// `table_xinfo`, not `table_info`: this libSQL build's `table_info` silently
// omits generated/virtual columns (e.g. entities.due, migrations/0001_core.sql)
// - `table_xinfo` is the superset that also reports them (marked `hidden`).
async function realColumnNames(client: Client, tableName: string): Promise<string[]> {
  const result = await client.execute(`PRAGMA table_xinfo(${tableName})`);
  return result.rows.map((row) => String(row.name));
}

function declaredColumnNames(table: Table): string[] {
  const columns = getTableColumns(table);
  return Object.values(columns).map((column) => column.name);
}

describe("db/schema.ts <-> migrations/*.sql parity", () => {
  it("applies every migration file cleanly, in filename order", async () => {
    const client = createClient({ url: "file::memory:" });
    await expect(applyMigrations(client)).resolves.toBeUndefined();
    client.close();
  });

  it("declares every drizzle column as a real column of its table (no phantom/removed columns)", async () => {
    const client = createClient({ url: "file::memory:" });
    await applyMigrations(client);
    const tableNames = await realTableNames(client);

    for (const table of TABLES) {
      const name = getTableName(table);
      expect(tableNames.has(name), `table "${name}" declared in schema.ts does not exist in migrations/*.sql`).toBe(true);

      const real = new Set(await realColumnNames(client, name));
      for (const declared of declaredColumnNames(table)) {
        expect(real.has(declared), `schema.ts's "${name}.${declared}" has no matching column in the real table`).toBe(true);
      }
    }

    client.close();
  });

  // The three drifts this audit found and fixed (docs/ARCHITECTURE.md's
  // "hand-synced" claim was false until now) - asserted individually, on top
  // of the generic subset check above, so a regression on any one of them
  // fails loudly with a specific message rather than a generic diff.
  it("users carries migrations/0007_auth_password.sql's password_hash", async () => {
    const client = createClient({ url: "file::memory:" });
    await applyMigrations(client);

    const real = await realColumnNames(client, "users");
    expect(real).toContain("password_hash");
    expect(declaredColumnNames(schema.users)).toContain("password_hash");

    client.close();
  });

  it("events carries migrations/0015 and 0016's caused_by_event_id and schema_version", async () => {
    const client = createClient({ url: "file::memory:" });
    await applyMigrations(client);

    const real = await realColumnNames(client, "events");
    expect(real).toContain("caused_by_event_id");
    expect(real).toContain("schema_version");
    const declared = declaredColumnNames(schema.events);
    expect(declared).toContain("caused_by_event_id");
    expect(declared).toContain("schema_version");

    client.close();
  });

  it("no longer declares plans/subscriptions, which migrations/0013_remove_billing.sql dropped", async () => {
    const client = createClient({ url: "file::memory:" });
    await applyMigrations(client);

    const tableNames = await realTableNames(client);
    expect(tableNames.has("plans")).toBe(false);
    expect(tableNames.has("subscriptions")).toBe(false);
    expect(Object.keys(schema)).not.toContain("plans");
    expect(Object.keys(schema)).not.toContain("subscriptions");

    client.close();
  });
});
