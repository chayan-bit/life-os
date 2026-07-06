//! HTTP surface for `lifeos-api`. One handler module per resource group.

mod agent;
mod annotation;
mod approval;
mod browser;
mod calendar;
mod configs;
mod connection;
mod drive;
mod entity;
mod event;
mod files;
mod gmail;
mod health;
mod job;
mod kite;
mod llm;
mod login;
mod marketplace;
mod membership;
mod memory;
mod metrics;
mod module_request;
mod notion;
mod pipeline;
mod planned;
mod push;
mod reading;
mod register;
mod edge;
mod search;
mod slack;
mod storage;
mod stream;
mod travel;
mod vcs;
mod whatsapp;
mod workspace;
mod workspace_db;

use crate::auth::{bearer_claims, resolve_role, resolve_workspace};
use crate::error::ApiError;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Router,
};

/// Paths that must skip the strict-mode role gate entirely: unauthenticated
/// identity/auth endpoints (they cannot present a JWT yet), invite acceptance
/// (the joining user is not a member yet - the token is the authority), and
/// inbound webhooks (external callers, no user JWT). `/api/invite/accept` is
/// matched exactly so `/api/invite` (owner-only create) stays gated.
const ROLE_EXEMPT_PREFIXES: &[&str] = &[
    "/api/login",
    "/api/register",
    "/api/logout",
    "/api/session/refresh",
    "/api/account/set-password",
    "/api/invite/accept",
    "/api/webhooks/",
];

fn is_write_method(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE)
}

fn is_role_exempt(path: &str) -> bool {
    ROLE_EXEMPT_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// Owner-only write surfaces: config promote/rollback, owned-credential
/// connections, storage backends, and membership mutations (issue #146,
/// design decision #3). An editor/agent hitting one of these gets 403; owner
/// passes. `/api/invite/accept` is already exempt above, so it never reaches
/// this despite the `/api/invite` prefix.
fn is_security_sensitive(path: &str) -> bool {
    path.starts_with("/api/connection")
        || path.starts_with("/api/storage")
        || path.starts_with("/api/member")
        || path.starts_with("/api/invite")
        || (path.starts_with("/api/configs")
            && (path.ends_with("/promote") || path == "/api/configs/rollback"))
}

/// Per-role write enforcement (issue #146). ONE middleware for the whole API,
/// not a per-route sweep. In local-first mode (`trust_workspace_header = true`)
/// it is a pure pass-through - personal deployments are unaffected. In strict
/// mode it gates writes only (reads are free for members): a viewer is denied
/// every write, an editor/agent is denied security-sensitive writes, an owner
/// passes. Identity itself is still resolved by `resolve_workspace`.
async fn enforce_role(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.config.trust_workspace_header {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if !is_write_method(&method) || is_role_exempt(&path) {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let workspace = match resolve_workspace(&headers, &state.config, None) {
        Ok(w) => w,
        Err(e) => return e.into_response(),
    };
    let user_id = bearer_claims(&headers, &state.config.jwt_secret).map(|c| c.sub);
    let role = match resolve_role(&state.conn, &workspace, user_id.as_deref()).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    if !role.can_write(is_security_sensitive(&path)) {
        return ApiError::Forbidden(format!(
            "role '{}' is not permitted to perform this write",
            role.as_str()
        ))
        .into_response();
    }
    next.run(req).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // --- liveness + identity ---
        .route("/api/health", get(health::health))
        .route("/api/register", post(register::register))
        // --- real login/session (issue #100, docs/SECURITY.md §5) ---
        .route("/api/login", post(login::login))
        .route("/api/session/refresh", post(login::refresh))
        .route("/api/logout", post(login::logout))
        .route("/api/account/set-password", post(login::set_password))
        .route("/api/me", get(workspace::me))
        .route("/api/workspace", get(workspace::get_workspace).patch(workspace::update_workspace))
        // --- generic entity CRUD (the spine the whole system rests on) ---
        .route("/api/entity", post(entity::create).get(entity::list))
        .route("/api/entity/:id", get(entity::get_one).patch(entity::update))
        // --- repair a forced sync conflict by replaying events (docs/DATA-MODEL.md §4.2) ---
        .route("/api/entity/:id/reconcile", post(entity::reconcile))
        // --- approval inbox (issue #142): the human-JWT surface for the gating
        //     state machine. List pending, then approve (CAS + execute_approval
        //     job; T5 gates need typed confirm) or deny (CAS, no job). ---
        .route("/api/approvals", get(approval::list))
        .route("/api/approval/:id/approve", post(approval::approve))
        .route("/api/approval/:id/deny", post(approval::deny))
        // --- reader/annotation layer: workspace-scoped notes/highlights/
        //     questions on entities (docs/DATA-MODEL.md §2.4). Plain mutable
        //     CRUD - not append-only - each write still logs an event. ---
        .route("/api/annotation", post(annotation::create).get(annotation::list))
        .route(
            "/api/annotation/:id",
            patch(annotation::update).delete(annotation::delete),
        )
        // --- graph edges ---
        .route("/api/edge", post(edge::create).get(edge::list))
        .route("/api/edge/:id", patch(edge::update))
        // --- events: append-only. Only POST (append) + GET (read) are wired;
        //     PUT/PATCH/DELETE resolve to 405 because no route defines them. ---
        .route("/api/event", post(event::create).get(event::list))
        // --- job queue (read for the UI, enqueue for producers) ---
        .route("/api/jobs", get(job::list))
        .route("/api/job", post(job::create))
        // --- hybrid recall: FTS5 (+ best-effort vectors) over the derived DB ---
        .route("/api/search", get(search::search))
        // --- cognitive memory (issues #111-#119, docs/AI-MEMORY.md): recall/
        //     context/consolidation over the event-sourced read models. Every
        //     recall is itself a ledger event; /inspect renders that ledger. ---
        .route("/api/memory/recall", post(memory::recall_handler))
        .route("/api/memory/context", post(memory::context_handler))
        .route("/api/memory/ingest", post(memory::ingest_handler))
        .route("/api/memory/sleep", post(memory::sleep_handler))
        .route("/api/memory/rebuild", post(memory::rebuild_handler))
        .route("/api/memory/tier", post(memory::tier_handler))
        .route("/api/memory/rules", get(memory::rules_handler))
        .route("/api/memory/inspect", get(memory::inspect_handler))
        // --- GraphRAG global queries (issue #139, docs/AGENT-CORE.md §13):
        //     community map + thematic map-reduce-lite over it ---
        .route("/api/memory/network", get(memory::network_handler))
        .route("/api/memory/network/ask", post(memory::network_ask_handler))
        // --- dashboards: pure SQL aggregation over events ---
        .route("/api/metrics", get(metrics::metrics))
        // --- self-extension intake + lifecycle polling (issue #76) ---
        .route("/api/module-request", post(module_request::create))
        .route("/api/module-request/:id", get(module_request::get_one))
        // --- owned-credential connect/disconnect (issue #47) ---
        .route("/api/connections", get(connection::list))
        .route("/api/connections/session", post(connection::start_session))
        .route("/api/connections/complete", post(connection::complete))
        .route("/api/connections/:id", axum::routing::delete(connection::disconnect))
        // --- Kite Connect: native custom connector, read-only (issue #51) ---
        .route("/api/connections/kite/login-url", get(kite::login_url_handler))
        .route("/api/connections/kite/complete", post(kite::complete))
        // --- WhatsApp via self-hosted GOWA: QR pairing, no send route (issue #52) ---
        .route("/api/connections/whatsapp/session", post(whatsapp::start_session))
        .route("/api/connections/whatsapp/qr", get(whatsapp::qr))
        .route("/api/connections/whatsapp/status", get(whatsapp::status))
        .route("/api/webhooks/whatsapp", post(whatsapp::webhook))
        .route("/api/whatsapp/send", post(whatsapp::send))
        // --- per-provider Nango proxy thin tools (issue #53): reads proxy
        //     straight through, writes only ever draft (docs/SECURITY.md §2) ---
        .route("/api/gmail/list", get(gmail::list))
        .route("/api/gmail/send", post(gmail::send))
        // --- Email module: materialize Gmail messages as entities (issue #56) ---
        .route("/api/gmail/sync", post(gmail::sync))
        .route("/api/calendar/list", get(calendar::list))
        .route("/api/calendar/create", post(calendar::create))
        .route("/api/calendar/move", post(calendar::move_event))
        // --- Calendar module: materialize Calendar events as entities (issue #57) ---
        .route("/api/calendar/sync", post(calendar::sync))
        .route("/api/drive/list", get(drive::list))
        .route("/api/drive/upload", post(drive::upload))
        .route("/api/drive/share", post(drive::share))
        // --- Files module: materialize Drive files + local version-history
        //     commits (issue #58) ---
        .route("/api/drive/sync", post(drive::sync))
        .route("/api/files/commit", post(files::commit))
        // --- Generic lifeos-vcs CLI surface (issue #86): commit/history/checkout,
        //     the first real byte-persisting callers of the CAS + commit model ---
        .route("/api/vcs/commit", post(vcs::commit))
        .route("/api/vcs/history", get(vcs::history))
        .route("/api/vcs/checkout", get(vcs::checkout))
        // --- TimeTravel frontend surface (issue #87): per-type diff + read/
        //     forward-only branch/tag/snapshot ---
        .route("/api/vcs/diff", get(vcs::diff))
        .route("/api/vcs/refs", get(vcs::list_refs))
        .route("/api/vcs/branch", post(vcs::create_branch))
        .route("/api/vcs/tag", post(vcs::create_tag))
        .route("/api/vcs/snapshot", get(vcs::read_snapshot))
        .route("/api/vcs/blob", get(vcs::blob))
        // --- per-workspace storage backends (issue #107): reads free,
        //     add/switch gated (docs/STORAGE-BACKENDS.md §4); migration is a
        //     resumable job that flips the primary pointer (issue #108) ---
        .route("/api/storage/backends", get(storage::list).post(storage::create))
        .route("/api/storage/migrate", post(storage::migrate))
        .route("/api/notion/list", get(notion::list))
        .route("/api/notion/create", post(notion::create))
        // --- Notion module: two-way sync in/back (issue #59) ---
        .route("/api/notion/sync", post(notion::sync))
        .route("/api/notion/push", post(notion::push))
        .route("/api/slack/list", get(slack::list))
        .route("/api/slack/post", post(slack::post))
        // --- Slack module: materialize channels/messages as entities (issue #60) ---
        .route("/api/slack/sync", post(slack::sync))
        // --- browser actuator: free read-only scrape, gated act, one
        //     interactive session-capture route (issue #54) ---
        .route("/api/browser/scrape", post(browser::scrape))
        .route("/api/browser/act", post(browser::act))
        .route("/api/connections/browser/session", post(browser::session))
        // --- Reading module: save/parse articles, capture highlights (issue #61) ---
        .route("/api/reading/save", post(reading::save))
        .route("/api/reading/highlight", post(reading::highlight))
        // --- Travel module: gated booking, free confirmation-email parsing (issue #62) ---
        .route("/api/travel/book", post(travel::book))
        .route("/api/travel/parse-emails", post(travel::parse_emails))
        // --- SSE: module lifecycle events for hot-reload tabs (no polling) ---
        .route("/api/stream/modules", get(stream::modules))
        // --- local agent router (OpenDesign-style) ---
        .route("/api/agents", get(llm::agents))
        .route("/api/llm", post(llm::llm))
        // --- agent core loop (#122) ---
        .route("/api/agent", post(agent::agent))
        // --- planned routes: enqueue where it makes sense, honest 501 otherwise ---
        .route("/api/ingest", post(planned::ingest))
        .route("/api/pipeline/run", post(planned::pipeline_run))
        // --- pipeline DAG introspection (issue #94) - static registry, no tenant scoping ---
        .route("/api/pipeline/registry", get(pipeline::registry))
        // --- read-only broker positions proxy (issue #51) - no order route exists ---
        .route("/api/broker/positions", get(kite::positions))
        // --- Release-loop candidate configs (issue #98): draft -> shadow ->
        //     promote|rollback, human-gated (docs/HARNESS-LOOP.md §4) ---
        .route("/api/configs", post(configs::create).get(configs::list))
        .route("/api/configs/:id/shadow", post(configs::shadow))
        .route("/api/configs/:id/promote", post(configs::promote))
        .route("/api/configs/rollback", post(configs::rollback))
        // --- module marketplace: publish/sign/install (issues #101/#102) ---
        .route("/api/marketplace/pubkey", get(marketplace::pubkey))
        .route("/api/marketplace/publish", post(marketplace::publish))
        .route("/api/marketplace/packages", get(marketplace::list))
        .route("/api/marketplace/verify", post(marketplace::verify))
        .route("/api/marketplace/install", post(marketplace::install))
        // --- PWA Web Push subscriptions (issue #103) ---
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        .route("/api/push/vapid-public-key", get(push::vapid_public_key))
        // --- database-per-workspace provisioning (issue #104) ---
        .route("/api/workspace/provision-db", post(workspace_db::provision))
        .route("/api/workspace/database", get(workspace_db::get_database))
        // --- workspace membership, roles, invites (issue #146) ---
        .route("/api/members", get(membership::list_members))
        .route("/api/member/:user_id/role", post(membership::set_role))
        .route("/api/member/:user_id", axum::routing::delete(membership::remove_member))
        .route("/api/invites", get(membership::list_invites))
        .route("/api/invite", post(membership::create_invite))
        .route("/api/invite/accept", post(membership::accept_invite))
        .route("/api/invite/:id", axum::routing::delete(membership::revoke_invite))
        // Per-role write enforcement (strict mode only); `route_layer` runs it
        // only for matched routes, so 404s stay 404s (issue #146).
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), enforce_role))
        .with_state(state)
}
