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
/// Web push fan-out (issue #151) - see `push`'s module doc for the full
/// design (event kinds notified on, cursor shape, VAPID sender).
pub mod push;

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
    /// `voice_turn` jobs (issue #143): a Telegram voice note the Worker
    /// enqueued (base64 audio + chat) - transcribe, run an agent turn on the
    /// transcript, reply. Handled by `run_voice_turn`.
    VoiceTurn,
    /// `daily_brief` jobs (issue #144): a scheduled, read-only (dry-run) agent
    /// turn digested to Telegram. Handled by `run_daily_brief`.
    DailyBrief,
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
        // `execute_approval` (issue #142): an approved gate. Acknowledged here
        // like storage_migrate - the real work (a draft's outward effect, or a
        // build gate's pipeline resume via `run_approval_resume` /
        // `ScaffoldJsResumer`) is driven by the resume path below, not this
        // `jobs`-dispatch arm, so a bare drain of the row never fails it.
        "execute_approval" => Dispatch::Stub("lifeos-drain approval resume"),
        "memory_sleep" => Dispatch::MemorySleep,
        "voice_turn" => Dispatch::VoiceTurn,
        "daily_brief" => Dispatch::DailyBrief,
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

// ------------------------------------------------ resume-on-approval (#142)
//
// A T3+ build node halts at a `pipelines/pending_approval` gate (server/build/
// gate.js) with its worktree discarded. When a human approves that gate, an
// `execute_approval` job is enqueued (worker/src/approvals.ts or the API's
// /api/approval/:id/approve). Resuming it means re-entering the pipeline from
// the halted node - a BUILD - so it must ONLY run on the trusted Mac with the
// full pipeline enabled, exactly like a fresh module build (ARCHITECTURE.md's
// "codegen runs only on the trusted Mac"). The wiring from `run_job` to this
// path is main.rs's (owned separately); these functions are the tested,
// DI-shaped core it calls, mirroring `run_module_build`/`ScaffoldJsBuilder`.

/// Payload of an `execute_approval` job: the approved entity + its type.
/// `Default` (empty strings) is the safe parse-failure fallback: an unknown
/// `entity_type` is never a build gate, so `should_resume_build` returns false
/// and the job is acknowledged rather than mis-resumed.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ExecuteApprovalPayload {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub entity_type: String,
}

/// True iff an approved entity should re-enter the build pipeline. Only
/// `pipelines/pending_approval` gate entities (type `pending_approval`) carry a
/// build to resume - a draft (`draft`) or storage backend (`storage_backend`)
/// approval has no build. And an untrusted `lifeos-node` container
/// (LIFEOS_BUILD_PIPELINE off) must NEVER resume a build, so the flag gates it.
pub fn should_resume_build(entity_type: &str, build_pipeline: bool) -> bool {
    build_pipeline && entity_type == "pending_approval"
}

/// Re-enters the build pipeline for an approved gate. Injected so the resume
/// orchestration is unit-testable without spawning a real `node` process - the
/// same DI seam `ModuleBuilder` uses for fresh builds.
#[async_trait]
pub trait BuildResumer: Send + Sync {
    /// `Ok(runId)` when the resumed pipeline completed its remaining nodes;
    /// `Err(message)` on any failure (honest, surfaced verbatim).
    async fn resume(&self, approval_entity_id: &str, workspace_id: &str) -> Result<String, String>;
}

/// Shells `node build/run.js --resume <approvalEntityId> <workspaceId>` - the
/// SAME process + last-line-JSON contract `ScaffoldJsBuilder` uses, just the
/// resume entry. `parse_build_result` reads either build shape.
pub struct ScaffoldJsResumer {
    pub server_dir: String,
}

#[async_trait]
impl BuildResumer for ScaffoldJsResumer {
    async fn resume(&self, approval_entity_id: &str, workspace_id: &str) -> Result<String, String> {
        let output = tokio::process::Command::new("node")
            .arg("build/run.js")
            .arg("--resume")
            .arg(approval_entity_id)
            .arg(workspace_id)
            .current_dir(&self.server_dir)
            .output()
            .await
            .map_err(|e| format!("failed to spawn node build/run.js --resume: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty());
        let Some(last_line) = last_line else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("build/run.js --resume produced no output (stderr: {stderr})"));
        };
        parse_build_result(last_line)
    }
}

/// Resumes a build iff the approved entity is a build gate AND the pipeline is
/// enabled. Returns `None` when there is nothing to resume (a draft/storage
/// approval, or the flag is off), `Some(result)` when a resume was attempted.
pub async fn run_approval_resume(
    resumer: &dyn BuildResumer,
    payload: &ExecuteApprovalPayload,
    workspace_id: &str,
    build_pipeline: bool,
) -> Option<Result<String, String>> {
    if !should_resume_build(&payload.entity_type, build_pipeline) {
        return None;
    }
    Some(resumer.resume(&payload.entity_id, workspace_id).await)
}

/// The `execute_approval` job dispatch entry `main.rs`'s `run_job` calls: parse
/// the claimed job's raw JSON payload, then delegate to `run_approval_resume`.
/// A malformed payload parses to `ExecuteApprovalPayload::default()`, whose
/// empty `entity_type` is never a build gate, so it yields `None` (acknowledge)
/// rather than a spurious resume. Kept here (not inline in `main.rs`) so the
/// job -> resume wiring is unit-testable without spawning a `node` process.
pub async fn run_approval_resume_from_payload(
    resumer: &dyn BuildResumer,
    payload_json: &str,
    workspace_id: &str,
    build_pipeline: bool,
) -> Option<Result<String, String>> {
    let payload: ExecuteApprovalPayload = serde_json::from_str(payload_json).unwrap_or_default();
    run_approval_resume(resumer, &payload, workspace_id, build_pipeline).await
}

/// What to do with an approved entity that is NOT a build gate to resume
/// (i.e. `run_approval_resume` returned `None`). Split out as a pure decision
/// so the "never silently complete an outward send" rule (audit finding 10)
/// is directly unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonBuildApproval {
    /// Acknowledge (complete the job) - the approval has no outward effect
    /// this drain owns. Two cases: a `storage_backend` switch (lifeos-api
    /// runs the real migration via a separate `storage_migrate` path), and a
    /// build gate (`pending_approval`) seen on an untrusted node whose
    /// `LIFEOS_BUILD_PIPELINE` is off - the trusted Mac resumes that, so
    /// acknowledging here is the unchanged, correct behavior.
    Acknowledge,
    /// An approved outward-send draft (email / calendar / whatsapp / slack /
    /// drive / social / ...). The real outward executor is a separate,
    /// not-yet-built issue, so the job must FAIL loudly and the approver be
    /// told - never report `done` for a send that never happened.
    OutwardUnimplemented,
}

/// Classify a non-build approval by its entity type. Denylist, not allowlist:
/// only the types we know carry no outward send here are acknowledged; every
/// other type (all the open-ended `{provider}_{action}` draft types) is
/// treated as an unimplemented outward send and failed, so a newly-added
/// provider can never silently regress to "reported done, nothing sent".
pub fn classify_non_build_approval(entity_type: &str) -> NonBuildApproval {
    match entity_type {
        // lifeos-api owns the real storage migration (issue #108).
        "storage_backend" => NonBuildApproval::Acknowledge,
        // A build gate that only reached here because this node can't resume
        // builds (untrusted). The trusted Mac resumes it.
        "pending_approval" => NonBuildApproval::Acknowledge,
        // A malformed/empty payload is not an outward send - acknowledge
        // rather than fabricate a "not sent" alarm.
        "" => NonBuildApproval::Acknowledge,
        _ => NonBuildApproval::OutwardUnimplemented,
    }
}

/// The approver-facing message for an approved-but-unexecuted outward send.
pub fn outward_unimplemented_message(entity_type: &str) -> String {
    format!("Approved {entity_type} but outward execution is not yet implemented - not sent.")
}

/// The approved entity's type from an `execute_approval` job payload; empty
/// string for a malformed payload (never a build gate or a known outward
/// send, so it is acknowledged).
fn execute_approval_entity_type(payload_json: &str) -> String {
    serde_json::from_str::<ExecuteApprovalPayload>(payload_json)
        .unwrap_or_default()
        .entity_type
}

/// Handles a claimed `execute_approval` job (issue #142 + audit finding 10):
/// resume the build pipeline for an approved gate; acknowledge a
/// storage-backend / untrusted-node / malformed approval; and for an approved
/// outward-send draft with no executor yet, FAIL the job and notify the
/// approver instead of silently reporting `done`. Kept here (DI over trait
/// objects) so it is unit-testable without a `node` process or a live
/// Telegram call. Returns the rows the status write touched (0 = lease lost).
pub async fn run_execute_approval(
    conn: &Connection,
    resumer: &dyn BuildResumer,
    notifier: &dyn Notifier,
    admin_chat_id: Option<&str>,
    job: &ClaimedJob,
    worker_id: &str,
    build_pipeline: bool,
) -> libsql::Result<u64> {
    match run_approval_resume_from_payload(resumer, &job.payload, &job.workspace_id, build_pipeline).await {
        Some(Ok(run_id)) => {
            println!("lifeos-drain: {} execute_approval resumed build -> {run_id}", job.id);
            complete_job(conn, &job.id, worker_id).await
        }
        Some(Err(e)) => {
            eprintln!("lifeos-drain: {} execute_approval resume failed: {e} - failing", job.id);
            fail_job(conn, &job.id, worker_id).await
        }
        None => {
            let entity_type = execute_approval_entity_type(&job.payload);
            match classify_non_build_approval(&entity_type) {
                NonBuildApproval::Acknowledge => {
                    println!(
                        "lifeos-drain: {} execute_approval acknowledged ('{entity_type}', no build to resume)",
                        job.id
                    );
                    complete_job(conn, &job.id, worker_id).await
                }
                NonBuildApproval::OutwardUnimplemented => {
                    let msg = outward_unimplemented_message(&entity_type);
                    eprintln!("lifeos-drain: {} execute_approval {msg} - failing", job.id);
                    match admin_chat_id {
                        Some(chat) => notifier.notify(chat, &msg).await,
                        None => println!("lifeos-drain: {} (no admin chat configured) {msg}", job.id),
                    }
                    fail_job(conn, &job.id, worker_id).await
                }
            }
        }
    }
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

// ------------------------------------------------------- agent turns (shared)
//
// Both the voice-note reply (#143) and the daily brief (#144) need to run ONE
// agent turn on the Mac. The canonical entry is `node agent/run.js <prompt>
// <workspaceId> [--dry-run]` (server/agent/run.js) - the exact process +
// last-line-JSON contract `lifeos-api`'s /api/agent route and
// `ScaffoldJsBuilder` already use. `AgentTurnRunner` is the DI seam so both
// orchestrations are unit-testable without spawning `node`.

/// Runs one agent turn and returns the turn's reply text. `dry_run` maps to
/// `runAgentTurn`'s `opts.dryRun` (issue #140): a side-effect-free read-only
/// turn (used by the daily brief).
#[async_trait]
pub trait AgentTurnRunner: Send + Sync {
    async fn run(&self, prompt: &str, workspace_id: &str, dry_run: bool) -> Result<String, String>;
}

/// Extracts the reply text from an agent turn's last stdout JSON line
/// (`runAgentTurn`'s return value: `{success, outcome, text, error?}`). On
/// success returns `text` (possibly empty); on failure returns the honest
/// `error`/`outcome`.
fn parse_agent_turn_result(last_line: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(last_line)
        .map_err(|e| format!("agent turn output was not valid JSON: {e} (line: {last_line})"))?;
    let field = |key: &str| parsed.get(key).and_then(|v| v.as_str()).map(String::from);
    if parsed.get("success").and_then(|v| v.as_bool()) == Some(true) {
        Ok(field("text").unwrap_or_default())
    } else {
        Err(field("error")
            .or_else(|| field("outcome"))
            .unwrap_or_else(|| "agent turn reported failure with no error message".to_string()))
    }
}

/// Shells `node agent/run.js <prompt> <workspaceId> [--dry-run]` - the SAME
/// process + last-line-JSON contract `ScaffoldJsBuilder`/`ScaffoldJsResumer`
/// use, just the agent-turn entry.
pub struct NodeAgentRunner {
    pub server_dir: String,
}

#[async_trait]
impl AgentTurnRunner for NodeAgentRunner {
    async fn run(&self, prompt: &str, workspace_id: &str, dry_run: bool) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new("node");
        cmd.arg("agent/run.js").arg(prompt).arg(workspace_id);
        if dry_run {
            cmd.arg("--dry-run");
        }
        let output = cmd
            .current_dir(&self.server_dir)
            .output()
            .await
            .map_err(|e| format!("failed to spawn node agent/run.js: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty());
        let Some(last_line) = last_line else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("agent/run.js produced no output (stderr: {stderr})"));
        };
        parse_agent_turn_result(last_line)
    }
}

// --------------------------------------------------------- voice notes (#143)

/// Payload of a `voice_turn` job (worker/src/voice.ts): a Telegram voice note
/// carried as base64 audio bytes plus the chat to reply into.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct VoiceTurnPayload {
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub audio_b64: String,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Turns raw voice-note bytes into a transcript. DI seam so `run_voice_turn` is
/// unit-testable without a real whisper model or ffmpeg; the real impl
/// (`IngestVoiceTranscriber`) delegates to `lifeos_ingest::transcribe_audio_bytes`.
#[async_trait]
pub trait VoiceTranscriber: Send + Sync {
    async fn transcribe_voice(&self, audio_bytes: &[u8]) -> Result<String, String>;
}

/// Real voice transcription via `lifeos-ingest` (symphonia + whisper, with the
/// OGG/Opus ffmpeg fallback for Telegram voice notes). Borrows the `Transcriber`
/// the drain already constructed rather than owning a second whisper model.
pub struct IngestVoiceTranscriber<'a> {
    pub transcriber: &'a dyn lifeos_ingest::Transcriber,
    pub ffmpeg_bin: Option<&'a str>,
}

#[async_trait]
impl VoiceTranscriber for IngestVoiceTranscriber<'_> {
    async fn transcribe_voice(&self, audio_bytes: &[u8]) -> Result<String, String> {
        lifeos_ingest::transcribe_audio_bytes(audio_bytes, self.transcriber, self.ffmpeg_bin).await
    }
}

/// Orchestrates a claimed `voice_turn` job (issue #143): decode+transcribe the
/// voice note, stamp a `voice.received` event (the turn stamps its own
/// `agent.turn`), run an agent turn on the transcript, and reply into the
/// originating chat. Kept here (DI over trait objects) so it is unit-testable
/// without a whisper model, a `node` process, or a live Telegram call - same
/// discipline as `run_module_build`.
pub async fn run_voice_turn(
    conn: &Connection,
    transcriber: &dyn VoiceTranscriber,
    agent: &dyn AgentTurnRunner,
    notifier: &dyn Notifier,
    payload: VoiceTurnPayload,
    workspace_id: &str,
    now: i64,
) -> Result<(), String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.audio_b64.as_bytes())
        .map_err(|e| format!("voice payload audio_b64 was not valid base64: {e}"))?;

    let transcript = transcriber.transcribe_voice(&bytes).await?;
    if transcript.is_empty() {
        // An empty transcript is an honest dead-end, not an agent turn.
        notifier
            .notify(&payload.chat_id, "I couldn't make out any speech in that voice note.")
            .await;
        return Ok(());
    }

    // Append-only record of what was heard (docs/SECURITY.md §1). The agent
    // turn writes its own `agent.turn` row, so memory consolidates both.
    emit_event(
        conn,
        workspace_id,
        "voice.received",
        "",
        "mac-drain",
        &serde_json::json!({ "chars": transcript.len(), "chat_id": payload.chat_id, "mime": payload.mime }),
        now,
    )
    .await
    .map_err(|e| format!("failed to record voice.received event: {e}"))?;

    let reply = agent.run(&transcript, workspace_id, false).await?;
    let reply = if reply.trim().is_empty() { "(no reply)".to_string() } else { reply };
    notifier.notify(&payload.chat_id, &reply).await;
    Ok(())
}

// --------------------------------------------------------- daily brief (#144)

/// The fixed, single-source daily-brief prompt (issue #144). Kept a constant
/// so the read-only turn is reproducible and the budget stays small.
pub const DAILY_BRIEF_PROMPT: &str = "Produce a concise morning brief for this workspace as a short bulleted digest. \
Cover, using only what the world snapshot and memory context show (do not fabricate): \
tasks due today, the count of items pending approval, builds awaiting a gate, \
gaps in the trading journal, and learning items due for review. \
Keep it under 10 short lines. This is a read-only summary - take no actions.";

/// Pure once-a-day + after-the-hour gate for the daily brief (issue #144),
/// split out so the scheduling rule is directly unit-testable. Fires when the
/// local wall-clock hour has reached `brief_hour` AND no brief was already sent
/// on today's local calendar day.
///
/// - `now` / `last_sent` are UTC unix seconds; `tz_offset_secs` is the local
///   UTC offset (east-positive) so both convert to the same local day.
pub fn brief_due(now: i64, tz_offset_secs: i64, brief_hour: i64, last_sent: Option<i64>) -> bool {
    let local_now = now + tz_offset_secs;
    let local_hour = local_now.rem_euclid(86_400) / 3_600;
    if local_hour < brief_hour {
        return false;
    }
    let today = local_now.div_euclid(86_400);
    match last_sent {
        Some(ts) => (ts + tz_offset_secs).div_euclid(86_400) < today,
        None => true,
    }
}

/// The timestamp of the most recent `brief.sent` event for a workspace, if any.
async fn last_brief_sent(conn: &Connection, workspace_id: &str) -> libsql::Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT MAX(ts) FROM events WHERE workspace_id = ?1 AND type = 'brief.sent'",
            params![workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<i64>>(0)?),
        None => Ok(None),
    }
}

/// Daily-brief scheduler (issue #144), the counterpart to
/// `maybe_enqueue_memory_sleep`: on each poll tick, enqueue one `daily_brief`
/// job per workspace that is `brief_due` and has no `daily_brief` already
/// queued/running (debounce). The `brief.sent` ledger + the debounce together
/// guarantee at most one brief per workspace per local day. Returns jobs
/// enqueued.
pub async fn maybe_enqueue_daily_brief(
    conn: &Connection,
    brief_hour: i64,
    tz_offset_secs: i64,
    now: i64,
) -> libsql::Result<u64> {
    let mut rows = conn.query("SELECT id FROM workspaces ORDER BY id", ()).await?;
    let mut workspaces = Vec::new();
    while let Some(row) = rows.next().await? {
        workspaces.push(row.get::<String>(0)?);
    }
    let mut enqueued = 0;
    for ws in workspaces {
        let last_sent = last_brief_sent(conn, &ws).await?;
        if !brief_due(now, tz_offset_secs, brief_hour, last_sent) {
            continue;
        }
        let mut pending = conn
            .query(
                "SELECT 1 FROM jobs WHERE workspace_id = ?1 AND kind = 'daily_brief' \
                 AND status IN ('queued', 'running') LIMIT 1",
                params![ws.clone()],
            )
            .await?;
        if pending.next().await?.is_some() {
            continue; // debounce: a brief is already scheduled/running today
        }
        conn.execute(
            "INSERT INTO jobs (id, workspace_id, kind, payload, status, priority, attempts, created_at) \
             VALUES (?1, ?2, 'daily_brief', '{}', 'queued', 0, 0, ?3)",
            params![format!("job_{}", Ulid::new()), ws, now],
        )
        .await?;
        enqueued += 1;
    }
    Ok(enqueued)
}

/// Writes the `brief` entity the PWA reads (issue #144). This is the ONE write
/// the daily-brief flow makes; it happens OUTSIDE the dry-run agent turn, by
/// drain itself, so the turn stays side-effect-free. Returns the entity id.
async fn insert_brief_entity(
    conn: &Connection,
    workspace_id: &str,
    text: &str,
    now: i64,
) -> libsql::Result<String> {
    let entity_id = format!("ent_{}", Ulid::new());
    let attrs = serde_json::json!({ "text": text }).to_string();
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, title, attrs, source, created_at, updated_at) \
         VALUES (?1, ?2, 'briefs', 'brief', 'Daily brief', ?3, 'agent', ?4, ?4)",
        params![entity_id.clone(), workspace_id, attrs, now],
    )
    .await?;
    Ok(entity_id)
}

/// Orchestrates a claimed `daily_brief` job (issue #144): run a READ-ONLY
/// (dry-run) agent turn over the world snapshot + memory, write the resulting
/// `brief` entity (drain's own write, not the turn's), stamp the `brief.sent`
/// ledger event (the once-a-day guard), and send the digest to Telegram.
/// Testable over the same trait mocks as `run_voice_turn`.
pub async fn run_daily_brief(
    conn: &Connection,
    agent: &dyn AgentTurnRunner,
    notifier: &dyn Notifier,
    workspace_id: &str,
    chat_id: Option<&str>,
    now: i64,
) -> Result<(), String> {
    // dry_run = true: the turn itself creates no entities/edges (issue #140).
    let brief = agent.run(DAILY_BRIEF_PROMPT, workspace_id, true).await?;
    let brief = if brief.trim().is_empty() { "No brief content today.".to_string() } else { brief };

    let entity_id = insert_brief_entity(conn, workspace_id, &brief, now)
        .await
        .map_err(|e| format!("failed to write brief entity: {e}"))?;
    // `brief.sent` is the ledger the once-a-day guard (`brief_due`) reads.
    emit_event(conn, workspace_id, "brief.sent", &entity_id, "mac-drain", &serde_json::json!({}), now)
        .await
        .map_err(|e| format!("failed to record brief.sent event: {e}"))?;

    match chat_id {
        Some(chat) => notifier.notify(chat, &format!("Daily brief\n\n{brief}")).await,
        None => println!("lifeos-drain: daily brief for {workspace_id} (no admin chat configured): {brief}"),
    }
    Ok(())
}

// --------------------------------------------------------- retention (waste audit)
//
// `jobs` (including up-to-20MB base64 voice payloads) and `sessions` (auth
// refresh-token rows) are otherwise never pruned and grow without bound. A
// drain tick reaps the terminal, expired rows.

/// Max rows deleted per prune call, so one tick never blocks the poll loop on
/// a large backlog - successive ticks drain the rest.
const PRUNE_BATCH: i64 = 500;

/// Retention prune (waste audit): delete terminal (`done`/`failed`) jobs and
/// expired-or-revoked auth `sessions` older than `retention_secs`, in one
/// bounded batch per call. `events` is the append-only domain log (a hard
/// rule) and is NEVER touched here. Only terminal jobs are eligible, so a
/// `queued` or `running` job - including one another worker holds the lease
/// on - is never pruned, keeping this lease-safe. Returns
/// `(jobs_pruned, sessions_pruned)`.
pub async fn prune_old_jobs_and_sessions(
    conn: &Connection,
    retention_secs: i64,
    now: i64,
) -> libsql::Result<(u64, u64)> {
    let cutoff = now - retention_secs;
    // DELETE ... WHERE id IN (SELECT ... LIMIT ?) rather than DELETE ... LIMIT
    // so it works regardless of libSQL's UPDATE/DELETE-LIMIT compile flag.
    let jobs_pruned = conn
        .execute(
            "DELETE FROM jobs WHERE id IN ( \
                SELECT id FROM jobs \
                WHERE status IN ('done', 'failed') AND created_at < ?1 \
                LIMIT ?2 \
             )",
            params![cutoff, PRUNE_BATCH],
        )
        .await?;
    let sessions_pruned = conn
        .execute(
            "DELETE FROM sessions WHERE id IN ( \
                SELECT id FROM sessions \
                WHERE (expires_at < ?1 OR (revoked_at IS NOT NULL AND revoked_at < ?1)) \
                LIMIT ?2 \
             )",
            params![cutoff, PRUNE_BATCH],
        )
        .await?;
    Ok((jobs_pruned, sessions_pruned))
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
        assert_eq!(dispatch("execute_approval"), Dispatch::Stub("lifeos-drain approval resume"));
        assert_eq!(dispatch("memory_sleep"), Dispatch::MemorySleep);
        assert_eq!(dispatch("voice_turn"), Dispatch::VoiceTurn);
        assert_eq!(dispatch("daily_brief"), Dispatch::DailyBrief);
        assert_eq!(dispatch("nonsense"), Dispatch::Unknown);
    }

    // ------------------------------------------------ resume-on-approval (#142)

    struct MockResumer {
        result: Result<String, String>,
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl BuildResumer for MockResumer {
        async fn resume(&self, approval_entity_id: &str, workspace_id: &str) -> Result<String, String> {
            self.calls.lock().unwrap().push((approval_entity_id.to_string(), workspace_id.to_string()));
            self.result.clone()
        }
    }

    #[test]
    fn should_resume_build_only_for_pipeline_gates_with_the_flag_on() {
        // Build gate + flag on -> resume.
        assert!(should_resume_build("pending_approval", true));
        // Flag off (untrusted lifeos-node) -> never resume, even a gate.
        assert!(!should_resume_build("pending_approval", false));
        // A draft / storage-backend approval is not a build -> never resume.
        assert!(!should_resume_build("draft", true));
        assert!(!should_resume_build("storage_backend", true));
    }

    #[tokio::test]
    async fn run_approval_resume_skips_non_build_and_flag_off() {
        let resumer = MockResumer { result: Ok("build_1".into()), calls: Mutex::new(vec![]) };
        let draft = ExecuteApprovalPayload { entity_id: "ent_d".into(), entity_type: "draft".into() };
        assert!(run_approval_resume(&resumer, &draft, "ws1", true).await.is_none());

        let gate = ExecuteApprovalPayload { entity_id: "ent_g".into(), entity_type: "pending_approval".into() };
        assert!(run_approval_resume(&resumer, &gate, "ws1", false).await.is_none(), "flag off must not resume");
        assert_eq!(resumer.calls.lock().unwrap().len(), 0, "resumer never invoked");
    }

    #[tokio::test]
    async fn run_approval_resume_runs_the_gate_build_when_enabled() {
        let resumer = MockResumer { result: Ok("build_42".into()), calls: Mutex::new(vec![]) };
        let gate = ExecuteApprovalPayload { entity_id: "ent_gate".into(), entity_type: "pending_approval".into() };

        let outcome = run_approval_resume(&resumer, &gate, "ws1", true).await;
        assert_eq!(outcome, Some(Ok("build_42".to_string())));
        let calls = resumer.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("ent_gate".to_string(), "ws1".to_string()));
    }

    #[tokio::test]
    async fn execute_approval_job_resumes_the_gate_only_when_the_build_pipeline_is_on() {
        // The exact JSON an `execute_approval` job for a build gate carries.
        let gate_payload = r#"{"entity_id":"ent_gate","entity_type":"pending_approval"}"#;

        // LIFEOS_BUILD_PIPELINE off (untrusted node): the resumer is NEVER
        // invoked and the job is acknowledged (None) rather than resumed.
        let resumer_off = MockResumer { result: Ok("build_1".into()), calls: Mutex::new(vec![]) };
        assert!(run_approval_resume_from_payload(&resumer_off, gate_payload, "ws1", false)
            .await
            .is_none());
        assert_eq!(resumer_off.calls.lock().unwrap().len(), 0, "flag off must never resume");

        // LIFEOS_BUILD_PIPELINE on (trusted Mac): the same job triggers the
        // resumer exactly once with the gate entity + workspace.
        let resumer_on = MockResumer { result: Ok("build_1".into()), calls: Mutex::new(vec![]) };
        let outcome = run_approval_resume_from_payload(&resumer_on, gate_payload, "ws1", true).await;
        assert_eq!(outcome, Some(Ok("build_1".to_string())));
        let calls = resumer_on.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("ent_gate".to_string(), "ws1".to_string()));
    }

    #[tokio::test]
    async fn execute_approval_job_with_a_non_gate_or_malformed_payload_is_acknowledged() {
        let resumer = MockResumer { result: Ok("build_1".into()), calls: Mutex::new(vec![]) };
        // A draft approval is not a build - acknowledged, never resumed.
        assert!(run_approval_resume_from_payload(
            &resumer,
            r#"{"entity_id":"ent_d","entity_type":"draft"}"#,
            "ws1",
            true,
        )
        .await
        .is_none());
        // A malformed payload falls back to Default (empty type) - also not a
        // build gate, so it is acknowledged rather than crashing the job.
        assert!(run_approval_resume_from_payload(&resumer, "not json", "ws1", true).await.is_none());
        assert_eq!(resumer.calls.lock().unwrap().len(), 0);
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

    // ------------------------------------------------------ agent-turn parse

    #[test]
    fn parse_agent_turn_result_reads_text_and_surfaces_failures() {
        assert_eq!(
            parse_agent_turn_result(r#"{"success":true,"outcome":"completed","text":"hi there"}"#).unwrap(),
            "hi there"
        );
        // Success with no text is an empty (not failed) reply.
        assert_eq!(parse_agent_turn_result(r#"{"success":true,"outcome":"completed"}"#).unwrap(), "");
        // Failure surfaces the honest error.
        assert_eq!(
            parse_agent_turn_result(r#"{"success":false,"outcome":"failed","error":"model down"}"#).unwrap_err(),
            "model down"
        );
        assert!(parse_agent_turn_result("not json").is_err());
    }

    // ------------------------------------------------------- voice notes (#143)

    struct MockVoiceTranscriber {
        result: Result<String, String>,
        seen: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl VoiceTranscriber for MockVoiceTranscriber {
        async fn transcribe_voice(&self, audio_bytes: &[u8]) -> Result<String, String> {
            self.seen.lock().unwrap().push(audio_bytes.len());
            self.result.clone()
        }
    }

    struct MockAgentRunner {
        result: Result<String, String>,
        calls: Mutex<Vec<(String, String, bool)>>,
    }

    #[async_trait]
    impl AgentTurnRunner for MockAgentRunner {
        async fn run(&self, prompt: &str, workspace_id: &str, dry_run: bool) -> Result<String, String> {
            self.calls.lock().unwrap().push((prompt.to_string(), workspace_id.to_string(), dry_run));
            self.result.clone()
        }
    }

    fn voice_payload(bytes: &[u8], chat_id: &str) -> VoiceTurnPayload {
        use base64::Engine as _;
        VoiceTurnPayload {
            chat_id: chat_id.to_string(),
            audio_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime: Some("audio/ogg".into()),
            file_name: Some("voice/file_1.oga".into()),
        }
    }

    #[tokio::test]
    async fn run_voice_turn_transcribes_runs_the_turn_and_replies() {
        let conn = fresh_conn("test_voice_turn_ok.db").await;
        let transcriber = MockVoiceTranscriber {
            result: Ok("what is due today".into()),
            seen: Mutex::new(vec![]),
        };
        let agent = MockAgentRunner { result: Ok("You have 2 tasks due.".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        run_voice_turn(&conn, &transcriber, &agent, &notifier, voice_payload(b"raw-ogg-bytes", "chat_7"), "ws1", 100)
            .await
            .unwrap();

        // A voice.received event was appended (await first, before any guard).
        assert_eq!(event_count(&conn, "voice.received").await, 1);
        // Transcriber saw the decoded bytes (13 = len of "raw-ogg-bytes").
        assert_eq!(transcriber.seen.lock().unwrap().clone(), vec![13]);
        // The agent turn ran on the transcript, NOT dry-run (a real reply).
        assert_eq!(agent.calls.lock().unwrap().clone(), vec![(
            "what is due today".to_string(),
            "ws1".to_string(),
            false,
        )]);
        // The reply went to the originating chat.
        assert_eq!(
            notifier.calls.lock().unwrap().clone(),
            vec![("chat_7".to_string(), "You have 2 tasks due.".to_string())]
        );

        let _ = std::fs::remove_file("test_voice_turn_ok.db");
    }

    #[tokio::test]
    async fn run_voice_turn_replies_gracefully_on_empty_transcript_without_a_turn() {
        let conn = fresh_conn("test_voice_turn_empty.db").await;
        // An empty transcript (whisper heard nothing) is a friendly dead-end,
        // not an agent turn.
        let transcriber = MockVoiceTranscriber { result: Ok(String::new()), seen: Mutex::new(vec![]) };
        let agent = MockAgentRunner { result: Ok("unused".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        run_voice_turn(&conn, &transcriber, &agent, &notifier, voice_payload(b"x", "chat_1"), "ws1", 100)
            .await
            .unwrap();

        // No agent turn ran, but the user still got a friendly reply.
        assert_eq!(agent.calls.lock().unwrap().len(), 0);
        assert_eq!(notifier.calls.lock().unwrap().len(), 1);
        assert_eq!(event_count(&conn, "voice.received").await, 0);

        let _ = std::fs::remove_file("test_voice_turn_empty.db");
    }

    #[tokio::test]
    async fn run_voice_turn_fails_when_transcription_fails() {
        let conn = fresh_conn("test_voice_turn_fail.db").await;
        let transcriber = MockVoiceTranscriber {
            result: Err("no whisper model configured".into()),
            seen: Mutex::new(vec![]),
        };
        let agent = MockAgentRunner { result: Ok("unused".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        let err = run_voice_turn(&conn, &transcriber, &agent, &notifier, voice_payload(b"x", "chat_1"), "ws1", 100)
            .await
            .unwrap_err();
        assert!(err.contains("whisper"));
        assert_eq!(agent.calls.lock().unwrap().len(), 0);

        let _ = std::fs::remove_file("test_voice_turn_fail.db");
    }

    // -------------------------------------------------------- daily brief (#144)

    #[test]
    fn brief_due_gates_on_hour_and_once_per_local_day() {
        // Fixed local offset of 0 (UTC) keeps the arithmetic obvious.
        // 2021-01-01 07:00 UTC -> hour 7 < 8: not yet due.
        assert!(!brief_due(1_609_484_400, 0, 8, None));
        // 2021-01-01 08:00 UTC -> hour 8 >= 8, no prior brief: due.
        assert!(brief_due(1_609_488_000, 0, 8, None));
        // Same day, already sent at 08:00 -> not due again at 09:00.
        assert!(!brief_due(1_609_491_600, 0, 8, Some(1_609_488_000)));
        // Next day after the hour, last sent yesterday -> due again.
        assert!(brief_due(1_609_574_400 + 3_600, 0, 8, Some(1_609_488_000)));
    }

    #[test]
    fn brief_due_respects_a_nonzero_local_offset() {
        // 2021-01-01 00:00:00 UTC.
        const MIDNIGHT_UTC: i64 = 1_609_459_200;
        // +05:30 (IST, 19800s). 02:45 UTC == 08:15 IST -> hour 8 >= 8: due.
        assert!(brief_due(MIDNIGHT_UTC + 2 * 3_600 + 45 * 60, 19_800, 8, None));
        // 01:00 UTC == 06:30 IST -> before hour 8: not due.
        assert!(!brief_due(MIDNIGHT_UTC + 3_600, 19_800, 8, None));
    }

    async fn brief_conn(path: &str) -> Connection {
        let conn = fresh_conn(path).await;
        conn.execute_batch(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT, created_at INTEGER, updated_at INTEGER);
             CREATE TABLE jobs (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL,
                payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued',
                priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER,
                attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL);
             CREATE TABLE entities (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT NOT NULL,
                type TEXT NOT NULL, parent_id TEXT, title TEXT, status TEXT, tier TEXT,
                attrs TEXT NOT NULL DEFAULT '{}', source TEXT, blob_ref TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             INSERT INTO workspaces VALUES ('ws_default', 'p', 1, 1);",
        )
        .await
        .unwrap();
        conn
    }

    #[tokio::test]
    async fn maybe_enqueue_daily_brief_fires_once_per_day_then_debounces() {
        let path = "test_daily_brief_enqueue.db";
        let conn = brief_conn(path).await;
        // 09:00 UTC, offset 0, hour 8: due. First tick enqueues one job.
        let now = 1_609_491_600;
        assert_eq!(maybe_enqueue_daily_brief(&conn, 8, 0, now).await.unwrap(), 1);
        // Second tick the same day: a daily_brief job is already queued -> 0.
        assert_eq!(maybe_enqueue_daily_brief(&conn, 8, 0, now + 60).await.unwrap(), 0, "debounced");

        let mut rows = conn
            .query("SELECT COUNT(*) FROM jobs WHERE kind = 'daily_brief'", ())
            .await
            .unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(n, 1);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn maybe_enqueue_daily_brief_skips_before_the_hour() {
        let path = "test_daily_brief_before_hour.db";
        let conn = brief_conn(path).await;
        // 07:00 UTC, hour 7 < 8: nothing enqueued.
        assert_eq!(maybe_enqueue_daily_brief(&conn, 8, 0, 1_609_484_400).await.unwrap(), 0);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn run_daily_brief_runs_a_dry_run_turn_and_writes_exactly_one_brief() {
        let conn = brief_conn("test_run_daily_brief.db").await;
        let agent = MockAgentRunner {
            result: Ok("- 2 tasks due\n- 1 pending approval".into()),
            calls: Mutex::new(vec![]),
        };
        let notifier = MockNotifier::default();

        run_daily_brief(&conn, &agent, &notifier, "ws_default", Some("admin_chat"), 200).await.unwrap();

        // The agent turn ran READ-ONLY (dry_run = true) on the fixed prompt.
        let calls = agent.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, DAILY_BRIEF_PROMPT);
        assert!(calls[0].2, "brief turn must be dry-run");

        // Exactly one brief entity was written - by drain, not the dry-run turn.
        let mut rows = conn
            .query("SELECT COUNT(*) FROM entities WHERE type = 'brief' AND workspace_id = 'ws_default'", ())
            .await
            .unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(n, 1);
        // The brief.sent ledger event (the once-a-day guard) was stamped.
        assert_eq!(event_count(&conn, "brief.sent").await, 1);
        // The digest went to the admin chat.
        let notes = notifier.calls.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "admin_chat");
        assert!(notes[0].1.contains("2 tasks due"));

        let _ = std::fs::remove_file("test_run_daily_brief.db");
    }

    // ---------------------------------------- execute_approval (audit finding 10)

    async fn queue_conn(path: &str) -> Connection {
        let conn = fresh_conn(path).await; // module_requests + events
        conn.execute(
            "CREATE TABLE jobs (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL, \
                payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued', \
                priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER, \
                attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    async fn insert_running_approval(conn: &Connection, id: &str, payload: &str) {
        conn.execute(
            "INSERT INTO jobs (id, workspace_id, kind, payload, status, claimed_by, claimed_at, attempts, created_at) \
             VALUES (?1, 'ws1', 'execute_approval', ?2, 'running', 'worker_1', 100, 1, 100)",
            params![id, payload],
        )
        .await
        .unwrap();
    }

    async fn job_status(conn: &Connection, id: &str) -> String {
        let mut rows = conn.query("SELECT status FROM jobs WHERE id=?1", params![id]).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    fn claimed(id: &str, payload: &str) -> ClaimedJob {
        ClaimedJob {
            id: id.to_string(),
            kind: "execute_approval".to_string(),
            payload: payload.to_string(),
            workspace_id: "ws1".to_string(),
        }
    }

    #[test]
    fn classify_non_build_approval_only_fails_outward_sends() {
        assert_eq!(classify_non_build_approval("storage_backend"), NonBuildApproval::Acknowledge);
        assert_eq!(classify_non_build_approval("pending_approval"), NonBuildApproval::Acknowledge);
        assert_eq!(classify_non_build_approval(""), NonBuildApproval::Acknowledge);
        assert_eq!(classify_non_build_approval("draft"), NonBuildApproval::OutwardUnimplemented);
        assert_eq!(classify_non_build_approval("gmail_send"), NonBuildApproval::OutwardUnimplemented);
        assert_eq!(classify_non_build_approval("whatsapp_send"), NonBuildApproval::OutwardUnimplemented);
    }

    #[tokio::test]
    async fn execute_approval_outward_draft_fails_and_notifies_not_completes() {
        let conn = queue_conn("test_ea_draft.db").await;
        let payload = r#"{"entity_id":"ent_d","entity_type":"draft"}"#;
        insert_running_approval(&conn, "job_draft", payload).await;
        let resumer = MockResumer { result: Ok("unused".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        let n = run_execute_approval(
            &conn,
            &resumer,
            &notifier,
            Some("admin_chat"),
            &claimed("job_draft", payload),
            "worker_1",
            true,
        )
        .await
        .unwrap();

        // Failed, not completed - never report `done` for a send that never happened.
        assert_eq!(n, 1, "the fail write touched the running job");
        assert_eq!(job_status(&conn, "job_draft").await, "failed");
        // A draft is not a build gate, so the resumer is never invoked.
        assert_eq!(resumer.calls.lock().unwrap().len(), 0);
        // The approver was told, verbatim.
        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "admin_chat");
        assert_eq!(
            calls[0].1,
            "Approved draft but outward execution is not yet implemented - not sent."
        );

        let _ = std::fs::remove_file("test_ea_draft.db");
    }

    #[tokio::test]
    async fn execute_approval_storage_backend_is_acknowledged_without_alarm() {
        let conn = queue_conn("test_ea_storage.db").await;
        let payload = r#"{"entity_id":"ent_s","entity_type":"storage_backend"}"#;
        insert_running_approval(&conn, "job_storage", payload).await;
        let resumer = MockResumer { result: Ok("unused".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        run_execute_approval(
            &conn,
            &resumer,
            &notifier,
            Some("admin_chat"),
            &claimed("job_storage", payload),
            "worker_1",
            true,
        )
        .await
        .unwrap();

        assert_eq!(job_status(&conn, "job_storage").await, "done");
        assert_eq!(notifier.calls.lock().unwrap().len(), 0, "no 'not sent' alarm for a storage switch");

        let _ = std::fs::remove_file("test_ea_storage.db");
    }

    #[tokio::test]
    async fn execute_approval_build_gate_resumes_and_completes() {
        let conn = queue_conn("test_ea_gate.db").await;
        let payload = r#"{"entity_id":"ent_g","entity_type":"pending_approval"}"#;
        insert_running_approval(&conn, "job_gate", payload).await;
        let resumer = MockResumer { result: Ok("build_9".into()), calls: Mutex::new(vec![]) };
        let notifier = MockNotifier::default();

        run_execute_approval(
            &conn,
            &resumer,
            &notifier,
            Some("admin_chat"),
            &claimed("job_gate", payload),
            "worker_1",
            true,
        )
        .await
        .unwrap();

        assert_eq!(job_status(&conn, "job_gate").await, "done");
        assert_eq!(resumer.calls.lock().unwrap().len(), 1, "the gate build resumed once");
        assert_eq!(notifier.calls.lock().unwrap().len(), 0);

        let _ = std::fs::remove_file("test_ea_gate.db");
    }

    // ------------------------------------------------ retention prune (finding 40)

    async fn prune_conn(path: &str) -> Connection {
        let conn = fresh_conn(path).await; // module_requests + events
        conn.execute_batch(
            "CREATE TABLE jobs (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL, \
                payload TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'queued', \
                priority INTEGER DEFAULT 0, run_after INTEGER, claimed_by TEXT, claimed_at INTEGER, \
                attempts INTEGER DEFAULT 0, created_at INTEGER NOT NULL);
             CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, workspace_id TEXT NOT NULL, \
                refresh_token_hash TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, \
                revoked_at INTEGER);",
        )
        .await
        .unwrap();
        conn
    }

    async fn count_all(conn: &Connection, sql: &str) -> i64 {
        let mut rows = conn.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn row_exists(conn: &Connection, table: &str, id: &str) -> bool {
        let mut rows =
            conn.query(&format!("SELECT 1 FROM {table} WHERE id=?1"), params![id]).await.unwrap();
        rows.next().await.unwrap().is_some()
    }

    #[tokio::test]
    async fn prune_reaps_old_terminal_jobs_and_expired_sessions_but_keeps_the_rest_and_events() {
        let conn = prune_conn("test_prune.db").await;
        let now: i64 = 1_000_000;
        let retention = 7 * 86_400; // 604800; cutoff = 395200
        let recent = now - 10;

        // done/failed older than the window -> pruned; recent done + queued +
        // running -> kept (only terminal, old rows are eligible).
        conn.execute_batch(&format!(
            "INSERT INTO jobs (id, workspace_id, kind, status, created_at) VALUES \
               ('j_done_old','ws1','ingest','done',100), \
               ('j_failed_old','ws1','ingest','failed',200), \
               ('j_done_recent','ws1','ingest','done',{recent}), \
               ('j_queued_old','ws1','ingest','queued',100), \
               ('j_running_old','ws1','ingest','running',100);"
        ))
        .await
        .unwrap();

        // long-expired + long-revoked -> pruned; active + recently-expired -> kept.
        conn.execute_batch(&format!(
            "INSERT INTO sessions (id, user_id, workspace_id, refresh_token_hash, created_at, expires_at, revoked_at) VALUES \
               ('s_expired_old','u','ws1','h',10,100,NULL), \
               ('s_revoked_old','u','ws1','h',10,2000000,200), \
               ('s_active','u','ws1','h',10,2000000,NULL), \
               ('s_expired_recent','u','ws1','h',10,{recent},NULL);"
        ))
        .await
        .unwrap();

        // An events row (append-only domain log) must survive untouched.
        emit_event(&conn, "ws1", "note.captured", "", "user", &serde_json::json!({}), 100).await.unwrap();

        let (jobs, sessions) = prune_old_jobs_and_sessions(&conn, retention, now).await.unwrap();
        assert_eq!(jobs, 2);
        assert_eq!(sessions, 2);

        assert_eq!(count_all(&conn, "SELECT COUNT(*) FROM jobs").await, 3);
        for id in ["j_done_recent", "j_queued_old", "j_running_old"] {
            assert!(row_exists(&conn, "jobs", id).await, "{id} should be kept");
        }

        assert_eq!(count_all(&conn, "SELECT COUNT(*) FROM sessions").await, 2);
        for id in ["s_active", "s_expired_recent"] {
            assert!(row_exists(&conn, "sessions", id).await, "{id} should be kept");
        }

        // events is never touched by the prune.
        assert_eq!(event_count(&conn, "note.captured").await, 1);

        let _ = std::fs::remove_file("test_prune.db");
    }
}
