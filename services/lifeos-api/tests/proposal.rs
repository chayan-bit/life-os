//! Proposals - the GitHub-analog on the entity graph (issue #148). A proposal
//! is itself an entity (`module='system'`, `type='proposal'`) whose `attrs`
//! carry a before/after change set. This suite locks down the whole lifecycle:
//! create (change set captured with before/after + a base version per target),
//! diff (live before vs proposed after, per attr), merge (patches applied as
//! event-sourced `entity.updated`s stamped `caused_by_event_id` -> the
//! proposal's creation event, status flipped to merged), merge conflict
//! detection (a target changed since drafting -> 409 needs_rebase, nothing
//! applied), reject, role gating (viewer/agent cannot merge), and workspace
//! isolation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::auth::issue_token;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &str = "test-secret";

struct TestApp {
    router: Router,
    db_path: String,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

fn base_config(db_path: &str, trust_workspace_header: bool) -> Config {
    Config {
        db_path: db_path.to_string(),
        turso_url: None,
        turso_token: None,
        sync_interval_secs: 60,
        derived_db_path: format!("{db_path}.derived"),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        jwt_secret: SECRET.into(),
        trust_workspace_header,
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
        vcs_blob_root: format!("{db_path}.blobs"),
        marketplace_signing_key: None,
        turso_platform_api_token: None,
        turso_org_slug: None,
    }
}

async fn test_app(trust_workspace_header: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_prop_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path, trust_workspace_header))
        .await
        .expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

async fn send_h(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    hdrs: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in hdrs {
        builder = builder.header(*k, *v);
    }
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    send_h(app, method, uri, body, &[]).await
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn register(app: &Router, name: &str) -> (String, String) {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        Some(json!({
            "email": format!("{name}@test.example"),
            "name": name,
            "password": "test-password-123",
            "workspace_name": name,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    (
        body["workspace_id"].as_str().unwrap().to_string(),
        body["user_id"].as_str().unwrap().to_string(),
    )
}

/// Create a task entity with the given attrs and return its id.
async fn create_entity(app: &Router, ws: &str, attrs: Value) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/entity",
        Some(json!({ "workspace_id": ws, "module": "tasks", "type": "task", "title": "t", "attrs": attrs })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create_entity: {body:?}");
    body["id"].as_str().unwrap().to_string()
}

/// Create a proposal patching one entity, return its id + full body.
async fn create_proposal(app: &Router, ws: &str, entity_id: &str, patch: Value) -> (String, Value) {
    let (st, body) = send(
        app,
        "POST",
        "/api/proposal",
        Some(json!({
            "workspace_id": ws,
            "title": "Bump the task",
            "base_ref": "main",
            "reviewers": ["usr_reviewer"],
            "changes": [ { "entity_id": entity_id, "patch": patch } ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create_proposal: {body:?}");
    (body["id"].as_str().unwrap().to_string(), body)
}

/// Open the underlying db directly to assert on columns the read models omit
/// (here: `events.caused_by_event_id`).
async fn raw_conn(db_path: &str) -> libsql::Connection {
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    raw.connect().unwrap()
}

#[tokio::test]
async fn create_captures_before_after_and_open_status() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-create").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;

    let (_id, body) = create_proposal(
        &ta.router,
        &ws,
        &ent,
        json!({ "priority": "high", "eta": "friday" }),
    )
    .await;

    assert!(body["id"].as_str().unwrap().starts_with("prop_"));
    assert_eq!(body["attrs"]["status"], "open");
    assert_eq!(body["attrs"]["title"], "Bump the task");
    assert_eq!(body["attrs"]["base_ref"], "main");
    let change = &body["attrs"]["changes"][0];
    assert_eq!(change["entity_id"], ent);
    // before = the live value at draft time; after = the proposed value.
    assert_eq!(change["attr_patch"]["priority"]["before"], "low");
    assert_eq!(change["attr_patch"]["priority"]["after"], "high");
    // A newly-introduced attr has a null "before".
    assert_eq!(change["attr_patch"]["eta"]["before"], Value::Null);
    assert_eq!(change["attr_patch"]["eta"]["after"], "friday");
    // A base version is captured so merge can detect drift.
    assert!(change["base_version"].is_string());
}

#[tokio::test]
async fn create_rejects_empty_changes_and_unknown_target() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-badreq").await;

    // No changes.
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/proposal",
        Some(json!({ "workspace_id": ws, "title": "empty", "changes": [] })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Change targeting a non-existent entity.
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/proposal",
        Some(json!({
            "workspace_id": ws,
            "title": "ghost",
            "changes": [ { "entity_id": "ent_ghost", "patch": { "x": 1 } } ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(body["error"].as_str().unwrap().contains("ent_ghost"));
}

#[tokio::test]
async fn diff_shows_live_before_and_proposed_after() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-diff").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws, &ent, json!({ "priority": "high" })).await;

    let (st, diff) = send(
        &ta.router,
        "GET",
        &format!("/api/proposal/{id}/diff?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{diff:?}");
    assert_eq!(diff["status"], "open");
    assert_eq!(diff["needs_rebase"], false);
    let e0 = &diff["entities"][0];
    assert_eq!(e0["entity_id"], ent);
    assert_eq!(e0["exists"], true);
    assert_eq!(e0["conflict"], false);
    let a0 = &e0["attrs"][0];
    assert_eq!(a0["attr"], "priority");
    assert_eq!(a0["before"], "low");
    assert_eq!(a0["after"], "high");
    assert_eq!(a0["changed"], true);
}

#[tokio::test]
async fn merge_applies_patches_links_cause_and_flips_status() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-merge").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(
        &ta.router,
        &ws,
        &ent,
        json!({ "priority": "high", "stage": "review" }),
    )
    .await;

    let (st, body) = send(
        &ta.router,
        "POST",
        &format!("/api/proposal/{id}/merge"),
        Some(json!({ "workspace_id": ws })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "merged");
    assert_eq!(body["applied"], 1);

    // The target entity now carries the proposed attrs. (The entity route
    // resolves its workspace from the header, not a query param.)
    let (st, ent_body) = send_h(&ta.router, "GET", &format!("/api/entity/{ent}"), None, &[("x-workspace-id", &ws)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ent_body["attrs"]["priority"], "high");
    assert_eq!(ent_body["attrs"]["stage"], "review");

    // The proposal itself is now merged.
    let (_st, prop) = send(&ta.router, "GET", &format!("/api/proposal/{id}?workspace_id={ws}"), None).await;
    assert_eq!(prop["attrs"]["status"], "merged");

    // The applied change is a real event-sourced entity.updated, stamped with a
    // caused_by pointer to the proposal's creation event (fully reversible +
    // auditable). COLS_EVENT omits caused_by_event_id, so read it directly.
    let conn = raw_conn(&ta.db_path).await;
    let mut rows = conn
        .query(
            "SELECT id FROM events WHERE workspace_id = ?1 AND entity_id = ?2 AND type = 'proposal.created'",
            libsql::params![ws.clone(), id.clone()],
        )
        .await
        .unwrap();
    let created_evt: String = rows.next().await.unwrap().unwrap().get(0).unwrap();

    let mut rows = conn
        .query(
            "SELECT caused_by_event_id FROM events WHERE workspace_id = ?1 AND entity_id = ?2 \
             AND type = 'entity.updated' ORDER BY id DESC LIMIT 1",
            libsql::params![ws.clone(), ent.clone()],
        )
        .await
        .unwrap();
    let caused_by: Option<String> = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(caused_by.as_deref(), Some(created_evt.as_str()), "merge event must link to the proposal");
}

#[tokio::test]
async fn merge_twice_is_a_conflict() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-merge-twice").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws, &ent, json!({ "priority": "high" })).await;

    let (st, _) = send(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({ "workspace_id": ws }))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = send(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({ "workspace_id": ws }))).await;
    assert_eq!(st, StatusCode::CONFLICT, "a merged proposal cannot merge again: {body:?}");
}

#[tokio::test]
async fn merge_detects_a_stale_base_and_refuses_with_needs_rebase() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-conflict").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws, &ent, json!({ "priority": "high" })).await;

    // Someone else edits the target after the proposal was drafted (this appends
    // an entity.updated event, so the target's base version drifts).
    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/entity/{ent}"),
        Some(json!({ "workspace_id": ws, "status": "blocked" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Merge must now refuse.
    let (st, body) = send(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({ "workspace_id": ws }))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
    assert!(body["error"].as_str().unwrap().to_lowercase().contains("rebase"));

    // Nothing was applied: the proposed priority is NOT on the entity, and the
    // proposal is flagged needs_rebase but still open (not merged).
    let (st, ent_body) = send_h(&ta.router, "GET", &format!("/api/entity/{ent}"), None, &[("x-workspace-id", &ws)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(ent_body["attrs"]["priority"], "high");
    let (_st, prop) = send(&ta.router, "GET", &format!("/api/proposal/{id}?workspace_id={ws}"), None).await;
    assert_eq!(prop["attrs"]["status"], "open");
    assert_eq!(prop["attrs"]["needs_rebase"], true);

    // The diff view also reports the conflict.
    let (_st, diff) = send(&ta.router, "GET", &format!("/api/proposal/{id}/diff?workspace_id={ws}"), None).await;
    assert_eq!(diff["needs_rebase"], true);
    assert_eq!(diff["entities"][0]["conflict"], true);
}

#[tokio::test]
async fn reject_flips_status_and_blocks_a_later_merge() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-reject").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws, &ent, json!({ "priority": "high" })).await;

    let (st, body) = send(&ta.router, "POST", &format!("/api/proposal/{id}/reject"), Some(json!({ "workspace_id": ws }))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "rejected");

    let (_st, prop) = send(&ta.router, "GET", &format!("/api/proposal/{id}?workspace_id={ws}"), None).await;
    assert_eq!(prop["attrs"]["status"], "rejected");

    // A rejected proposal cannot be merged.
    let (st, _) = send(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({ "workspace_id": ws }))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    // The entity is untouched.
    let (st, ent_body) = send_h(&ta.router, "GET", &format!("/api/entity/{ent}"), None, &[("x-workspace-id", &ws)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(ent_body["attrs"]["priority"], "high");
}

#[tokio::test]
async fn list_returns_workspace_proposals_only() {
    let ta = test_app(true).await;
    let (ws, _uid) = register(&ta.router, "prop-list").await;
    let ent = create_entity(&ta.router, &ws, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws, &ent, json!({ "priority": "high" })).await;

    let (st, list) = send(&ta.router, "GET", &format!("/api/proposal?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], id);
    assert_eq!(arr[0]["type"], "proposal");
    // The plain task entity is not surfaced by the proposal list.
    assert!(arr.iter().all(|p| p["id"] != json!(ent)));
}

#[tokio::test]
async fn strict_mode_only_editor_or_owner_can_merge() {
    let ta = test_app(false).await;
    let (ws, owner_id) = register(&ta.router, "prop-strict-owner").await;
    let (_w1, viewer_id) = register(&ta.router, "prop-strict-viewer").await;
    let (_w2, editor_id) = register(&ta.router, "prop-strict-editor").await;
    let (_w3, agent_id) = register(&ta.router, "prop-strict-agent").await;

    // Seat the whole role matrix in the owner's workspace.
    let conn = raw_conn(&ta.db_path).await;
    for (uid, role) in [
        (&owner_id, "owner"),
        (&viewer_id, "viewer"),
        (&editor_id, "editor"),
        (&agent_id, "agent"),
    ] {
        conn.execute(
            "INSERT INTO workspace_members (workspace_id, user_id, role, invited_by, created_at) \
             VALUES (?1, ?2, ?3, NULL, 1) ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
            libsql::params![ws.clone(), uid.clone(), role],
        )
        .await
        .unwrap();
    }
    let tok_owner = issue_token(SECRET, &owner_id, &ws, "o@test.example");
    let tok_viewer = issue_token(SECRET, &viewer_id, &ws, "v@test.example");
    let tok_editor = issue_token(SECRET, &editor_id, &ws, "e@test.example");
    let tok_agent = issue_token(SECRET, &agent_id, &ws, "a@test.example");

    // Owner sets up a target entity + a proposal.
    let (st, ent_body) = send_h(
        &ta.router,
        "POST",
        "/api/entity",
        Some(json!({ "module": "tasks", "type": "task", "title": "t", "attrs": { "priority": "low" } })),
        &[("authorization", &bearer(&tok_owner))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ent_body:?}");
    let ent = ent_body["id"].as_str().unwrap().to_string();
    let (st, prop_body) = send_h(
        &ta.router,
        "POST",
        "/api/proposal",
        Some(json!({ "title": "p", "changes": [ { "entity_id": ent, "patch": { "priority": "high" } } ] })),
        &[("authorization", &bearer(&tok_owner))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{prop_body:?}");
    let id = prop_body["id"].as_str().unwrap().to_string();

    // Viewer: blocked by the strict write middleware.
    let (st, _) = send_h(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({})), &[("authorization", &bearer(&tok_viewer))]).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "viewer cannot merge");

    // Agent: passes the middleware (ordinary write) but the handler's editor+
    // gate denies it - merging is a review-authority action.
    let (st, _) = send_h(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({})), &[("authorization", &bearer(&tok_agent))]).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "agent cannot merge");

    // Proposal is still open after the two denied attempts.
    let (_st, prop) = send_h(&ta.router, "GET", &format!("/api/proposal/{id}"), None, &[("authorization", &bearer(&tok_owner))]).await;
    assert_eq!(prop["attrs"]["status"], "open");

    // Editor: allowed.
    let (st, body) = send_h(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({})), &[("authorization", &bearer(&tok_editor))]).await;
    assert_eq!(st, StatusCode::OK, "editor can merge: {body:?}");
    assert_eq!(body["status"], "merged");
}

#[tokio::test]
async fn proposals_are_workspace_isolated() {
    let ta = test_app(true).await;
    let (ws_a, _ua) = register(&ta.router, "prop-iso-a").await;
    let (ws_b, _ub) = register(&ta.router, "prop-iso-b").await;
    let ent = create_entity(&ta.router, &ws_a, json!({ "priority": "low" })).await;
    let (id, _) = create_proposal(&ta.router, &ws_a, &ent, json!({ "priority": "high" })).await;

    // B cannot see A's proposal in its list.
    let (st, list) = send(&ta.router, "GET", &format!("/api/proposal?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|p| p["id"] != json!(id)), "A's proposal leaked to B");

    // B cannot fetch or merge A's proposal.
    let (st, _) = send(&ta.router, "GET", &format!("/api/proposal/{id}?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = send(&ta.router, "POST", &format!("/api/proposal/{id}/merge"), Some(json!({ "workspace_id": ws_b }))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A's proposal is untouched and still open.
    let (_st, prop) = send(&ta.router, "GET", &format!("/api/proposal/{id}?workspace_id={ws_a}"), None).await;
    assert_eq!(prop["attrs"]["status"], "open");
}
