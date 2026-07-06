//! `/api/configs/:id/shadow` + `/api/configs/:id/promote` HTTP tests
//! (finding 15 - cross-tenant IDOR).
//!
//! Both routes used to load a config by bare id without ever comparing its
//! `workspace_id` to the caller's resolved workspace. `enforce_role` gates
//! promote/rollback as owner-only writes, but that is a role check, not a
//! per-row tenancy check - an authenticated owner of workspace A could
//! shadow/promote workspace B's config given a leaked or guessed id. Both
//! routes now resolve the caller's workspace and scope every SELECT/UPDATE
//! by it, returning 404 (not 403) on a cross-tenant id - the same shape
//! `approval::read_entity_scoped` already uses.

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
        .join(format!("lifeos_cfg_{}.db", new_id("t")))
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
        trust_workspace_header: true,
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
    TestApp { router: routes::router(state), db_path }
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    send_with_header(app, method, uri, body, None).await
}

/// Like `send`, but can attach one extra header - used to scope body-less
/// requests (like promote) to a workspace via `X-Workspace-Id`.
async fn send_with_header(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    header: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some((name, value)) = header {
        builder = builder.header(name, value);
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

async fn register(app: &Router, name: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

async fn create_draft_config(app: &Router, ws: &str, kind: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/configs",
        Some(json!({"kind": kind, "payload": {"x": 1}, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create_draft_config: {body:?}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn shadow_same_workspace_succeeds() {
    let ta = test_app().await;
    let ws = register(&ta.router, "cfg-shadow-same").await;
    let id = create_draft_config(&ta.router, &ws, "kind-a").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        &format!("/api/configs/{id}/shadow"),
        Some(json!({"shadow_summary": {"ok": true}, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "shadow");
}

#[tokio::test]
async fn shadow_cross_tenant_is_404_and_leaves_config_in_draft() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "cfg-shadow-iso-a").await;
    let ws_b = register(&ta.router, "cfg-shadow-iso-b").await;
    let id = create_draft_config(&ta.router, &ws_a, "kind-a").await;

    // B tries to shadow A's config -> 404 (invisible), not 403.
    let (st, _) = send(
        &ta.router,
        "POST",
        &format!("/api/configs/{id}/shadow"),
        Some(json!({"shadow_summary": {"ok": true}, "workspace_id": ws_b})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A's config must remain untouched in draft.
    let (_, list_a) = send(&ta.router, "GET", &format!("/api/configs?workspace_id={ws_a}"), None).await;
    let cfgs = list_a["configs"].as_array().unwrap();
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0]["status"], "draft");
    assert_eq!(cfgs[0]["shadow_summary"], Value::Null);
}

#[tokio::test]
async fn promote_same_workspace_succeeds() {
    let ta = test_app().await;
    let ws = register(&ta.router, "cfg-promote-same").await;
    let id = create_draft_config(&ta.router, &ws, "kind-a").await;

    let (st, body) =
        send_with_header(&ta.router, "POST", &format!("/api/configs/{id}/promote"), None, Some(("x-workspace-id", &ws))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "promoted");

    let (_, list) = send(&ta.router, "GET", &format!("/api/configs?workspace_id={ws}"), None).await;
    assert_eq!(list["active"]["kind-a"], id);
}

#[tokio::test]
async fn promote_cross_tenant_is_404_and_does_not_flip_active_pointer() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "cfg-promote-iso-a").await;
    let ws_b = register(&ta.router, "cfg-promote-iso-b").await;
    let id = create_draft_config(&ta.router, &ws_a, "kind-a").await;

    // B tries to promote A's config by scoping itself to workspace B -> 404.
    let (st, _) =
        send_with_header(&ta.router, "POST", &format!("/api/configs/{id}/promote"), None, Some(("x-workspace-id", &ws_b))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Neither workspace got an active pointer, and A's config is still draft.
    let (_, list_a) = send(&ta.router, "GET", &format!("/api/configs?workspace_id={ws_a}"), None).await;
    assert!(list_a["active"].as_object().unwrap().is_empty());
    assert_eq!(list_a["configs"][0]["status"], "draft");

    let (_, list_b) = send(&ta.router, "GET", &format!("/api/configs?workspace_id={ws_b}"), None).await;
    assert!(list_b["active"].as_object().unwrap().is_empty());

    // Same-workspace promote still works after the failed cross-tenant attempt.
    let (st, body) =
        send_with_header(&ta.router, "POST", &format!("/api/configs/{id}/promote"), None, Some(("x-workspace-id", &ws_a))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "promoted");
}
