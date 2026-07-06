// Shared in-memory schema for tests - trimmed migrations/0001_core.sql DDL
// (plus events.caused_by_event_id/schema_version from 0015/0016 - db/
// schema.ts declares them, so drizzle inserts include them), against
// @lifeos/db's own drizzle-orm instance (db/client.local.ts).
import { createLocalDb, type LocalDb } from "@lifeos/db/client/local";
import { sql } from "@lifeos/db/query";

const SCHEMA_SQL = `
CREATE TABLE entities (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  module TEXT NOT NULL,
  type TEXT NOT NULL,
  parent_id TEXT,
  title TEXT,
  status TEXT,
  tier TEXT,
  attrs TEXT NOT NULL DEFAULT '{}',
  source TEXT,
  blob_ref TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  due INTEGER GENERATED ALWAYS AS (json_extract(attrs, '$.due')) VIRTUAL
);
CREATE TABLE events (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  type TEXT NOT NULL,
  entity_id TEXT,
  actor TEXT,
  attrs TEXT DEFAULT '{}',
  run_id TEXT,
  tier TEXT,
  model TEXT,
  tokens_in INTEGER,
  tokens_out INTEGER,
  cost REAL,
  latency_ms INTEGER,
  error TEXT,
  outcome TEXT,
  eval_score REAL,
  gated INTEGER DEFAULT 0,
  caused_by_event_id TEXT,
  schema_version INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'queued',
  priority INTEGER DEFAULT 0,
  run_after INTEGER,
  claimed_by TEXT,
  claimed_at INTEGER,
  attempts INTEGER DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE module_requests (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued',
  error TEXT,
  chat_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
`;

export async function createTestDb(): Promise<LocalDb> {
  const db = createLocalDb("file::memory:");
  for (const stmt of SCHEMA_SQL.split(";").map((s) => s.trim()).filter(Boolean)) {
    await db.run(sql.raw(stmt));
  }
  return db;
}
