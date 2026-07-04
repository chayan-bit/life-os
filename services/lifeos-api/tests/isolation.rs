//! Multi-tenant isolation verification suite (issue #5). A must-pass security
//! gate: one workspace must never read or mutate another's entities/edges/events,
//! and the workspace-resolution precedence (JWT > X-Workspace-Id > param > default)
//! must hold end-to-end over HTTP.
//!
//! Note on scoping reads: `GET /api/entity/:id` resolves the tenant from the
//! verified JWT or the `X-Workspace-Id` header only (never a query param), so the
//! cross-tenant read tests scope the caller via that header.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

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

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_isol_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let config = Config {
        db_path: db_path.clone(),
        turso_url: None,
        turso_token: None,
        sync_interval_secs: 60,
        derived_db_path: format!("{db_path}.derived"),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        jwt_secret: "test-secret".into(),
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
    };
    let state = build_state(config).await.expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

/// Send a request with optional JSON body and optional header pairs.
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

/// Register a fresh workspace; returns (workspace_id, key_token).
async fn register(app: &Router, name: &str) -> (String, String) {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    (
        body["workspace_id"].as_str().unwrap().to_string(),
        body["key_token"].as_str().unwrap().to_string(),
    )
}

/// Create an entity scoped to `ws` (via body param); returns its id.
async fn create_entity(app: &Router, ws: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": ws, "module": "tasks", "type": "task", "title": "isolation-test"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create_entity: {body:?}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn workspace_b_cannot_read_workspace_a_entity() {
    let ta = test_app().await;
    let (ws_a, _) = register(&ta.router, "tenant-a-read").await;
    let (ws_b, _) = register(&ta.router, "tenant-b-read").await;
    let ent = create_entity(&ta.router, &ws_a).await;

    // B (scoped via X-Workspace-Id header) cannot GET A's entity by id.
    let (st, _) =
        send_h(&ta.router, "GET", &format!("/api/entity/{ent}"), None, &[("x-workspace-id", &ws_b)]).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "B must not read A's entity by id");

    // B's list does not contain A's entity.
    let (st, list) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|e| e["id"] != ent), "A's entity leaked into B's list");
}

#[tokio::test]
async fn workspace_b_cannot_patch_workspace_a_entity() {
    let ta = test_app().await;
    let (ws_a, _) = register(&ta.router, "tenant-a-patch").await;
    let (ws_b, _) = register(&ta.router, "tenant-b-patch").await;
    let ent = create_entity(&ta.router, &ws_a).await;

    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/entity/{ent}"),
        Some(json!({"workspace_id": ws_b, "title": "hacked"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "B's PATCH of A's entity -> 404");

    // A re-reads (scoped via header): title unchanged.
    let (st, body) =
        send_h(&ta.router, "GET", &format!("/api/entity/{ent}"), None, &[("x-workspace-id", &ws_a)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["title"], "isolation-test", "A's entity was mutated by B");
}

#[tokio::test]
async fn edge_isolation_between_workspaces() {
    let ta = test_app().await;
    let (ws_a, _) = register(&ta.router, "tenant-a-edge").await;
    let (ws_b, _) = register(&ta.router, "tenant-b-edge").await;
    let src = create_entity(&ta.router, &ws_a).await;
    let dst = create_entity(&ta.router, &ws_a).await;

    let (st, edge) = send(
        &ta.router,
        "POST",
        "/api/edge",
        Some(json!({"workspace_id": ws_a, "src_id": src, "dst_id": dst, "rel": "blocks"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let edge_id = edge["id"].as_str().unwrap().to_string();

    // B's edge list excludes A's edge.
    let (st, list) = send(&ta.router, "GET", &format!("/api/edge?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|e| e["id"] != edge_id), "A's edge leaked to B");

    // B cannot PATCH A's edge.
    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/edge/{edge_id}"),
        Some(json!({"workspace_id": ws_b, "state": "accepted"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "B's PATCH of A's edge -> 404");
}

#[tokio::test]
async fn event_isolation_between_workspaces() {
    let ta = test_app().await;
    let (ws_a, _) = register(&ta.router, "tenant-a-event").await;
    let (ws_b, _) = register(&ta.router, "tenant-b-event").await;

    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/event",
        Some(json!({"workspace_id": ws_a, "type": "isolation.test", "actor": "user"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // B sees none of A's events.
    let (st, list) = send(&ta.router, "GET", &format!("/api/event?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0, "A's event leaked into B's list");
}

#[tokio::test]
async fn jwt_claim_overrides_body_workspace_param() {
    let ta = test_app().await;
    let (ws_b, token_b) = register(&ta.router, "tenant-b-jwt").await;
    let ws_a = "default-personal-workspace"; // seeded, real workspace

    // B's JWT but body claims ws_a -> JWT wins, entity lands in B.
    let bearer = format!("Bearer {token_b}");
    let (st, body) = send_h(
        &ta.router,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": ws_a, "module": "tasks", "type": "task", "title": "jwt"})),
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create with B token: {body:?}");
    let ent = body["id"].as_str().unwrap().to_string();

    // B sees it.
    let (_, b_list) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}"), None).await;
    assert!(b_list.as_array().unwrap().iter().any(|e| e["id"] == ent), "entity not in B despite B's JWT");

    // A does not.
    let (_, a_list) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_a}"), None).await;
    assert!(
        a_list.as_array().unwrap().iter().all(|e| e["id"] != ent),
        "entity leaked into A: body workspace_id was honored over the JWT claim"
    );
}

#[tokio::test]
async fn cross_tenant_id_guess_always_404() {
    let ta = test_app().await;
    let (ws_a, _) = register(&ta.router, "tenant-a-fuzz").await;
    let (ws_b, _) = register(&ta.router, "tenant-b-fuzz").await;
    let real = create_entity(&ta.router, &ws_a).await;

    let guesses = [
        "ent_0000".to_string(),
        "ent_deadbeef".to_string(),
        "00000000-0000-0000-0000-000000000000".to_string(),
        real, // a real id from A, queried as B
    ];
    for g in &guesses {
        let (st, _) =
            send_h(&ta.router, "GET", &format!("/api/entity/{g}"), None, &[("x-workspace-id", &ws_b)]).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "guessed id '{g}' must 404 for B");
    }
}

#[tokio::test]
async fn unknown_workspace_create_returns_400() {
    let ta = test_app().await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": "ws-does-not-exist", "module": "tasks", "type": "task"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("workspace"));
}

#[tokio::test]
async fn unknown_workspace_list_returns_empty() {
    let ta = test_app().await;
    let (st, body) =
        send(&ta.router, "GET", "/api/entity?workspace_id=ghost-workspace-xyz", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}
