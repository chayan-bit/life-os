//! lifeos-drain queue core: atomic claim, reaper, and dispatch-by-kind.
//!
//! The Mac drains heavy work enqueued to `jobs`. Claims must be atomic so two
//! drainers never run the same job; crashed claims must be reaped and retried.
//! These functions are split out of `main` so the concurrency and reaper
//! guarantees can be tested directly against a libSQL connection.
//!
//! `module_requests` (issue #76, docs/SELF-EXTENSION.md §1) gets its own
//! queued->building->installed|failed transitions below, guarded by the same
//! CAS-via-WHERE-clause discipline as `complete_job`/`fail_job`. This crate
//! has no dependency on `lifeos-api` (it's a standalone binary against the
//! same DB file), so `emit_event` is a small self-contained mirror of
//! `lifeos_api::audit::emit` rather than a cross-crate import.

pub mod ai;

use async_trait::async_trait;
use libsql::{params, Connection};
use ulid::{Generator, Ulid};
use std::sync::Mutex;

static EVENT_ID_GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());

fn new_event_id() -> String {
    let ulid = EVENT_ID_GENERATOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .generate()
        .unwrap_or_else(|_| Ulid::new());
    format!("evt_{ulid}")
}

/// Append one `events` row. Mirrors `lifeos_api::audit::emit`'s shape exactly
/// (same table, same id scheme) so events this crate writes are
/// indistinguishable from ones the API writes.
async fn emit_event(
    conn: &Connection,
    workspace_id: &str,
    event_type: &str,
    entity_id: &str,
    actor: &str,
    attrs: &serde_json::Value,
    now: i64,
) -> libsql::Result<()> {
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![new_event_id(), workspace_id, now, event_type, entity_id, actor, attrs_str],
    )
    .await?;
    Ok(())
}

/// A job a drainer has exclusively claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: String,
    pub kind: String,
    pub payload: String,
    pub workspace_id: String,
}

/// Tunables, all overridable via env in `main`.
#[derive(Debug, Clone, Copy)]
pub struct DrainConfig {
    /// Seconds a `running` job may go untouched before it is reaped.
    pub stuck_ttl_secs: i64,
    /// Max claim attempts before a job is marked `failed` for good.
    pub max_attempts: i64,
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self {
            stuck_ttl_secs: 300,
            max_attempts: 3,
        }
    }
}

/// Atomically claim the highest-priority eligible job, if any.
///
/// SQLite serializes writers, so the `UPDATE ... WHERE id = (SELECT ...
/// status='queued' ...)` re-checks `status` under the write lock - two drainers
/// racing this statement can never select-and-claim the same row. We bump
/// `attempts` here so the count survives a crash (the reaper requeues without
/// re-incrementing).
pub async fn claim_job(
    conn: &Connection,
    worker_id: &str,
    now: i64,
    cfg: DrainConfig,
) -> libsql::Result<Option<ClaimedJob>> {
    let sql = "UPDATE jobs \
         SET status='running', claimed_by=?1, claimed_at=?2, attempts=attempts+1 \
         WHERE id = ( \
            SELECT id FROM jobs \
            WHERE status='queued' \
              AND (run_after IS NULL OR run_after <= ?2) \
              AND attempts < ?3 \
            ORDER BY priority DESC, created_at ASC LIMIT 1 \
         ) \
         RETURNING id, kind, payload, workspace_id";
    let mut rows = conn
        .query(sql, params![worker_id, now, cfg.max_attempts])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(ClaimedJob {
            id: row.get(0)?,
            kind: row.get(1)?,
            payload: row.get(2)?,
            workspace_id: row.get(3)?,
        })),
        None => Ok(None),
    }
}

/// Mark a claimed job done. Guarded by `claimed_by` + `status='running'` so a
/// worker can only finalize a job it still holds: if this worker stalled, the
/// reaper requeued the job, and another worker re-claimed it, this stale update
/// matches zero rows instead of clobbering the new owner's claim (double-run).
/// Returns the number of rows updated (0 = lease lost).
pub async fn complete_job(conn: &Connection, id: &str, worker_id: &str) -> libsql::Result<u64> {
    let n = conn
        .execute(
            "UPDATE jobs SET status='done' WHERE id=?1 AND claimed_by=?2 AND status='running'",
            params![id, worker_id],
        )
        .await?;
    Ok(n)
}

/// Mark a claimed job failed (no further retries). Same lease guard as
/// `complete_job`. Returns the number of rows updated (0 = lease lost).
pub async fn fail_job(conn: &Connection, id: &str, worker_id: &str) -> libsql::Result<u64> {
    let n = conn
        .execute(
            "UPDATE jobs SET status='failed' WHERE id=?1 AND claimed_by=?2 AND status='running'",
            params![id, worker_id],
        )
        .await?;
    Ok(n)
}

/// Reap jobs stuck in `running` past the TTL. Those under the attempt cap go
/// back to `queued`; those that have exhausted their retries become `failed`.
/// Returns the number of rows reaped.
pub async fn reap_stuck(conn: &Connection, now: i64, cfg: DrainConfig) -> libsql::Result<u64> {
    let threshold = now - cfg.stuck_ttl_secs;
    let n = conn
        .execute(
            "UPDATE jobs \
             SET status = CASE WHEN attempts >= ?1 THEN 'failed' ELSE 'queued' END, \
                 claimed_by = NULL, claimed_at = NULL \
             WHERE status='running' AND claimed_at IS NOT NULL AND claimed_at < ?2",
            params![cfg.max_attempts, threshold],
        )
        .await?;
    Ok(n)
}

/// Result of dispatching a claimed job to its (eventual) handler.
#[derive(Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// A known kind whose real handler lands in a later phase (no-op stub).
    Stub(&'static str),
    /// `ingest` jobs have a real handler now (issue #88, `lifeos_ingest::process_ingest_job`),
    /// called directly as a library - not a subprocess, both crates share this workspace.
    Ingest,
    /// `pipeline` jobs have a real handler now (issue #92,
    /// `lifeos_pipelines::process_pipeline_job`), same direct-library-call
    /// shape as `Ingest`.
    Pipeline,
    /// `memory_sleep` jobs (issue #115, docs/AI-MEMORY.md §5) run one
    /// consolidation cycle via `lifeos_memory::run_sleep_cycle`, same
    /// direct-library-call shape as `Ingest`/`Pipeline`.
    MemorySleep,
    /// Unknown kind - cannot be handled, will be failed.
    Unknown,
}

/// Route a job to its handler by kind. `ingest` (#88) and `pipeline` (#92)
/// are real; module_build/eval land in later phases - until then those
/// known kinds are acknowledged as no-op stubs and unknown kinds are
/// rejected.
///
/// `reconcile` (docs/DATA-MODEL.md §4.2) already has a real handler -
/// `lifeos_api::reconcile::reconcile_entity`, reachable today via
/// `POST /api/entity/:id/reconcile`. It is dispatched here as a stub too so a
/// queued `jobs` row of this kind is acknowledged rather than rejected as
/// Unknown; wiring drain to actually call the API is a later phase, same as
/// the other stub kinds.
///
/// `module_build` jobs (from `POST /api/module-request`) stay a stub here on
/// purpose: the real build path (#78) polls `module_requests` directly via
/// `claim_next_module_request`, not through `jobs` - see that function's doc
/// comment for why the two intake paths haven't converged yet.
///
/// `action` jobs (issue #93, `lifeos-actions`' Life OS Actions engine) are a
/// stub too: a declared rule firing on a real `events` row and enqueuing a
/// real, correctly-shaped `jobs` row is #93's whole acceptance bar (see
/// `lifeos_actions::process_workspace_events`'s doc comment) - what the job
/// actually *does* is deferred, same as `module_build`/`eval`/`reconcile`.
pub fn dispatch(kind: &str) -> Dispatch {
    match kind {
        "ingest" => Dispatch::Ingest,
        "pipeline" => Dispatch::Pipeline,
        "module_build" => Dispatch::Stub("scaffold.js"),
        "eval" => Dispatch::Stub("harness eval"),
        "reconcile" => Dispatch::Stub("lifeos-api reconcile"),
        "action" => Dispatch::Stub("lifeos-actions run"),
        // Storage migrations (issue #108) run inside lifeos-api (it owns the
        // Nango/secret_enc clients backends need); a row drained here (API
        // was down when it fired) is acknowledged like reconcile, and the
        // API's has-before-put resume makes re-running it from the API safe.
        "storage_migrate" => Dispatch::Stub("lifeos-api storage migration"),
        "memory_sleep" => Dispatch::MemorySleep,
        _ => Dispatch::Unknown,
    }
}

/// Consolidation trigger (issue #115: "triggered on idle + an accumulated-
/// importance threshold"): on each idle poll tick, enqueue one `memory_sleep`
/// job per workspace whose unconsolidated-event backlog crossed `threshold` -
/// unless one is already queued/running (debounce). Returns jobs enqueued.
pub async fn maybe_enqueue_memory_sleep(
    conn: &Connection,
    threshold: i64,
    now: i64,
) -> Result<u64, lifeos_memory::MemoryError> {
    let mut rows = conn.query("SELECT id FROM workspaces ORDER BY id", ()).await?;
    let mut workspaces = Vec::new();
    while let Some(row) = rows.next().await? {
        workspaces.push(row.get::<String>(0)?);
    }
    let mut enqueued = 0;
    for ws in workspaces {
        if lifeos_memory::unconsolidated_importance(conn, &ws).await? < threshold {
            continue;
        }
        let mut pending = conn
            .query(
                "SELECT 1 FROM jobs WHERE workspace_id = ?1 AND kind = 'memory_sleep' \
                 AND status IN ('queued', 'running') LIMIT 1",
                params![ws.clone()],
            )
            .await?;
        if pending.next().await?.is_some() {
            continue; // debounce: a cycle is already scheduled/running
        }
        conn.execute(
            "INSERT INTO jobs (id, workspace_id, kind, payload, status, priority, attempts, created_at) \
             VALUES (?1, ?2, 'memory_sleep', '{}', 'queued', 0, 0, ?3)",
            params![format!("job_{}", Ulid::new()), ws, now],
        )
        .await?;
        enqueued += 1;
    }
    Ok(enqueued)
}

// ----------------------------------------------------- module_requests (#76)
//
// A `module_build` job's payload carries `request_id` - the linked
// `module_requests` row a requester polls via `GET /api/module-request/:id`.
// These three functions are the queued->building->installed|failed state
// machine, each guarded by the current status exactly like `complete_job`/
// `fail_job`'s lease guard (a mismatched WHERE = 0 rows = someone else
// already moved this request, don't clobber it) and each emitting the
// matching `module.*` event only when the transition actually applied.
//
// Deliberately NOT called from `run_job`/`dispatch` yet: `module_build` is
// still a `Dispatch::Stub` (no real `scaffold.js` invocation - that's #78's
// job), and marking a request `installed` for a build that never actually
// ran would be exactly the kind of false-confidence result this project's
// validators (#74/#75) were built to avoid. #78's real drain loop calls
// these in lockstep with `claim_job`/`complete_job`/`fail_job` once it
// actually invokes `scaffoldModule()`.

/// `queued` -> `building`. Call right after `claim_job` claims the linked
/// `module_build` job. Returns rows affected (0 = already transitioned).
pub async fn claim_module_request(
    conn: &Connection,
    request_id: &str,
    workspace_id: &str,
    now: i64,
) -> libsql::Result<u64> {
    let n = conn
        .execute(
            "UPDATE module_requests SET status='building', updated_at=?2 WHERE id=?1 AND status='queued'",
            params![request_id, now],
        )
        .await?;
    if n > 0 {
        emit_event(conn, workspace_id, "module.building", request_id, "mac-drain", &serde_json::json!({}), now).await?;
    }
    Ok(n)
}

/// `building` -> `installed`. Call once the real build (§1 step 5) lands the
/// module. Returns rows affected (0 = lease lost / already transitioned).
pub async fn complete_module_request(
    conn: &Connection,
    request_id: &str,
    workspace_id: &str,
    module_id: &str,
    now: i64,
) -> libsql::Result<u64> {
    let n = conn
        .execute(
            "UPDATE module_requests SET status='installed', updated_at=?2 WHERE id=?1 AND status='building'",
            params![request_id, now],
        )
        .await?;
    if n > 0 {
        emit_event(
            conn,
            workspace_id,
            "module.installed",
            request_id,
            "mac-drain",
            &serde_json::json!({ "id": module_id }),
            now,
        )
        .await?;
    }
    Ok(n)
}

/// `building` -> `failed`, with the honest error message a requester's
/// `GET /api/module-request/:id` surfaces directly (issue #76's acceptance:
/// "failure surfaces honestly to the requester", not a generic "something
/// went wrong"). Returns rows affected (0 = lease lost / already transitioned).
pub async fn fail_module_request(
    conn: &Connection,
    request_id: &str,
    workspace_id: &str,
    error: &str,
    now: i64,
) -> libsql::Result<u64> {
    let n = conn
        .execute(
            "UPDATE module_requests SET status='failed', error=?2, updated_at=?3 WHERE id=?1 AND status='building'",
            params![request_id, error, now],
        )
        .await?;
    if n > 0 {
        emit_event(
            conn,
            workspace_id,
            "module.failed",
            request_id,
            "mac-drain",
            &serde_json::json!({ "error": error }),
            now,
        )
        .await?;
    }
    Ok(n)
}

// ------------------------------------------------------- offline build (#78)
//
// `POST /api/module-request` (the API path) links `module_requests` to a
// `jobs` row of kind `module_build`, but the Telegram bot's `/addmodule`
// (the offline, phone-initiated path this issue is about,
// `worker/src/moduleRequests.ts::enqueueModuleRequest`) inserts only the
// `module_requests` row - no `jobs` row exists for the drain's `claim_job`
// to ever see. So the drain claims directly off `module_requests` here,
// independent of `jobs` entirely. `claim_module_request` (above, by-id) is
// left as-is for a future `jobs`-driven caller; it is not used by this path.

/// A `module_requests` row this drainer has exclusively claimed (transitioned
/// to `building`), including the requester's Telegram chat to notify back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRequestRow {
    pub id: String,
    pub workspace_id: String,
    pub prompt: String,
    pub chat_id: Option<String>,
}

/// Atomically claim the oldest queued `module_requests` row, if any, and
/// transition it straight to `building`. Same `UPDATE ... WHERE id = (SELECT
/// ...)` shape as `claim_job` so two drainers can never claim the same row.
pub async fn claim_next_module_request(
    conn: &Connection,
    now: i64,
) -> libsql::Result<Option<ModuleRequestRow>> {
    let sql = "UPDATE module_requests \
         SET status='building', updated_at=?1 \
         WHERE id = ( \
            SELECT id FROM module_requests \
            WHERE status='queued' ORDER BY created_at ASC LIMIT 1 \
         ) \
         RETURNING id, workspace_id, prompt, chat_id";
    let mut rows = conn.query(sql, params![now]).await?;
    let claimed = match rows.next().await? {
        Some(row) => Some(ModuleRequestRow {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            prompt: row.get(2)?,
            chat_id: row.get(3)?,
        }),
        None => None,
    };
    if let Some(req) = &claimed {
        emit_event(conn, &req.workspace_id, "module.building", &req.id, "mac-drain", &serde_json::json!({}), now).await?;
    }
    Ok(claimed)
}

/// Runs the real `scaffold.js` build for a claimed module request. Injected
/// so `run_module_build` is fully unit-testable without spawning a real
/// process or invoking the Agent SDK - the same DI discipline
/// `docs/SELF-EXTENSION.md`'s `scaffoldModule(prompt, workspaceId, opts)`
/// already uses on the Node side, and the same trait+mock shape
/// `lifeos-api`'s `NangoClient`/`WhatsAppClient` use for every external call.
#[async_trait]
pub trait ModuleBuilder: Send + Sync {
    /// `Ok(module_id)` on a real, committed install; `Err(message)` otherwise.
    async fn build(&self, prompt: &str, workspace_id: &str) -> Result<String, String>;
}

/// Spawns the Node build entry (`build/run.js` by default, the multi-tier
/// spec->plan->DAG->validate->gate->commit pipeline of issue #132; or the plain
/// single-manifest `scaffold.js` of issue #78 when `build_pipeline` is off) as
/// `node <script> <prompt> <workspaceId>` and parses its last stdout line as
/// the JSON the entry already returns. Both entries share the same last-line
/// JSON contract, so the parse below handles either shape.
pub struct ScaffoldJsBuilder {
    pub server_dir: String,
    /// `LIFEOS_BUILD_PIPELINE` (default on): route claimed module requests
    /// through the full pipeline; set to `0` to fall back to plain scaffold.js.
    pub build_pipeline: bool,
}

/// The Node entry script for the chosen build path - the one place the pipeline
/// vs. plain-scaffold switch resolves, kept pure so it is directly unit-tested.
pub fn entry_script(build_pipeline: bool) -> &'static str {
    if build_pipeline {
        "build/run.js"
    } else {
        "scaffold.js"
    }
}

/// Extracts the installed-identifier / error from a build entry's last stdout
/// JSON line, tolerant of both shapes: scaffold.js returns `{success, moduleId,
/// error}`; build/run.js returns `{success, runId, nodes, summary, error?}`. On
/// success prefer `moduleId`, else the `runId`; on failure prefer `error`, else
/// the honest `summary`.
fn parse_build_result(last_line: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(last_line)
        .map_err(|e| format!("build output was not valid JSON: {e} (line: {last_line})"))?;

    let field = |key: &str| parsed.get(key).and_then(|v| v.as_str()).map(String::from);

    if parsed.get("success").and_then(|v| v.as_bool()) == Some(true) {
        field("moduleId")
            .or_else(|| field("runId"))
            .ok_or_else(|| "build reported success but no moduleId/runId".to_string())
    } else {
        Err(field("error")
            .or_else(|| field("summary"))
            .unwrap_or_else(|| "build reported failure with no error message".to_string()))
    }
}

#[async_trait]
impl ModuleBuilder for ScaffoldJsBuilder {
    async fn build(&self, prompt: &str, workspace_id: &str) -> Result<String, String> {
        let script = entry_script(self.build_pipeline);
        let output = tokio::process::Command::new("node")
            .arg(script)
            .arg(prompt)
            .arg(workspace_id)
            .current_dir(&self.server_dir)
            .output()
            .await
            .map_err(|e| format!("failed to spawn node {script}: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty());
        let Some(last_line) = last_line else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{script} produced no output (stderr: {stderr})"));
        };

        parse_build_result(last_line)
    }
}

/// Notifies the original requester. Fire-and-forget from the caller's
/// perspective - a failed notification must never block or reverse the
/// build's own DB state transition, so implementations swallow their own
/// errors (logging, not propagating).
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, chat_id: &str, text: &str);
}

/// Real implementation: a direct Telegram Bot API call from the Mac, no
/// Cloudflare Worker round-trip - the drain is offline-first by design.
pub struct TelegramNotifier {
    pub token: String,
    http: reqwest::Client,
}

impl TelegramNotifier {
    pub fn new(token: String) -> Self {
        Self { token, http: reqwest::Client::new() }
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, chat_id: &str, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let result = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
            .send()
            .await;
        match result {
            Ok(resp) if !resp.status().is_success() => {
                eprintln!("lifeos-drain: telegram notify to {chat_id} returned {}", resp.status());
            }
            Err(e) => eprintln!("lifeos-drain: telegram notify to {chat_id} failed: {e}"),
            Ok(_) => {}
        }
    }
}

/// Notifier used when `TELEGRAM_BOT_TOKEN` isn't configured - the build still
/// completes/fails correctly, just without a phone ping.
pub struct NoopNotifier;

#[async_trait]
impl Notifier for NoopNotifier {
    async fn notify(&self, _chat_id: &str, _text: &str) {}
}

/// Delivers a pipeline eval-gate rationale (issue #96,
/// docs/HARNESS-LOOP.md §2) - a single-user "admin" ping, unlike
/// `run_module_build`'s per-requester notify, since a pipeline run has no
/// associated `chat_id`. Kept as a small directly-testable function (same
/// reasoning as `run_module_build`) rather than inline in `main.rs`.
pub async fn notify_pipeline_gated(notifier: &dyn Notifier, chat_id: &str, stage: &str, rationale: &str) {
    let text = format!("\u{26d4} pipeline gated at stage '{stage}': {rationale}");
    notifier.notify(chat_id, &text).await;
}

/// Runs a claimed module request's build to completion: calls `builder`,
/// applies the matching `module_requests` transition, and notifies the
/// requester's chat (if any). This is the orchestration `main.rs`'s loop
/// calls per claimed request, kept in `lib.rs` so it's testable with
/// `ModuleBuilder`/`Notifier` mocks instead of a real subprocess/HTTP call.
pub async fn run_module_build(
    conn: &Connection,
    builder: &dyn ModuleBuilder,
    notifier: &dyn Notifier,
    req: ModuleRequestRow,
    now: i64,
) {
    match builder.build(&req.prompt, &req.workspace_id).await {
        Ok(module_id) => {
            if let Err(e) = complete_module_request(conn, &req.id, &req.workspace_id, &module_id, now).await {
                eprintln!("lifeos-drain: complete_module_request for {} failed: {e}", req.id);
            }
            if let Some(chat_id) = &req.chat_id {
                notifier.notify(chat_id, &format!("\u{2705} live: modules/{module_id}")).await;
            }
        }
        Err(error) => {
            if let Err(e) = fail_module_request(conn, &req.id, &req.workspace_id, &error, now).await {
                eprintln!("lifeos-drain: fail_module_request for {} failed: {e}", req.id);
            }
            if let Some(chat_id) = &req.chat_id {
                notifier.notify(chat_id, &format!("\u{274C} build failed: {error}")).await;
            }
        }
    }
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
            "CREATE TABLE module_requests (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, prompt TEXT NOT NULL, \
                status TEXT NOT NULL DEFAULT 'queued', error TEXT, chat_id TEXT, \
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER NOT NULL, \
                type TEXT NOT NULL, entity_id TEXT, actor TEXT NOT NULL, attrs TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    async fn insert_queued(conn: &Connection, id: &str, workspace_id: &str, now: i64) {
        conn.execute(
            "INSERT INTO module_requests (id, workspace_id, prompt, status, error, chat_id, created_at, updated_at) \
             VALUES (?1, ?2, 'add a widget module', 'queued', NULL, NULL, ?3, ?3)",
            params![id, workspace_id, now],
        )
        .await
        .unwrap();
    }

    async fn insert_queued_with_chat(conn: &Connection, id: &str, workspace_id: &str, chat_id: &str, now: i64) {
        conn.execute(
            "INSERT INTO module_requests (id, workspace_id, prompt, status, error, chat_id, created_at, updated_at) \
             VALUES (?1, ?2, 'add a widget module', 'queued', NULL, ?3, ?4, ?4)",
            params![id, workspace_id, chat_id, now],
        )
        .await
        .unwrap();
    }

    async fn status_of(conn: &Connection, id: &str) -> String {
        let mut rows = conn
            .query("SELECT status FROM module_requests WHERE id=?1", params![id])
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn event_count(conn: &Connection, event_type: &str) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM events WHERE type=?1", params![event_type])
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn walks_queued_building_installed_with_an_event_at_each_step() {
        let conn = fresh_conn("test_mr_happy.db").await;
        insert_queued(&conn, "req_1", "ws1", 100).await;

        assert_eq!(claim_module_request(&conn, "req_1", "ws1", 101).await.unwrap(), 1);
        assert_eq!(status_of(&conn, "req_1").await, "building");
        assert_eq!(event_count(&conn, "module.building").await, 1);

        assert_eq!(
            complete_module_request(&conn, "req_1", "ws1", "widgets", 102).await.unwrap(),
            1
        );
        assert_eq!(status_of(&conn, "req_1").await, "installed");
        assert_eq!(event_count(&conn, "module.installed").await, 1);

        let _ = std::fs::remove_file("test_mr_happy.db");
    }

    #[tokio::test]
    async fn walks_queued_building_failed_with_the_error_surfaced() {
        let conn = fresh_conn("test_mr_failed.db").await;
        insert_queued(&conn, "req_2", "ws1", 100).await;

        claim_module_request(&conn, "req_2", "ws1", 101).await.unwrap();
        assert_eq!(
            fail_module_request(&conn, "req_2", "ws1", "PreToolUse hook denied", 103)
                .await
                .unwrap(),
            1
        );

        assert_eq!(status_of(&conn, "req_2").await, "failed");
        let mut rows = conn
            .query("SELECT error FROM module_requests WHERE id='req_2'", ())
            .await
            .unwrap();
        let error: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(error, "PreToolUse hook denied");
        assert_eq!(event_count(&conn, "module.failed").await, 1);

        let _ = std::fs::remove_file("test_mr_failed.db");
    }

    #[tokio::test]
    async fn claim_is_a_noop_on_a_request_that_is_not_queued() {
        let conn = fresh_conn("test_mr_claim_noop.db").await;
        insert_queued(&conn, "req_3", "ws1", 100).await;
        claim_module_request(&conn, "req_3", "ws1", 101).await.unwrap();

        // Second claim attempt on an already-building request is a no-op,
        // not a re-transition or a duplicate event - same discipline as a
        // job whose lease was already taken.
        assert_eq!(claim_module_request(&conn, "req_3", "ws1", 102).await.unwrap(), 0);
        assert_eq!(event_count(&conn, "module.building").await, 1);

        let _ = std::fs::remove_file("test_mr_claim_noop.db");
    }

    #[tokio::test]
    async fn complete_and_fail_are_noops_outside_the_building_state() {
        let conn = fresh_conn("test_mr_wrong_state.db").await;
        insert_queued(&conn, "req_4", "ws1", 100).await;

        // Still 'queued' - neither transition should apply, and neither
        // should emit an event for a state change that didn't happen.
        assert_eq!(
            complete_module_request(&conn, "req_4", "ws1", "widgets", 101).await.unwrap(),
            0
        );
        assert_eq!(fail_module_request(&conn, "req_4", "ws1", "boom", 101).await.unwrap(), 0);
        assert_eq!(status_of(&conn, "req_4").await, "queued");
        assert_eq!(event_count(&conn, "module.installed").await, 0);
        assert_eq!(event_count(&conn, "module.failed").await, 0);

        let _ = std::fs::remove_file("test_mr_wrong_state.db");
    }

    // ------------------------------------------------------------- #78

    #[tokio::test]
    async fn claim_next_module_request_claims_the_oldest_queued_row_atomically() {
        let conn = fresh_conn("test_mr_claim_next.db").await;
        insert_queued(&conn, "req_older", "ws1", 100).await;
        insert_queued_with_chat(&conn, "req_newer", "ws1", "chat_42", 200).await;

        let claimed = claim_next_module_request(&conn, 300).await.unwrap().unwrap();
        assert_eq!(claimed.id, "req_older");
        assert_eq!(claimed.chat_id, None);
        assert_eq!(status_of(&conn, "req_older").await, "building");
        assert_eq!(status_of(&conn, "req_newer").await, "queued");
        assert_eq!(event_count(&conn, "module.building").await, 1);

        let claimed2 = claim_next_module_request(&conn, 301).await.unwrap().unwrap();
        assert_eq!(claimed2.id, "req_newer");
        assert_eq!(claimed2.chat_id, Some("chat_42".to_string()));

        assert!(claim_next_module_request(&conn, 302).await.unwrap().is_none());

        let _ = std::fs::remove_file("test_mr_claim_next.db");
    }

    struct MockModuleBuilder {
        result: Result<String, String>,
    }

    #[async_trait]
    impl ModuleBuilder for MockModuleBuilder {
        async fn build(&self, _prompt: &str, _workspace_id: &str) -> Result<String, String> {
            self.result.clone()
        }
    }

    #[derive(Default)]
    struct MockNotifier {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Notifier for MockNotifier {
        async fn notify(&self, chat_id: &str, text: &str) {
            self.calls.lock().unwrap().push((chat_id.to_string(), text.to_string()));
        }
    }

    #[tokio::test]
    async fn notify_pipeline_gated_sends_the_rationale_to_the_admin_chat() {
        let notifier = MockNotifier::default();
        notify_pipeline_gated(&notifier, "admin_chat", "verify", "reads like a placeholder").await;
        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "admin_chat");
        assert_eq!(calls[0].1, "\u{26d4} pipeline gated at stage 'verify': reads like a placeholder");
    }

    #[tokio::test]
    async fn run_module_build_installs_and_notifies_on_success() {
        let conn = fresh_conn("test_run_build_ok.db").await;
        insert_queued_with_chat(&conn, "req_ok", "ws1", "chat_1", 100).await;
        let req = claim_next_module_request(&conn, 101).await.unwrap().unwrap();

        let builder = MockModuleBuilder { result: Ok("widgets".to_string()) };
        let notifier = MockNotifier::default();

        run_module_build(&conn, &builder, &notifier, req, 102).await;

        assert_eq!(status_of(&conn, "req_ok").await, "installed");
        assert_eq!(event_count(&conn, "module.installed").await, 1);
        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "chat_1");
        assert!(calls[0].1.contains("widgets"));

        let _ = std::fs::remove_file("test_run_build_ok.db");
    }

    #[tokio::test]
    async fn run_module_build_fails_and_notifies_on_error() {
        let conn = fresh_conn("test_run_build_fail.db").await;
        insert_queued_with_chat(&conn, "req_fail", "ws1", "chat_2", 100).await;
        let req = claim_next_module_request(&conn, 101).await.unwrap().unwrap();

        let builder = MockModuleBuilder { result: Err("PreToolUse hook denied".to_string()) };
        let notifier = MockNotifier::default();

        run_module_build(&conn, &builder, &notifier, req, 102).await;

        assert_eq!(status_of(&conn, "req_fail").await, "failed");
        assert_eq!(event_count(&conn, "module.failed").await, 1);
        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "chat_2");
        assert!(calls[0].1.contains("PreToolUse hook denied"));

        let _ = std::fs::remove_file("test_run_build_fail.db");
    }

    #[tokio::test]
    async fn run_module_build_skips_notify_when_no_chat_id() {
        let conn = fresh_conn("test_run_build_no_chat.db").await;
        insert_queued(&conn, "req_no_chat", "ws1", 100).await;
        let req = claim_next_module_request(&conn, 101).await.unwrap().unwrap();

        let builder = MockModuleBuilder { result: Ok("widgets".to_string()) };
        let notifier = MockNotifier::default();

        run_module_build(&conn, &builder, &notifier, req, 102).await;

        assert_eq!(status_of(&conn, "req_no_chat").await, "installed");
        assert_eq!(notifier.calls.lock().unwrap().len(), 0);

        let _ = std::fs::remove_file("test_run_build_no_chat.db");
    }

    #[test]
    fn entry_script_switches_between_pipeline_and_plain_scaffold() {
        assert_eq!(entry_script(true), "build/run.js");
        assert_eq!(entry_script(false), "scaffold.js");
    }

    #[test]
    fn parse_build_result_reads_the_pipeline_and_scaffold_shapes() {
        // scaffold.js success shape.
        assert_eq!(
            parse_build_result(r#"{"success":true,"moduleId":"widgets"}"#).unwrap(),
            "widgets"
        );
        // build/run.js success shape (no moduleId) falls back to runId.
        assert_eq!(
            parse_build_result(r#"{"success":true,"runId":"build_1","nodes":[],"summary":"1/1"}"#).unwrap(),
            "build_1"
        );
        // Explicit error is surfaced verbatim.
        assert_eq!(
            parse_build_result(r#"{"success":false,"error":"plan rejected: cycle"}"#).unwrap_err(),
            "plan rejected: cycle"
        );
        // Partial pipeline failure with no `error` falls back to the honest summary.
        assert_eq!(
            parse_build_result(r#"{"success":false,"runId":"b","nodes":[],"summary":"1/2 nodes committed"}"#).unwrap_err(),
            "1/2 nodes committed"
        );
        // Non-JSON fails closed.
        assert!(parse_build_result("not json").is_err());
    }

    #[test]
    fn dispatch_routes_ingest_to_its_real_handler() {
        assert_eq!(dispatch("ingest"), Dispatch::Ingest);
        assert_eq!(dispatch("pipeline"), Dispatch::Pipeline);
        assert_eq!(dispatch("action"), Dispatch::Stub("lifeos-actions run"));
        assert_eq!(dispatch("storage_migrate"), Dispatch::Stub("lifeos-api storage migration"));
        assert_eq!(dispatch("memory_sleep"), Dispatch::MemorySleep);
        assert_eq!(dispatch("nonsense"), Dispatch::Unknown);
    }

    #[tokio::test]
    async fn memory_sleep_enqueues_on_threshold_and_debounces() {
        let path = "test_memory_sleep_enqueue.db";
        let conn = fresh_conn(path).await;
        conn.execute_batch(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT, created_at INTEGER, updated_at INTEGER);
             CREATE TABLE jobs (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL,
                payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued',
                priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER,
                attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL);
             CREATE TABLE memory_cursors (workspace_id TEXT PRIMARY KEY,
                projected_ts INTEGER NOT NULL DEFAULT 0, projected_id TEXT NOT NULL DEFAULT '',
                consolidated_ts INTEGER NOT NULL DEFAULT 0, consolidated_id TEXT NOT NULL DEFAULT '',
                updated_at INTEGER NOT NULL DEFAULT 0);
             INSERT INTO workspaces VALUES ('ws_default', 'p', 1, 1);",
        )
        .await
        .unwrap();

        // Two events: below the threshold of 3 - nothing enqueued.
        for i in 0..2 {
            emit_event(&conn, "ws_default", "note.captured", "", "user", &serde_json::json!({}), 100 + i)
                .await
                .unwrap();
        }
        assert_eq!(maybe_enqueue_memory_sleep(&conn, 3, 1000).await.unwrap(), 0);

        // Third event crosses the threshold - one job, then debounced.
        emit_event(&conn, "ws_default", "note.captured", "", "user", &serde_json::json!({}), 102)
            .await
            .unwrap();
        assert_eq!(maybe_enqueue_memory_sleep(&conn, 3, 1000).await.unwrap(), 1);
        assert_eq!(maybe_enqueue_memory_sleep(&conn, 3, 1001).await.unwrap(), 0, "debounced");

        let mut rows = conn
            .query("SELECT COUNT(*) FROM jobs WHERE kind = 'memory_sleep' AND status = 'queued'", ())
            .await
            .unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(n, 1);

        let _ = std::fs::remove_file(path);
    }
}
