//! lifeos-drain: the Mac-side job queue consumer. Polls `jobs`, atomically
//! claims one at a time, dispatches by kind, and reaps crashed claims. Also
//! polls `module_requests` directly for bot-queued self-extension builds
//! (issue #78) - that path doesn't go through `jobs` at all (see
//! `lifeos_drain::claim_next_module_request`'s doc comment for why).
//!
//! Config (env): LIFEOS_DB_PATH (default `lifeos.db`),
//! LIFEOS_DRAIN_POLL_SECS (3), LIFEOS_DRAIN_STUCK_TTL_SECS (300),
//! LIFEOS_DRAIN_MAX_ATTEMPTS (3), LIFEOS_SERVER_DIR (default `server`, the
//! directory `scaffold.js` lives in - set explicitly for a compiled binary
//! whose cwd isn't the repo root, e.g. in the launchd plist),
//! TELEGRAM_BOT_TOKEN (optional - without it, module-build notifications are
//! logged locally instead of sent to Telegram), LIFEOS_VCS_BLOB_ROOT (default
//! `lifeos-blobs`, same env var `lifeos-api` uses for its blob store),
//! LIFEOS_MEMVEC (optional path to `server/memvec.py` - without it, ingest
//! still creates segments, just without semantic embedding),
//! LIFEOS_DERIVED_DB_PATH (default `lifeos-derived.db`, only used when
//! LIFEOS_MEMVEC is set), LIFEOS_WHISPER_MODEL (optional path to a GGML
//! whisper.cpp model, e.g. ggml-tiny.en.bin - without it, audio ingest jobs
//! fail loudly rather than silently producing zero segments, see #89),
//! LIFEOS_AGENT / LIFEOS_AGENT_MODEL (optional - which local agent CLI and
//! model the keyless AI lanes use; default is the first CLI detected on
//! PATH), ANTHROPIC_API_KEY (optional FALLBACK - only consulted when no
//! agent CLI is detected on PATH; without either, image ingest jobs fail
//! loudly, see #90, and pipeline jobs fail loudly too, see #92; it also
//! gates whether the pipeline eval stage uses a real judge or the length
//! heuristic fallback, see #96), LIFEOS_TESSERACT_BIN
//! (optional path to the `tesseract` CLI binary - without it, image ingest
//! still captions, just without OCR text, see #90).
//! PIPELINE_EVAL_SAMPLE_RATE (default 0.2 - fraction of eval-gated stages
//! that call the real Haiku judge; the rest use the heuristic fallback,
//! keeping the judge "cents/day"), TELEGRAM_ADMIN_CHAT_ID (optional -
//! without it, a gated pipeline's rationale is only logged locally, see
//! #96).
//!
//! Each poll tick also runs the Life OS Actions engine (issue #93,
//! `lifeos_actions::run_action_engine_tick`) - no extra env var needed, it
//! reuses the already-open DB connection to scan `events` and enqueue
//! `action` jobs for any declared rule that fires.

use libsql::Builder;
use lifeos_drain::ai::{AgentCliCaptioner, AgentCliJudge, AgentCliModel, AgentCliStageRunner};
use lifeos_drain::{
    claim_job, claim_next_module_request, complete_job, dispatch, fail_job, notify_pipeline_gated,
    reap_stuck, run_module_build, Dispatch, DrainConfig, NoopNotifier, Notifier, ScaffoldJsBuilder,
    TelegramNotifier,
};
use lifeos_ingest::{
    Captioner, Embedder, HaikuCaptioner, IngestJobPayload, NoopCaptioner, NoopEmbedder, NoopOcr,
    NoopTranscriber, Ocr, SubprocessEmbedder, TesseractOcr, Transcriber, WhisperTranscriber,
};
use lifeos_pipelines::{
    HaikuJudge, HaikuStageRunner, Judge, NoopStageRunner, PipelineJobPayload, PipelineOutcome,
    PipelineStageRunner,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

mod node;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn env_int(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_float(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Open the DB the drain consumes, in one of two modes:
///
/// - **Turso primary (remote, issue #141):** when `TURSO_URL` + `TURSO_TOKEN`
///   are both set - the `lifeos-node` container's mode. A *pure remote*
///   connection (not an embedded replica) runs every statement server-side on
///   the single primary, so `claim_job`'s atomic `UPDATE ... RETURNING` stays
///   the one place a job is claimed even with N nodes racing: there is no local
///   replica to fall behind and double-claim under last-push-wins sync.
///   `LIFEOS_DB_PATH` is ignored in this mode.
/// - **Local file (default):** a plain local libSQL file at `LIFEOS_DB_PATH` -
///   the personal-Mac default, where `lifeos-api` owns the embedded replica and
///   the drain shares the same on-disk file (unchanged behavior). The Mac
///   drain never sets `TURSO_URL`/`TURSO_TOKEN`, so this branch is preserved
///   byte-for-byte for it.
async fn open_database(db_path: &str) -> Result<libsql::Database, libsql::Error> {
    let turso_url = std::env::var("TURSO_URL").ok().filter(|s| !s.is_empty());
    let turso_token = std::env::var("TURSO_TOKEN").ok().filter(|s| !s.is_empty());
    match (turso_url, turso_token) {
        (Some(url), Some(token)) => {
            println!("lifeos-drain: connecting to Turso primary (remote mode; LIFEOS_DB_PATH ignored)");
            Builder::new_remote(url, token).build().await
        }
        _ => {
            println!("lifeos-drain: opening local DB {db_path}");
            Builder::new_local(db_path).build().await
        }
    }
}

#[tokio::main]
async fn main() {
    let db_path = std::env::var("LIFEOS_DB_PATH").unwrap_or_else(|_| "lifeos.db".to_string());
    let poll = Duration::from_secs(env_int("LIFEOS_DRAIN_POLL_SECS", 3).max(1) as u64);
    let memory_sleep_threshold = env_int("LIFEOS_MEMORY_SLEEP_THRESHOLD", 25).max(1);
    let cfg = DrainConfig {
        stuck_ttl_secs: env_int("LIFEOS_DRAIN_STUCK_TTL_SECS", 300),
        max_attempts: env_int("LIFEOS_DRAIN_MAX_ATTEMPTS", 3),
    };

    let db = match open_database(&db_path).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("lifeos-drain: failed to open database: {e}");
            std::process::exit(1);
        }
    };
    let conn = db.connect().expect("connect");
    // Wait rather than error on a write lock so two drainers cooperate.
    let _ = conn.execute("PRAGMA busy_timeout = 5000", ()).await;

    // Node identity for claim observability (issue #141). On the Mac this is
    // just the hostname; on a `lifeos-node` container it is `LIFEOS_NODE_ID` (or
    // the container id via `HOSTNAME`). `worker_id` stays the per-process lease
    // owner (must be unique per running drainer); `node_id` is the stable,
    // human-meaningful fleet identity stamped into `job.claimed` events.
    let node_id = node::node_id();
    let worker_id = format!("{node_id}-{}", now_secs());
    println!("lifeos-drain: node '{node_id}' worker {worker_id} (poll {poll:?}, {cfg:?})");

    let server_dir = std::env::var("LIFEOS_SERVER_DIR").unwrap_or_else(|_| "server".to_string());
    // LIFEOS_BUILD_PIPELINE doubles as the trusted-node gate (issue #141):
    // - default on (unset / `1`): TRUSTED. This drainer polls `module_requests`
    //   and runs the full multi-tier self-extension build pipeline (#132,
    //   `build/run.js`) - the Mac's posture.
    // - `0`: UNTRUSTED node. Codegen is disabled entirely: the drainer does NOT
    //   poll `module_requests` and never scaffolds/commits code (CLAUDE.md:
    //   "codegen runs only on the trusted Mac"). A remote `lifeos-node` runs
    //   this way by default; flipping the flag to `1` marks the node trusted.
    let build_pipeline = std::env::var("LIFEOS_BUILD_PIPELINE").map(|v| v != "0").unwrap_or(true);
    if !build_pipeline {
        println!("lifeos-drain: LIFEOS_BUILD_PIPELINE=0 (untrusted-node posture): codegen disabled, not polling module_requests");
    }
    let builder = ScaffoldJsBuilder { server_dir, build_pipeline };
    let notifier: Box<dyn Notifier> = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(token) if !token.is_empty() => Box::new(TelegramNotifier::new(token)),
        _ => {
            println!("lifeos-drain: TELEGRAM_BOT_TOKEN not set, module-build notifications will only be logged");
            Box::new(NoopNotifier)
        }
    };

    let vcs_blob_root = std::env::var("LIFEOS_VCS_BLOB_ROOT").unwrap_or_else(|_| "lifeos-blobs".to_string());
    let vcs_store = lifeos_vcs::ObjectStore::new(vcs_blob_root.clone());
    // The local CAS backend memory_sleep tiers cold nodes into. Drain has no
    // per-workspace storage-backend config machinery (that lives in lifeos-api),
    // so it uses the same local blob root as its honest default - identical to
    // lifeos-api's `primary_backend` fallback when nothing is configured.
    let sleep_backend = lifeos_vcs::LocalFsBackend::new(vcs_blob_root);
    let embedder: Box<dyn Embedder> = match std::env::var("LIFEOS_MEMVEC") {
        Ok(memvec_path) if !memvec_path.is_empty() => {
            let derived_db_path =
                std::env::var("LIFEOS_DERIVED_DB_PATH").unwrap_or_else(|_| "lifeos-derived.db".to_string());
            Box::new(SubprocessEmbedder { memvec_path, derived_db_path })
        }
        _ => {
            println!("lifeos-drain: LIFEOS_MEMVEC not set, ingested segments won't be semantically embedded");
            Box::new(NoopEmbedder)
        }
    };

    let transcriber: Box<dyn Transcriber> = match std::env::var("LIFEOS_WHISPER_MODEL") {
        Ok(model_path) if !model_path.is_empty() => Box::new(WhisperTranscriber { model_path }),
        _ => {
            println!("lifeos-drain: LIFEOS_WHISPER_MODEL not set, audio ingest jobs will fail loudly");
            Box::new(NoopTranscriber)
        }
    };

    // Keyless-first AI lanes (issue: run everything through the agent CLIs
    // already on this machine): any detected CLI beats the API-key path.
    let cli_agents = std::sync::Arc::new(lifeos_agents::detect());
    if let Some(id) = lifeos_agents::default_agent_id(&cli_agents) {
        println!("lifeos-drain: AI lanes use local agent CLI '{id}' (override with LIFEOS_AGENT/LIFEOS_AGENT_MODEL)");
    }

    let captioner: Box<dyn Captioner> = if !cli_agents.is_empty() {
        Box::new(AgentCliCaptioner { agents: cli_agents.clone() })
    } else {
        match std::env::var("ANTHROPIC_API_KEY") {
            Ok(api_key) if !api_key.is_empty() => Box::new(HaikuCaptioner { api_key }),
            _ => {
                println!("lifeos-drain: no agent CLI on PATH and ANTHROPIC_API_KEY not set, image ingest jobs will fail loudly");
                Box::new(NoopCaptioner)
            }
        }
    };

    let ocr: Box<dyn Ocr> = match std::env::var("LIFEOS_TESSERACT_BIN") {
        Ok(bin_path) if !bin_path.is_empty() => Box::new(TesseractOcr { bin_path }),
        _ => {
            println!("lifeos-drain: LIFEOS_TESSERACT_BIN not set, image ingest will caption without OCR text");
            Box::new(NoopOcr)
        }
    };

    let pipeline_runner: Box<dyn PipelineStageRunner> = if !cli_agents.is_empty() {
        Box::new(AgentCliStageRunner { agents: cli_agents.clone() })
    } else {
        match std::env::var("ANTHROPIC_API_KEY") {
            Ok(api_key) if !api_key.is_empty() => Box::new(HaikuStageRunner { api_key }),
            _ => {
                println!("lifeos-drain: no agent CLI on PATH and ANTHROPIC_API_KEY not set, pipeline jobs will fail loudly");
                Box::new(NoopStageRunner)
            }
        }
    };

    // Real judge when an agent CLI (preferred) or a key is available;
    // otherwise fall back to `HeuristicJudge` itself (it implements `Judge`
    // too) so eval-gated stages keep #92's original length-based behavior
    // rather than every gate call erroring.
    let pipeline_judge: Box<dyn Judge> = if !cli_agents.is_empty() {
        Box::new(AgentCliJudge { agents: cli_agents.clone() })
    } else {
        match std::env::var("ANTHROPIC_API_KEY") {
            Ok(api_key) if !api_key.is_empty() => Box::new(HaikuJudge { api_key }),
            _ => {
                println!("lifeos-drain: no agent CLI on PATH and ANTHROPIC_API_KEY not set, pipeline eval gate will use the length heuristic only");
                Box::new(lifeos_pipelines::HeuristicJudge)
            }
        }
    };
    let pipeline_eval_sample_rate = env_float("PIPELINE_EVAL_SAMPLE_RATE", 0.2);
    let telegram_admin_chat_id = std::env::var("TELEGRAM_ADMIN_CHAT_ID").ok();
    if telegram_admin_chat_id.is_none() {
        println!("lifeos-drain: TELEGRAM_ADMIN_CHAT_ID not set, gated pipeline rationales will only be logged");
    }

    loop {
        match claim_job(&conn, &worker_id, now_secs(), cfg).await {
            Ok(Some(job)) => {
                // Observability stamp (issue #141) - which node claimed this
                // job. Non-fatal: a failed stamp must never abort the job.
                if let Err(e) =
                    node::emit_claim_event(&conn, &job.workspace_id, &job.id, &job.kind, &node_id, now_secs()).await
                {
                    eprintln!("lifeos-drain: claim-event stamp for {} failed: {e}", job.id);
                }
                run_job(
                    &conn,
                    &job,
                    &worker_id,
                    &vcs_store,
                    embedder.as_ref(),
                    transcriber.as_ref(),
                    captioner.as_ref(),
                    ocr.as_ref(),
                    pipeline_runner.as_ref(),
                    pipeline_judge.as_ref(),
                    pipeline_eval_sample_rate,
                    notifier.as_ref(),
                    telegram_admin_chat_id.as_deref(),
                    &sleep_backend,
                    &cli_agents,
                )
                .await
            }
            Ok(None) => {}
            Err(e) => eprintln!("lifeos-drain: claim failed: {e}"),
        }
        // Untrusted nodes (LIFEOS_BUILD_PIPELINE=0) never claim self-extension
        // build requests - codegen stays on the trusted Mac (issue #141).
        if build_pipeline {
            match claim_next_module_request(&conn, now_secs()).await {
                Ok(Some(req)) => {
                    println!("lifeos-drain: building module request {} ({})", req.id, req.prompt);
                    run_module_build(&conn, &builder, notifier.as_ref(), req, now_secs()).await;
                }
                Ok(None) => {}
                Err(e) => eprintln!("lifeos-drain: module_request claim failed: {e}"),
            }
        }
        match reap_stuck(&conn, now_secs(), cfg).await {
            Ok(n) if n > 0 => println!("lifeos-drain: reaped {n} stuck job(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("lifeos-drain: reaper failed: {e}"),
        }
        match lifeos_actions::run_action_engine_tick(&conn, now_secs()).await {
            Ok(n) if n > 0 => println!("lifeos-drain: actions engine fired {n} job(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("lifeos-drain: actions engine tick failed: {e}"),
        }
        // Memory consolidation trigger (issue #115): idle tick + backlog
        // threshold (LIFEOS_MEMORY_SLEEP_THRESHOLD, default 25 events).
        match lifeos_drain::maybe_enqueue_memory_sleep(&conn, memory_sleep_threshold, now_secs()).await {
            Ok(n) if n > 0 => println!("lifeos-drain: enqueued {n} memory_sleep job(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("lifeos-drain: memory sleep trigger failed: {e}"),
        }
        sleep(poll).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    conn: &libsql::Connection,
    job: &lifeos_drain::ClaimedJob,
    worker_id: &str,
    vcs_store: &lifeos_vcs::ObjectStore,
    embedder: &dyn Embedder,
    transcriber: &dyn Transcriber,
    captioner: &dyn Captioner,
    ocr: &dyn Ocr,
    pipeline_runner: &dyn PipelineStageRunner,
    pipeline_judge: &dyn Judge,
    pipeline_eval_sample_rate: f64,
    notifier: &dyn Notifier,
    telegram_admin_chat_id: Option<&str>,
    sleep_backend: &dyn lifeos_vcs::StorageBackend,
    cli_agents: &std::sync::Arc<Vec<lifeos_agents::DetectedAgent>>,
) {
    println!("lifeos-drain: claimed {} (kind={})", job.id, job.kind);
    let result = match dispatch(&job.kind) {
        Dispatch::Stub(handler) => {
            println!("lifeos-drain: {} -> {handler} (stub, no-op this phase)", job.id);
            complete_job(conn, &job.id, worker_id).await
        }
        Dispatch::Ingest => {
            let payload: IngestJobPayload = serde_json::from_str(&job.payload).unwrap_or_default();
            match lifeos_ingest::process_ingest_job(
                conn,
                vcs_store,
                embedder,
                transcriber,
                captioner,
                ocr,
                &job.workspace_id,
                payload,
                now_secs(),
            )
            .await
            {
                Ok(outcome) => {
                    println!("lifeos-drain: {} ingest -> {outcome:?}", job.id);
                    complete_job(conn, &job.id, worker_id).await
                }
                Err(e) => {
                    eprintln!("lifeos-drain: {} ingest failed: {e} - failing", job.id);
                    fail_job(conn, &job.id, worker_id).await
                }
            }
        }
        Dispatch::Pipeline => {
            let payload: PipelineJobPayload = serde_json::from_str(&job.payload).unwrap_or_default();
            match lifeos_pipelines::process_pipeline_job(
                conn,
                &job.workspace_id,
                &job.id,
                payload,
                pipeline_runner,
                pipeline_judge,
                pipeline_eval_sample_rate,
                now_secs(),
            )
            .await
            {
                Ok(outcome) => {
                    println!("lifeos-drain: {} pipeline -> {outcome:?}", job.id);
                    if let PipelineOutcome::Gated { stage, rationale } = &outcome {
                        match telegram_admin_chat_id {
                            Some(chat_id) => notify_pipeline_gated(notifier, chat_id, stage, rationale).await,
                            None => println!(
                                "lifeos-drain: {} pipeline gated at '{stage}': {rationale}",
                                job.id
                            ),
                        }
                    }
                    complete_job(conn, &job.id, worker_id).await
                }
                Err(e) => {
                    eprintln!("lifeos-drain: {} pipeline failed: {e} - failing", job.id);
                    fail_job(conn, &job.id, worker_id).await
                }
            }
        }
        Dispatch::MemorySleep => {
            // One consolidation cycle (issue #115). The chosen model goes
            // through the BLAKE3 replay cache, so a later rebuild replays
            // this cycle's summaries verbatim regardless of which model
            // produced them. `LIFEOS_MEMORY_MODEL=agent` (audit #6, see
            // `lifeos_drain::ai::AgentCliModel`'s doc comment) routes
            // through the local agent-CLI router with a heuristic fallback;
            // anything else (including unset) keeps the deterministic
            // extractive heuristic, unchanged from before.
            let now = now_secs();
            let inner_model: Box<dyn lifeos_memory::MemoryModel> =
                if std::env::var("LIFEOS_MEMORY_MODEL").as_deref() == Ok("agent") {
                    Box::new(AgentCliModel { agents: cli_agents.clone() })
                } else {
                    Box::new(lifeos_memory::HeuristicModel)
                };
            let model = lifeos_memory::ReplayCachedModel::new(inner_model.as_ref(), conn, now);
            match lifeos_memory::run_sleep_cycle(
                conn,
                &job.workspace_id,
                &model,
                &lifeos_memory::HeuristicPolicyLearner,
                &lifeos_memory::HeuristicSummarizer,
                now,
                Some(sleep_backend),
            )
            .await
            {
                Ok(report) => {
                    println!("lifeos-drain: {} memory_sleep -> {report:?}", job.id);
                    complete_job(conn, &job.id, worker_id).await
                }
                Err(e) => {
                    eprintln!("lifeos-drain: {} memory_sleep failed: {e} - failing", job.id);
                    fail_job(conn, &job.id, worker_id).await
                }
            }
        }
        Dispatch::Unknown => {
            eprintln!("lifeos-drain: unknown kind '{}' for {} - failing", job.kind, job.id);
            fail_job(conn, &job.id, worker_id).await
        }
    };
    match result {
        // 0 rows = this worker no longer holds the lease (reaped + re-claimed
        // while it was working). Don't overwrite the new owner's claim.
        Ok(0) => eprintln!(
            "lifeos-drain: lease lost for {} (reaped + re-claimed); skipping status write",
            job.id
        ),
        Ok(_) => {}
        Err(e) => eprintln!("lifeos-drain: status update for {} failed: {e}", job.id),
    }
}
