//! Shared test scaffolding: an in-file libSQL DB with the canonical tables
//! this crate touches (stub `workspaces`/`entities` + real `events` shape +
//! the 0017 memory read models), and an optional attached derived schema
//! carrying the 0018 memory FTS index.

use libsql::{params, Builder, Connection};

/// The real memory migration, embedded so unit tests exercise the exact DDL
/// production runs (same include_str! pattern as lifeos-api/src/db.rs).
const MIGRATION_MEMORY: &str = include_str!("../../../migrations/0017_memory.sql");
const MIGRATION_DERIVED_MEMORY: &str = include_str!("../../../migrations/0018_derived_memory.sql");
const MIGRATION_MEMORY_COMMUNITIES: &str =
    include_str!("../../../migrations/0019_memory_communities.sql");

const STUB_CORE: &str = "
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS entities (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT NOT NULL, type TEXT NOT NULL,
  title TEXT, status TEXT, attrs TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER NOT NULL, type TEXT NOT NULL,
  entity_id TEXT, actor TEXT, attrs TEXT DEFAULT '{}',
  caused_by_event_id TEXT, schema_version INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL,
  payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued',
  priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER,
  attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL
);
INSERT OR IGNORE INTO workspaces (id, name, created_at, updated_at) VALUES ('ws_1', 'one', 1, 1);
INSERT OR IGNORE INTO workspaces (id, name, created_at, updated_at) VALUES ('ws_2', 'two', 1, 1);
";

/// Fresh in-memory-style DB (`:memory:` isn't supported by this builder, so a
/// unique temp file) with all tables. No derived schema attached: `d.*`
/// writes are best-effort and tests without it cover the degraded path.
pub async fn test_conn() -> Connection {
    let dir = std::env::temp_dir().join(format!("lifeos-mem-test-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.db");
    let db = Builder::new_local(path.to_str().unwrap()).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(STUB_CORE).await.unwrap();
    conn.execute_batch(MIGRATION_MEMORY).await.unwrap();
    conn.execute_batch(MIGRATION_MEMORY_COMMUNITIES).await.unwrap();
    conn
}

/// Same, plus a physically separate derived file ATTACHed as `d` with the
/// memory FTS schema - the production topology.
pub async fn test_conn_with_derived() -> Connection {
    let dir = std::env::temp_dir().join(format!("lifeos-mem-test-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_path = dir.join("test.db");
    let derived_path = dir.join("test.derived.db");

    // Bootstrap derived DDL by opening the file directly (triggers/FTS can't
    // be created through an ATTACH alias) - mirrors db.rs::bootstrap_derived.
    let ddb = Builder::new_local(derived_path.to_str().unwrap()).build().await.unwrap();
    ddb.connect().unwrap().execute_batch(MIGRATION_DERIVED_MEMORY).await.unwrap();

    let db = Builder::new_local(main_path.to_str().unwrap()).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(STUB_CORE).await.unwrap();
    conn.execute_batch(MIGRATION_MEMORY).await.unwrap();
    conn.execute_batch(MIGRATION_MEMORY_COMMUNITIES).await.unwrap();
    conn.execute(
        &format!("ATTACH DATABASE 'file:{}' AS d", derived_path.to_str().unwrap()),
        (),
    )
    .await
    .unwrap();
    conn
}

/// Append one event row, the way every tier does it.
#[allow(clippy::too_many_arguments)]
pub async fn seed_event(
    conn: &Connection,
    ws: &str,
    id: &str,
    ts: i64,
    event_type: &str,
    entity_id: Option<&str>,
    actor: &str,
    attrs: serde_json::Value,
    caused_by: Option<&str>,
) {
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs, caused_by_event_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, ws, ts, event_type, entity_id, actor, attrs.to_string(), caused_by],
    )
    .await
    .unwrap();
}

pub async fn seed_entity(conn: &Connection, ws: &str, id: &str, module: &str, title: &str) {
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, title, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'item', ?4, 1, 1)",
        params![id, ws, module, title],
    )
    .await
    .unwrap();
}
