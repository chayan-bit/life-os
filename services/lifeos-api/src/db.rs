//! Database bootstrap: open the libSQL file, run migrations, seed the personal
//! tenant. Migrations are embedded at compile time so the binary runs correctly
//! regardless of the working directory it is launched from.

use crate::config::{Config, DEFAULT_WORKSPACE};
use crate::ids::now_secs;
use libsql::{Builder, Connection, Database};
use std::time::Duration;

/// Migrations are baked into the binary. Paths are relative to this source file
/// (`services/lifeos-api/src/db.rs` -> repo-root `migrations/`).
const MIGRATION_CORE: &str = include_str!("../../../migrations/0001_core.sql");
const MIGRATION_CONTROL: &str = include_str!("../../../migrations/0002_control_plane.sql");
/// Applied to the attached derived schema `d` (FTS5 lexical index). Separate
/// from core/control because the derived DB is never synced and is rebuildable.
const MIGRATION_DERIVED: &str = include_str!("../../../migrations/0003_derived.sql");
/// `ALTER TABLE ADD COLUMN` isn't naturally idempotent like the `CREATE TABLE
/// IF NOT EXISTS` migrations above - `add_column_if_missing` below guards it.
const MIGRATION_MODULE_REQUESTS_CHAT_ID: &str =
    include_str!("../../../migrations/0004_module_requests_chat_id.sql");
/// `vcs_refs` (lifeos-vcs branch/tag pointers, issue #84) - a new `CREATE
/// TABLE IF NOT EXISTS`, naturally idempotent like core/control.
const MIGRATION_VCS_REFS: &str = include_str!("../../../migrations/0005_vcs_refs.sql");
/// `configs` (release-loop candidate configs, issue #98) - a new `CREATE
/// TABLE IF NOT EXISTS`, naturally idempotent like core/control/vcs_refs.
const MIGRATION_RELEASE_CONFIGS: &str = include_str!("../../../migrations/0006_release_configs.sql");
/// `users.password_hash` (real login, issue #100) - `ALTER TABLE ADD COLUMN`,
/// guarded by `add_column_if_missing` like `MIGRATION_MODULE_REQUESTS_CHAT_ID`.
const MIGRATION_AUTH_PASSWORD: &str = include_str!("../../../migrations/0007_auth_password.sql");
/// `sessions` (refresh-token rotation, issue #100) - a new `CREATE TABLE IF
/// NOT EXISTS`, naturally idempotent.
const MIGRATION_AUTH_SESSIONS: &str = include_str!("../../../migrations/0008_auth_sessions.sql");
/// `module_packages` (marketplace publish/install, issues #101/#102) - a new
/// `CREATE TABLE IF NOT EXISTS`, naturally idempotent.
const MIGRATION_MARKETPLACE: &str = include_str!("../../../migrations/0009_marketplace.sql");
/// `push_subscriptions` (PWA Web Push, issue #103) - a new `CREATE TABLE IF
/// NOT EXISTS`, naturally idempotent.
const MIGRATION_PUSH_SUBSCRIPTIONS: &str = include_str!("../../../migrations/0010_push_subscriptions.sql");
/// `workspaces.envelope_key_enc` (database-per-workspace, issue #104) -
/// `ALTER TABLE ADD COLUMN`, guarded by `add_column_if_missing`.
const MIGRATION_WORKSPACE_ENVELOPE_KEY: &str =
    include_str!("../../../migrations/0011_workspace_envelope_key.sql");
/// `workspace_databases` (issue #104) - a new `CREATE TABLE IF NOT EXISTS`,
/// naturally idempotent.
const MIGRATION_WORKSPACE_DATABASES: &str = include_str!("../../../migrations/0012_workspace_databases.sql");
/// Drops the unused billing/quota seam (`plans`/`subscriptions`, issue #104) -
/// this is a self-hosted, bring-your-own-database project with no product to
/// meter or bill. `DROP TABLE IF EXISTS` is naturally idempotent.
const MIGRATION_REMOVE_BILLING: &str = include_str!("../../../migrations/0013_remove_billing.sql");

/// `blob_backends` (pluggable-storage backend index, issue #106) - a new
/// `CREATE TABLE IF NOT EXISTS`, naturally idempotent.
const MIGRATION_BLOB_BACKENDS: &str = include_str!("../../../migrations/0014_blob_backends.sql");

/// `events.caused_by_event_id` (memory causal pointer, issue #111) - `ALTER
/// TABLE ADD COLUMN`, guarded by `add_column_if_missing`.
const MIGRATION_EVENTS_CAUSED_BY: &str = include_str!("../../../migrations/0015_events_caused_by.sql");
/// `events.schema_version` (versioned replay, issue #111) - `ALTER TABLE ADD
/// COLUMN`, guarded by `add_column_if_missing`.
const MIGRATION_EVENTS_SCHEMA_VERSION: &str =
    include_str!("../../../migrations/0016_events_schema_version.sql");
/// Memory read models (issues #111-#118) - new `CREATE TABLE IF NOT EXISTS`
/// tables, naturally idempotent. All rebuildable from `events`.
const MIGRATION_MEMORY: &str = include_str!("../../../migrations/0017_memory.sql");
/// FTS5 over memory_nodes - applied to the DERIVED file (never synced),
/// alongside 0003, in `bootstrap_derived`.
const MIGRATION_DERIVED_MEMORY: &str = include_str!("../../../migrations/0018_derived_memory.sql");

/// The canonical DB plus its live connection. `database` is retained by the caller
/// so the embedded-replica's background replicator stays alive (dropping it would
/// stop syncing) and so an explicit `database.sync()` can be triggered.
pub struct Db {
    pub database: Database,
    pub conn: Connection,
}

/// Open the canonical DB, apply migrations, seed the default tenant, and ATTACH the
/// separate derived DB.
///
/// Two modes (DATA-MODEL §4):
/// - **local-first (default):** `db_path` is a plain local libSQL file. Fully
///   offline; writes never need the network. This is the personal-Mac default.
/// - **embedded replica:** when `turso_url` + `turso_token` are set, `db_path`
///   becomes a replica of the Turso primary with read-your-writes and periodic
///   background pull (`sync_interval_secs`).
///
/// Conflict model is **last-push-wins at row granularity over the whole `attrs`
/// blob - NOT last-writer-wins on `updated_at`** (libSQL's actual behavior). The
/// defenses are single-writer-per-row tiering (bot vs Mac lanes) plus the
/// append-only `events` log as the reconciliation source of truth.
///
/// Offline writes against a remote replica (the JS client's `offline:true`, Turso
/// Sync public beta) are **not** available in the Rust libSQL 0.6 client; the
/// local-first plain-file mode is how we stay offline-capable until that lands.
pub async fn connect(config: &Config) -> Result<Db, libsql::Error> {
    let database = match (&config.turso_url, &config.turso_token) {
        (Some(url), Some(token)) => {
            tracing::info!("opening embedded replica against Turso primary");
            Builder::new_remote_replica(&config.db_path, url.clone(), token.clone())
                .read_your_writes(true)
                .sync_interval(Duration::from_secs(config.sync_interval_secs))
                .build()
                .await?
        }
        _ => {
            tracing::info!("opening local-first canonical DB (no Turso sync configured)");
            Builder::new_local(&config.db_path).build().await?
        }
    };

    let conn = database.connect()?;
    // Enforce the FK constraints the schema declares. SQLite/libSQL default this
    // OFF per connection, so without it every `workspace_id`/`user_id` FK is
    // decorative and orphaned cross-tenant rows can be inserted - the exact
    // integrity guard the multi-tenant model relies on. Must run before any
    // writes (migrations/seed) on this connection.
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    run_migrations(&conn).await?;
    seed(&conn).await?;
    // Create the derived schema in its own file FIRST (triggers/FTS DDL can't be
    // schema-qualified through an ATTACH alias), then attach it for querying.
    bootstrap_derived(&config.derived_db_path).await?;
    attach_derived(&conn, &config.derived_db_path).await?;
    // Build the lexical index from the canonical DB so search works at boot.
    if let Err(e) = rebuild_derived_index(&conn).await {
        tracing::warn!("initial derived index rebuild failed (search degraded): {e}");
    }

    Ok(Db { database, conn })
}

/// ATTACH the never-synced derived DB as schema `d`. Physically separate from the
/// canonical file so FTS5/sqlite-vec state can never be pushed to the primary
/// (libSQL has no table-level sync-exclusion flag). See DATA-MODEL §5.
pub async fn attach_derived(conn: &Connection, derived_path: &str) -> Result<(), libsql::Error> {
    // `?` would be parsed as a bind; ATTACH needs the literal path. The path comes
    // from our own config, never from request input, so interpolation is safe here.
    conn.execute(&format!("ATTACH DATABASE 'file:{derived_path}' AS d"), ())
        .await?;
    tracing::info!("attached derived DB '{derived_path}' as schema 'd' (never synced)");
    Ok(())
}

/// Create the derived FTS5 schema by opening the derived file directly (DDL with
/// triggers can't be created through an ATTACH alias). Idempotent. The semantic
/// `entity_vec` table is created by server/memvec.py, not here (vec0 is not
/// loadable from the Rust libSQL build).
pub async fn bootstrap_derived(derived_path: &str) -> Result<(), libsql::Error> {
    let db = Builder::new_local(derived_path).build().await?;
    let conn = db.connect()?;
    conn.execute_batch(MIGRATION_DERIVED).await?;
    conn.execute_batch(MIGRATION_DERIVED_MEMORY).await?;
    tracing::info!("derived FTS5 schema bootstrapped in '{derived_path}'");
    Ok(())
}

/// Rebuild the lexical index from the canonical entities table. Cheap full
/// rebuild; the derived DB is disposable by design (DATA-MODEL §6).
pub async fn rebuild_derived_index(conn: &Connection) -> Result<(), libsql::Error> {
    conn.execute("DELETE FROM d.entities_idx", ()).await?;
    conn.execute(
        "INSERT INTO d.entities_idx (id, workspace_id, module, type, title, status, attrs, updated_at) \
         SELECT id, workspace_id, module, type, title, status, attrs, updated_at FROM main.entities",
        (),
    )
    .await?;
    Ok(())
}

/// Best-effort incremental upsert of one entity into the lexical index. Called
/// after entity create/update so search stays live without a full rebuild.
/// Errors are non-fatal: the boot rebuild reconciles any drift.
pub async fn index_entity(conn: &Connection, id: &str) -> Result<(), libsql::Error> {
    conn.execute(
        "INSERT INTO d.entities_idx (id, workspace_id, module, type, title, status, attrs, updated_at) \
         SELECT id, workspace_id, module, type, title, status, attrs, updated_at \
         FROM main.entities WHERE id = ?1 \
         ON CONFLICT(id) DO UPDATE SET \
            workspace_id=excluded.workspace_id, module=excluded.module, type=excluded.type, \
            title=excluded.title, status=excluded.status, attrs=excluded.attrs, \
            updated_at=excluded.updated_at",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

pub async fn run_migrations(conn: &Connection) -> Result<(), libsql::Error> {
    conn.execute_batch(MIGRATION_CORE).await?;
    conn.execute_batch(MIGRATION_CONTROL).await?;
    add_column_if_missing(conn, "module_requests", "chat_id", MIGRATION_MODULE_REQUESTS_CHAT_ID).await?;
    conn.execute_batch(MIGRATION_VCS_REFS).await?;
    conn.execute_batch(MIGRATION_RELEASE_CONFIGS).await?;
    add_column_if_missing(conn, "users", "password_hash", MIGRATION_AUTH_PASSWORD).await?;
    conn.execute_batch(MIGRATION_AUTH_SESSIONS).await?;
    conn.execute_batch(MIGRATION_MARKETPLACE).await?;
    conn.execute_batch(MIGRATION_PUSH_SUBSCRIPTIONS).await?;
    add_column_if_missing(conn, "workspaces", "envelope_key_enc", MIGRATION_WORKSPACE_ENVELOPE_KEY).await?;
    conn.execute_batch(MIGRATION_WORKSPACE_DATABASES).await?;
    conn.execute_batch(MIGRATION_REMOVE_BILLING).await?;
    conn.execute_batch(MIGRATION_BLOB_BACKENDS).await?;
    add_column_if_missing(conn, "events", "caused_by_event_id", MIGRATION_EVENTS_CAUSED_BY).await?;
    add_column_if_missing(conn, "events", "schema_version", MIGRATION_EVENTS_SCHEMA_VERSION).await?;
    conn.execute_batch(MIGRATION_MEMORY).await?;
    tracing::info!("migrations applied (core + control plane)");
    Ok(())
}

/// Runs an `ALTER TABLE ADD COLUMN` migration only if the column isn't
/// already there, so `run_migrations` stays safe to call on every boot (a
/// second `ADD COLUMN` on the same column is a hard SQLite error, unlike the
/// `CREATE TABLE IF NOT EXISTS` migrations above).
async fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> Result<(), libsql::Error> {
    let mut rows = conn.query(&format!("PRAGMA table_info({table})"), ()).await?;
    let mut exists = false;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == column {
            exists = true;
            break;
        }
    }
    if !exists {
        conn.execute_batch(ddl).await?;
    }
    Ok(())
}

/// Idempotently seed the single personal workspace, its owner user, and the
/// membership joining them. Safe to call on every boot.
pub async fn seed(conn: &Connection) -> Result<(), libsql::Error> {
    let now = now_secs();

    // No billing/quota catalog to seed (issue #104 removed it) - `plan` on
    // workspaces stays a free-text label only, never gated on.
    let exists = scalar_exists(conn, "SELECT 1 FROM workspaces WHERE id = ?1", DEFAULT_WORKSPACE).await?;
    if !exists {
        tracing::info!("seeding default personal workspace");
        conn.execute(
            "INSERT INTO workspaces (id, name, plan, limits, created_at, updated_at) \
             VALUES (?1, ?2, 'free', '{}', ?3, ?4)",
            libsql::params![DEFAULT_WORKSPACE, "Personal Workspace", now, now],
        )
        .await?;
    }

    let user_exists = scalar_exists(conn, "SELECT 1 FROM users WHERE email = ?1", "chayan@lifeos.app").await?;
    if !user_exists {
        tracing::info!("seeding default user + membership");
        conn.execute(
            "INSERT INTO users (id, email, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params!["usr_chayan", "chayan@lifeos.app", "Chayan Aggarwal", now, now],
        )
        .await?;
        conn.execute(
            "INSERT INTO memberships (id, user_id, workspace_id, role, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 'owner', ?4, ?5)",
            libsql::params!["memb_default", "usr_chayan", DEFAULT_WORKSPACE, now, now],
        )
        .await?;
    }

    Ok(())
}

/// True if the given single-param query returns at least one row.
async fn scalar_exists(conn: &Connection, sql: &str, param: &str) -> Result<bool, libsql::Error> {
    let mut rows = conn.query(sql, libsql::params![param]).await?;
    Ok(rows.next().await?.is_some())
}

/// True if a workspace row exists. Used to validate tenant scope before writes.
pub async fn workspace_exists(conn: &Connection, workspace_id: &str) -> Result<bool, libsql::Error> {
    scalar_exists(conn, "SELECT 1 FROM workspaces WHERE id = ?1", workspace_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(path: &str) -> Config {
        Config {
            db_path: path.to_string(),
            turso_url: None,
            turso_token: None,
            sync_interval_secs: 60,
            derived_db_path: format!("{path}.derived"),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            jwt_secret: "test".into(),
            agent_cwd: None,
            agent_timeout_secs: 30,
            server_dir: "server".to_string(),
            nango_server_url: None,
            nango_secret_key: None,
            kite_api_key: None,
            kite_api_secret: None,
            secret_encryption_key: None,
            gowa_base_url: None,
            gowa_basic_auth: None,
            gowa_webhook_secret: None,
            browser_script_path: None,
            vcs_blob_root: format!("{path}.blobs"),
            marketplace_signing_key: None,
            turso_platform_api_token: None,
            turso_org_slug: None,
        }
    }

    async fn fresh(path: &str) -> Connection {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}.derived"));
        connect(&test_config(path)).await.unwrap().conn
    }

    #[tokio::test]
    async fn migrations_and_seed_are_idempotent() {
        let path = "test_db_seed.db";
        let conn = fresh(path).await;
        // Second connect must not error (idempotent seed).
        run_migrations(&conn).await.unwrap();
        seed(&conn).await.unwrap();

        assert!(workspace_exists(&conn, DEFAULT_WORKSPACE).await.unwrap());

        let mut rows = conn
            .query("SELECT name FROM users WHERE email = ?1", libsql::params!["chayan@lifeos.app"])
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("user seeded");
        let name: String = row.get(0).unwrap();
        assert_eq!(name, "Chayan Aggarwal");

        // Acceptance (#2): seed creates exactly one workspace, one user, one owner
        // membership - even after a second migrate+seed pass.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM workspaces").await, 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM users").await, 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM memberships WHERE role='owner'").await, 1);
        // Billing/quota catalog removed entirely (issue #104) - no 'plans'/
        // 'subscriptions' tables to seed or assert on.
        let mut rows = conn.query("SELECT name FROM sqlite_master WHERE type='table' AND name IN ('plans','subscriptions')", ()).await.unwrap();
        assert!(rows.next().await.unwrap().is_none(), "plans/subscriptions tables must be dropped");

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn derived_db_is_attached_as_a_separate_file() {
        let path = "test_db_derived.db";
        let _ = std::fs::remove_file(path);
        let cfg = test_config(path);
        let _ = std::fs::remove_file(&cfg.derived_db_path);
        let conn = connect(&cfg).await.unwrap().conn;

        // Derived schema `d` is usable.
        conn.execute("CREATE TABLE d.derived_probe (x INTEGER)", ()).await.unwrap();
        conn.execute("INSERT INTO d.derived_probe (x) VALUES (42)", ()).await.unwrap();
        let mut rows = conn.query("SELECT x FROM d.derived_probe", ()).await.unwrap();
        let x: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(x, 42);

        // The derived table exists ONLY in `d`, never in the canonical (synced) DB.
        let in_main = count(
            &conn,
            "SELECT COUNT(*) FROM main.sqlite_master WHERE name = 'derived_probe'",
        )
        .await;
        assert_eq!(in_main, 0, "derived state must not land in the canonical DB");

        // It is a physically distinct file (this is what enforces 'never synced').
        assert!(std::path::Path::new(&cfg.derived_db_path).exists());
        assert_ne!(cfg.derived_db_path, cfg.db_path);

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(&cfg.derived_db_path);
    }

    /// Helper: run a `SELECT COUNT(*)` and return the integer.
    async fn count(conn: &Connection, sql: &str) -> i64 {
        let mut rows = conn.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn annotations_table_and_due_generated_column_exist() {
        let path = "test_db_schema.db";
        let conn = fresh(path).await;

        // annotations table (spec §2.4) accepts a workspace-scoped note.
        conn.execute(
            "INSERT INTO annotations (id, workspace_id, entity_id, kind, body, created_by, created_at, updated_at) \
             VALUES ('anno_1', ?1, 'ent_1', 'note', 'hello', 'user', 1, 1)",
            libsql::params![DEFAULT_WORKSPACE],
        )
        .await
        .unwrap();

        // `due` is a GENERATED VIRTUAL column lifted from attrs (§7); it must be
        // queryable and reflect json_extract(attrs,'$.due') without an explicit write.
        conn.execute(
            "INSERT INTO entities (id, workspace_id, module, type, attrs, created_at, updated_at) \
             VALUES ('ent_1', ?1, 'tasks', 'task', '{\"due\": 1700000000}', 1, 1)",
            libsql::params![DEFAULT_WORKSPACE],
        )
        .await
        .unwrap();

        let mut rows = conn
            .query("SELECT due FROM entities WHERE id = 'ent_1'", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("entity row");
        let due: i64 = row.get(0).unwrap();
        assert_eq!(due, 1_700_000_000);

        let _ = std::fs::remove_file(path);
    }
}
