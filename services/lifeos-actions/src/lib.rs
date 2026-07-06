//! lifeos-actions: the Life OS Actions engine (issue #93,
//! docs/PLATFORM-SYSTEMS.md §6) - "GitHub Actions for everything". A hot
//! event loop watches `events` and turns matching rows into `jobs`,
//! declaratively, with zero new code per automation.
//!
//! `lifeos-drain`'s poll loop calls `run_action_engine_tick` once per tick,
//! the same shape as its existing `reap_stuck` call. This crate has no
//! dependency on `lifeos-api` (same standalone style as `lifeos-drain`/
//! `lifeos-ingest`/`lifeos-pipelines`): it reads/writes `entities`/`events`/
//! `jobs` with its own small SQL, mirroring `audit::emit`'s INSERT shape by
//! hand.
//!
//! Scope of #93: a real (not manifest-driven) static rule registry seeded
//! with docs/PLATFORM-SYSTEMS.md §6's 3 examples - no module manifest
//! declares an `actions` array yet, same deferred-bridge gap
//! `lifeos-pipelines::pipeline_registry()` documents for `pipelines`. "A
//! declared action fires on its event and enqueues the right job" is the
//! acceptance bar; the enqueued `action` job's own execution is deferred
//! (an honest `Dispatch::Stub` in `lifeos-drain`, same as `module_build`/
//! `eval`/`reconcile` today). `if` conditions are a real but intentionally
//! minimal single-field-equality check over the triggering event's `attrs`,
//! not a general expression language (same minimalism precedent as #92's
//! `eval_stage_output`).
//!
//! "Zero new tables" (the doc's own words): the per-workspace incremental
//! cursor lives in one `entities` row (`module='actions', type='cursor'`),
//! keyed by a deterministic id so it can be upserted without a
//! lookup-then-insert race. `events.id` is a ULID (time-ordered), so
//! `WHERE id > cursor ORDER BY id ASC` is a correct incremental scan with
//! no extra ordering column needed.

use libsql::{params, Connection};
use serde_json::{json, Value};
use std::sync::Mutex;
use ulid::{Generator, Ulid};

static ID_GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());

fn new_id(prefix: &str) -> String {
    let ulid = ID_GENERATOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .generate()
        .unwrap_or_else(|_| Ulid::new());
    format!("{prefix}_{ulid}")
}

// --------------------------------------------------------------- registry

/// One declared automation. Field names match docs/PLATFORM-SYSTEMS.md §6's
/// `actions: [{on, if?, run}]` shape.
#[derive(Debug, Clone)]
pub struct ActionRule {
    pub id: &'static str,
    pub on: &'static str,
    /// Minimal `if` support: `(attrs field name, expected string value)`.
    pub if_attr: Option<(&'static str, &'static str)>,
    pub run_kind: &'static str,
    pub run: fn() -> Value,
}

/// Static rule registry - see module doc for why this isn't manifest-driven
/// yet. Seeded with the 3 examples docs/PLATFORM-SYSTEMS.md §6 documents.
///
/// Trigger audit (issue #93 follow-up, finding 23): the only rule below
/// whose `on` matched a kind the codebase actually emits was
/// `thumbnail_caption_draft`, and even that one was misspelled -
/// `routes/files.rs`/`lifeos_vcs::commit::commit_version` emit `version.created`
/// (generic, module-agnostic - every `file`/asset commit gets one), never a
/// module-namespaced `asset.version_created`. Fixed below.
///
/// `equity_curve_journal` (`trade.closed`) and `topic_quiz` (`topic.due`)
/// are left as their originally-declared triggers: neither event kind is
/// emitted anywhere in this codebase today. Entity status transitions
/// (e.g. a `trade` moving to `closed`, or spaced-repetition scheduling a
/// `topic` into `review_due`) only ever produce the generic
/// `entity.updated` event via `routes/entity.rs::update` - there is no
/// code path that synthesizes a domain-typed event from a status change,
/// and `modules/trading/module.js` / `modules/learning/module.js` only
/// *declare* `trade.closed`/`topic.due`(-adjacent `study.review`) in their
/// manifest `events` arrays without a bridge wiring them up (the same
/// deferred manifest-to-emitter gap this module's own doc comment calls
/// out for `pipelines`). Renaming these to `entity.updated` would be worse
/// than leaving them dead: that event fires for every entity in every
/// module, so the rule would misfire constantly instead of never firing.
/// Correctly wiring them needs either a real emitter added at the trading/
/// learning write path, or a richer `if` condition than this engine's
/// single-field-equality check supports (matching `module`+`type`+
/// `status` on a generic `entity.updated`) - both are out of scope here.
pub fn action_registry() -> Vec<ActionRule> {
    vec![
        ActionRule {
            id: "thumbnail_caption_draft",
            on: "version.created",
            if_attr: None,
            run_kind: "action",
            run: || json!({ "tool": "asset.thumbnail_caption_draft" }),
        },
        ActionRule {
            id: "equity_curve_journal",
            on: "trade.closed",
            if_attr: None,
            run_kind: "action",
            run: || json!({ "tool": "trading.equity_curve_and_journal" }),
        },
        ActionRule {
            id: "topic_quiz",
            on: "topic.due",
            if_attr: None,
            run_kind: "action",
            run: || json!({ "tool": "telegram.quiz" }),
        },
    ]
}

/// Whether `rule` fires for an event of `event_type` carrying `attrs`.
pub fn rule_matches(rule: &ActionRule, event_type: &str, attrs: &Value) -> bool {
    if rule.on != event_type {
        return false;
    }
    match rule.if_attr {
        None => true,
        Some((field, expected)) => attrs.get(field).and_then(Value::as_str) == Some(expected),
    }
}

// --------------------------------------------------------------- cursor

fn cursor_entity_id(workspace_id: &str) -> String {
    format!("actions_cursor_{workspace_id}")
}

/// Per-workspace incremental scan position - the last `events.id` this
/// engine has already considered (matched or not). Defaults to `""`, which
/// sorts before every real ULID.
async fn get_cursor(conn: &Connection, workspace_id: &str) -> Result<String, String> {
    let mut rows = conn
        .query(
            "SELECT attrs FROM entities WHERE id = ?1",
            params![cursor_entity_id(workspace_id)],
        )
        .await
        .map_err(|e| format!("failed to read actions cursor: {e}"))?;
    match rows.next().await.map_err(|e| format!("failed to read actions cursor: {e}"))? {
        Some(row) => {
            let attrs_str: String = row.get(0).map_err(|e| e.to_string())?;
            let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or(Value::Null);
            Ok(attrs.get("last_event_id").and_then(Value::as_str).unwrap_or("").to_string())
        }
        None => Ok(String::new()),
    }
}

async fn set_cursor(conn: &Connection, workspace_id: &str, event_id: &str, now: i64) -> Result<(), String> {
    let attrs = json!({ "last_event_id": event_id });
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, attrs, source, created_at, updated_at) \
         VALUES (?1, ?2, 'actions', 'cursor', ?3, 'lifeos-actions', ?4, ?4) \
         ON CONFLICT(id) DO UPDATE SET attrs = excluded.attrs, updated_at = excluded.updated_at",
        params![
            cursor_entity_id(workspace_id),
            workspace_id,
            serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into()),
            now
        ],
    )
    .await
    .map_err(|e| format!("failed to write actions cursor: {e}"))?;
    Ok(())
}

// --------------------------------------------------------------- run

const SCAN_BATCH_LIMIT: i64 = 200;

/// Scans new `events` for one workspace since its cursor, fires any
/// matching rules (enqueuing a real `jobs` row + an `action.fired` audit
/// event each), and advances the cursor past the whole batch regardless of
/// whether anything matched. Returns the number of jobs enqueued.
pub async fn process_workspace_events(conn: &Connection, workspace_id: &str, now: i64) -> Result<usize, String> {
    let cursor = get_cursor(conn, workspace_id).await?;
    let mut rows = conn
        .query(
            "SELECT id, type, entity_id, attrs FROM events \
             WHERE workspace_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
            params![workspace_id, cursor, SCAN_BATCH_LIMIT],
        )
        .await
        .map_err(|e| format!("failed to scan events: {e}"))?;

    let registry = action_registry();
    let mut fired = 0usize;
    let mut last_id: Option<String> = None;

    while let Some(row) = rows.next().await.map_err(|e| format!("failed to scan events: {e}"))? {
        let event_id: String = row.get(0).map_err(|e| e.to_string())?;
        let event_type: String = row.get(1).map_err(|e| e.to_string())?;
        let entity_id: Option<String> = row.get(2).map_err(|e| e.to_string())?;
        let attrs_str: String = row.get(3).map_err(|e| e.to_string())?;
        let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or(Value::Null);

        for rule in &registry {
            if !rule_matches(rule, &event_type, &attrs) {
                continue;
            }
            let job_id = new_id("job");
            let payload = json!({
                "rule_id": rule.id,
                "on": rule.on,
                "run": (rule.run)(),
                "event_entity_id": entity_id,
                "event_attrs": attrs,
            });
            conn.execute(
                "INSERT INTO jobs (id, workspace_id, kind, payload, status, attempts, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?5)",
                params![
                    job_id.clone(),
                    workspace_id,
                    rule.run_kind,
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()),
                    now
                ],
            )
            .await
            .map_err(|e| format!("failed to enqueue action job: {e}"))?;

            emit_event(
                conn,
                workspace_id,
                "action.fired",
                entity_id.as_deref().unwrap_or(""),
                &json!({ "rule_id": rule.id, "job_id": job_id }),
                now,
            )
            .await
            .map_err(|e| format!("failed to emit action.fired event: {e}"))?;

            fired += 1;
        }

        last_id = Some(event_id);
    }

    if let Some(id) = last_id {
        set_cursor(conn, workspace_id, &id, now).await?;
    }

    Ok(fired)
}

/// Mirrors `routes/event.rs`'s INSERT by hand (standalone-crate convention,
/// same as `lifeos-drain`'s `emit_event`/`lifeos-pipelines`' `emit_run_event`).
async fn emit_event(
    conn: &Connection,
    workspace_id: &str,
    event_type: &str,
    entity_id: &str,
    attrs: &Value,
    now: i64,
) -> libsql::Result<()> {
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'lifeos-actions', ?6)",
        params![
            new_id("evt"),
            workspace_id,
            now,
            event_type,
            entity_id,
            serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into())
        ],
    )
    .await?;
    Ok(())
}

/// The function `lifeos-drain`'s poll loop calls once per tick: scans every
/// workspace independently (each keeps its own cursor) and sums the jobs
/// enqueued across all of them.
pub async fn run_action_engine_tick(conn: &Connection, now: i64) -> Result<usize, String> {
    let mut rows = conn
        .query("SELECT id FROM workspaces", ())
        .await
        .map_err(|e| format!("failed to list workspaces: {e}"))?;
    let mut workspace_ids = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| format!("failed to list workspaces: {e}"))? {
        workspace_ids.push(row.get::<String>(0).map_err(|e| e.to_string())?);
    }

    let mut total = 0usize;
    for workspace_id in workspace_ids {
        total += process_workspace_events(conn, &workspace_id, now).await?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Builder;

    async fn fresh_conn(path: &str) -> Connection {
        let _ = std::fs::remove_file(path);
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE entities (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT, type TEXT, \
                parent_id TEXT, title TEXT, status TEXT, attrs TEXT NOT NULL DEFAULT '{}', \
                source TEXT, blob_ref TEXT, created_at INTEGER, updated_at INTEGER)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER, type TEXT, \
                entity_id TEXT, actor TEXT, attrs TEXT)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE jobs (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL, \
                payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued', \
                priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER, \
                attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    async fn insert_event(conn: &Connection, id: &str, workspace_id: &str, event_type: &str, attrs: &Value, ts: i64) {
        conn.execute(
            "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
             VALUES (?1, ?2, ?3, ?4, 'ent_1', 'user', ?5)",
            params![id, workspace_id, ts, event_type, serde_json::to_string(attrs).unwrap()],
        )
        .await
        .unwrap();
    }

    async fn job_count(conn: &Connection, workspace_id: &str) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM jobs WHERE workspace_id=?1", params![workspace_id])
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn fired_event_count(conn: &Connection) -> i64 {
        let mut rows = conn.query("SELECT COUNT(*) FROM events WHERE type='action.fired'", ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[test]
    fn rule_matches_on_event_type() {
        let rule = &action_registry()[1]; // trade.closed
        assert!(rule_matches(rule, "trade.closed", &json!({})));
        assert!(!rule_matches(rule, "trade.opened", &json!({})));
    }

    /// Finding 23: `thumbnail_caption_draft` was wired to `asset.version_created`,
    /// a kind nothing ever emits. `routes/files.rs`/`commit::commit_version`
    /// emit `version.created` for every file/asset commit - the rule must
    /// fire on that real kind, and must not fire on the old, never-emitted one.
    #[test]
    fn thumbnail_rule_matches_the_real_version_created_kind() {
        let rule = &action_registry()[0];
        assert_eq!(rule.id, "thumbnail_caption_draft");
        assert!(rule_matches(rule, "version.created", &json!({})));
        assert!(!rule_matches(rule, "asset.version_created", &json!({})));
    }

    #[test]
    fn rule_matches_respects_if_attr() {
        let rule = ActionRule {
            id: "x",
            on: "task.completed",
            if_attr: Some(("priority", "high")),
            run_kind: "action",
            run: || json!({}),
        };
        assert!(rule_matches(&rule, "task.completed", &json!({ "priority": "high" })));
        assert!(!rule_matches(&rule, "task.completed", &json!({ "priority": "low" })));
        assert!(!rule_matches(&rule, "task.completed", &json!({})));
    }

    #[tokio::test]
    async fn matching_event_enqueues_one_job_and_one_fired_event() {
        let conn = fresh_conn("/tmp/lifeos-actions-test-match.db").await;
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws1','w')", ()).await.unwrap();
        insert_event(&conn, "evt_0001", "ws1", "trade.closed", &json!({}), 100).await;

        let n = process_workspace_events(&conn, "ws1", 101).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(job_count(&conn, "ws1").await, 1);
        assert_eq!(fired_event_count(&conn).await, 1);

        let mut rows = conn.query("SELECT kind, payload FROM jobs", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let kind: String = row.get(0).unwrap();
        assert_eq!(kind, "action");
        let payload: String = row.get(1).unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["rule_id"], "equity_curve_journal");
        assert_eq!(payload["run"]["tool"], "trading.equity_curve_and_journal");
    }

    /// End-to-end version of `thumbnail_rule_matches_the_real_version_created_kind`:
    /// a `version.created` event (the kind `routes/files.rs` actually emits
    /// on every commit) must make it all the way through the engine to a
    /// queued `action` job for `thumbnail_caption_draft`.
    #[tokio::test]
    async fn real_version_created_event_fires_the_thumbnail_rule() {
        let conn = fresh_conn("/tmp/lifeos-actions-test-version-created.db").await;
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws1','w')", ()).await.unwrap();
        insert_event(&conn, "evt_0001", "ws1", "version.created", &json!({}), 100).await;

        let n = process_workspace_events(&conn, "ws1", 101).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(job_count(&conn, "ws1").await, 1);

        let mut rows = conn.query("SELECT payload FROM jobs", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let payload: String = row.get(0).unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["rule_id"], "thumbnail_caption_draft");
        assert_eq!(payload["run"]["tool"], "asset.thumbnail_caption_draft");
    }

    #[tokio::test]
    async fn non_matching_event_advances_cursor_without_enqueuing() {
        let conn = fresh_conn("/tmp/lifeos-actions-test-nomatch.db").await;
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws1','w')", ()).await.unwrap();
        insert_event(&conn, "evt_0001", "ws1", "task.completed", &json!({}), 100).await;

        let n = process_workspace_events(&conn, "ws1", 101).await.unwrap();
        assert_eq!(n, 0);
        assert_eq!(job_count(&conn, "ws1").await, 0);

        assert_eq!(get_cursor(&conn, "ws1").await.unwrap(), "evt_0001");
    }

    #[tokio::test]
    async fn reprocessing_after_cursor_advance_does_not_refire() {
        let conn = fresh_conn("/tmp/lifeos-actions-test-cursor.db").await;
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws1','w')", ()).await.unwrap();
        insert_event(&conn, "evt_0001", "ws1", "trade.closed", &json!({}), 100).await;

        assert_eq!(process_workspace_events(&conn, "ws1", 101).await.unwrap(), 1);
        // Second tick with no new events - must not refire the same event.
        assert_eq!(process_workspace_events(&conn, "ws1", 102).await.unwrap(), 0);
        assert_eq!(job_count(&conn, "ws1").await, 1);
    }

    #[tokio::test]
    async fn tick_sums_across_workspaces_with_independent_cursors() {
        let conn = fresh_conn("/tmp/lifeos-actions-test-multiws.db").await;
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws1','w')", ()).await.unwrap();
        conn.execute("INSERT INTO workspaces (id, name) VALUES ('ws2','w')", ()).await.unwrap();
        insert_event(&conn, "evt_a1", "ws1", "trade.closed", &json!({}), 100).await;
        insert_event(&conn, "evt_b1", "ws2", "topic.due", &json!({}), 100).await;
        insert_event(&conn, "evt_b2", "ws2", "topic.due", &json!({}), 101).await;

        let total = run_action_engine_tick(&conn, 102).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(job_count(&conn, "ws1").await, 1);
        assert_eq!(job_count(&conn, "ws2").await, 2);

        assert_eq!(run_action_engine_tick(&conn, 103).await.unwrap(), 0);
    }
}
