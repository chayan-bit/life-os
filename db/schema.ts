// Drizzle schema mirroring migrations/0001_core.sql and migrations/0002_control_plane.sql,
// plus every later migration that touches one of these tables (0007, 0013, 0015, 0016 so
// far - see the inline comments below each affected column/table). Tables introduced by
// other migrations (vcs_refs, marketplace, memory, ...) are queried via raw SQL elsewhere
// and are intentionally not declared here.
// One module, imported by both the Worker (@libsql/client/web) and the Mac
// (embedded replica) - see client.worker.ts / client.mac.ts in this package.
// Source of truth for column shape is the SQL migrations; keep this in sync by hand,
// there is no migration generation step in this repo (migrations/ is applied directly).
// schema.test.ts is a mechanical parity gate: it applies every migrations/*.sql file to an
// in-memory DB and asserts every column declared here actually exists on the real table.
import { sql } from "drizzle-orm";
import {
  sqliteTable,
  text,
  integer,
  real,
  index,
  uniqueIndex,
} from "drizzle-orm/sqlite-core";

// ---------------------------------------------------------------------------
// 0001_core.sql - data plane
// ---------------------------------------------------------------------------

export const workspaces = sqliteTable("workspaces", {
  id: text("id").primaryKey(),
  name: text("name").notNull(),
  plan: text("plan").default("free"),
  limits: text("limits").notNull().default("{}"),
  createdAt: integer("created_at").notNull(),
  updatedAt: integer("updated_at").notNull(),
});

export const entities = sqliteTable(
  "entities",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    module: text("module").notNull(),
    type: text("type").notNull(),
    parentId: text("parent_id"),
    title: text("title"),
    status: text("status"),
    tier: text("tier"),
    attrs: text("attrs").notNull().default("{}"),
    source: text("source"),
    blobRef: text("blob_ref"),
    createdAt: integer("created_at").notNull(),
    updatedAt: integer("updated_at").notNull(),
    due: integer("due").generatedAlwaysAs(
      sql`json_extract(attrs, '$.due')`,
      { mode: "virtual" },
    ),
  },
  (table) => [
    index("ix_entities_ws_module_type").on(
      table.workspaceId,
      table.module,
      table.type,
    ),
    index("ix_entities_parent").on(table.parentId),
    index("ix_entities_due").on(table.workspaceId, table.due),
  ],
);

export const edges = sqliteTable(
  "edges",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    srcId: text("src_id").notNull(),
    dstId: text("dst_id"),
    dstRef: text("dst_ref"),
    rel: text("rel").notNull(),
    state: text("state").default("accepted"),
    createdBy: text("created_by"),
    createdAt: integer("created_at").notNull(),
  },
  (table) => [
    index("ix_edges_src").on(table.workspaceId, table.srcId),
    index("ix_edges_dst").on(table.workspaceId, table.dstId),
  ],
);

// Append-only (docs/ARCHITECTURE.md hard rules): no UPDATE/DELETE helper
// should ever be added against this table in application code.
export const events = sqliteTable(
  "events",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    ts: integer("ts").notNull(),
    type: text("type").notNull(),
    entityId: text("entity_id"),
    actor: text("actor"),
    attrs: text("attrs").default("{}"),
    // harness run-log columns (events doubles as the run log)
    runId: text("run_id"),
    tier: text("tier"),
    model: text("model"),
    tokensIn: integer("tokens_in"),
    tokensOut: integer("tokens_out"),
    cost: real("cost"),
    latencyMs: integer("latency_ms"),
    error: text("error"),
    outcome: text("outcome"),
    evalScore: real("eval_score"),
    gated: integer("gated").default(0),
    // migrations/0015_events_caused_by.sql - causal pointer memory/consolidation follows.
    causedByEventId: text("caused_by_event_id"),
    // migrations/0016_events_schema_version.sql - versioned replay; pre-existing rows default to 1.
    schemaVersion: integer("schema_version").notNull().default(1),
  },
  (table) => [
    index("ix_events_ws_ts").on(table.workspaceId, table.ts),
    index("ix_events_type").on(table.workspaceId, table.type),
  ],
);

export const annotations = sqliteTable(
  "annotations",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    entityId: text("entity_id"),
    kind: text("kind").notNull().default("note"),
    body: text("body"),
    anchor: text("anchor"),
    attrs: text("attrs").notNull().default("{}"),
    createdBy: text("created_by"),
    createdAt: integer("created_at").notNull(),
    updatedAt: integer("updated_at").notNull(),
  },
  (table) => [
    index("ix_annotations_entity").on(table.workspaceId, table.entityId),
    index("ix_annotations_kind").on(table.workspaceId, table.kind),
  ],
);

export const jobs = sqliteTable(
  "jobs",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    kind: text("kind").notNull(),
    payload: text("payload").notNull().default("{}"),
    status: text("status").notNull().default("queued"),
    priority: integer("priority").default(0),
    runAfter: integer("run_after"),
    claimedBy: text("claimed_by"),
    claimedAt: integer("claimed_at"),
    attempts: integer("attempts").default(0),
    createdAt: integer("created_at").notNull(),
  },
  (table) => [
    // Mirrors migrations/0001_core.sql: (status, priority DESC, created_at).
    // priority DESC matters - lifeos-drain claims the highest-priority job first.
    // This drizzle version expresses column ordering via raw sql (IndexColumn =
    // SQLiteColumn | SQL), not a `.desc()` column method.
    index("ix_jobs_claim").on(table.status, sql`${table.priority} DESC`, table.createdAt),
  ],
);

export const moduleRequests = sqliteTable("module_requests", {
  id: text("id").primaryKey(),
  workspaceId: text("workspace_id")
    .notNull()
    .references(() => workspaces.id),
  prompt: text("prompt").notNull(),
  status: text("status").notNull().default("queued"),
  error: text("error"),
  // Telegram chat to notify on install/failure (issue #78, 0004 migration).
  // NULL for API-originated requests, which have no chat behind them.
  chatId: text("chat_id"),
  createdAt: integer("created_at").notNull(),
  updatedAt: integer("updated_at").notNull(),
});

// ---------------------------------------------------------------------------
// 0002_control_plane.sql - control plane
// ---------------------------------------------------------------------------

export const users = sqliteTable("users", {
  id: text("id").primaryKey(),
  email: text("email").notNull().unique(),
  name: text("name"),
  // migrations/0007_auth_password.sql - nullable: the seeded personal user and
  // any pre-#100 row has none, so login for those fails closed until a
  // password is set via register/reset.
  passwordHash: text("password_hash"),
  createdAt: integer("created_at").notNull(),
  updatedAt: integer("updated_at").notNull(),
});

export const memberships = sqliteTable(
  "memberships",
  {
    id: text("id").primaryKey(),
    userId: text("user_id")
      .notNull()
      .references(() => users.id),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    role: text("role").notNull().default("member"),
    createdAt: integer("created_at").notNull(),
    updatedAt: integer("updated_at").notNull(),
  },
  (table) => [
    uniqueIndex("ux_memberships_user_workspace").on(
      table.userId,
      table.workspaceId,
    ),
  ],
);

export const connections = sqliteTable(
  "connections",
  {
    id: text("id").primaryKey(),
    workspaceId: text("workspace_id")
      .notNull()
      .references(() => workspaces.id),
    provider: text("provider").notNull(),
    accountHandle: text("account_handle"),
    nangoConnectionId: text("nango_connection_id"),
    secretEnc: text("secret_enc"),
    scopes: text("scopes"),
    expiresAt: integer("expires_at"),
    status: text("status").default("active"),
    createdAt: integer("created_at").notNull(),
  },
  (table) => [
    index("ix_connections_ws_provider").on(table.workspaceId, table.provider),
  ],
);

// `plans`/`subscriptions` (the billing/quota catalog from this file's
// original 0002_control_plane.sql) were dropped by migrations/
// 0013_remove_billing.sql (issue #104) - this is self-hosted,
// bring-your-own-database-and-AI-model software with nothing to meter or
// bill, and neither table was ever read by any route. Do not re-add them
// here without a matching migration that recreates the tables.
