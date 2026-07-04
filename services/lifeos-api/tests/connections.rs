//! `/api/connections` HTTP-level tests (issue #47) against a mock Nango
//! client - no real Nango deployment needed to verify the API surface.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::nango::mock::MockNangoClient;
use lifeos_api::{build_state_with_nango, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    nango: Arc<MockNangoClient>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

/// `with_nango = false` builds an app with no Nango client configured, to
/// exercise the NotImplemented path.
async fn test_app(with_nango: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_conn_{}.db", new_id("t")))
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
    let nango = Arc::new(MockNangoClient::new());
    let state = build_state_with_nango(config, if with_nango { Some(nango.clone()) } else { None })
        .await
        .expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
        nango,
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

#[tokio::test]
async fn without_nango_configured_routes_return_not_implemented() {
    let ta = test_app(false).await;
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/connections/session",
        Some(json!({"provider": "github"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn full_connect_flow_session_complete_list_disconnect() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "conn-flow").await;

    // 1. Start a Connect session.
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/session",
        Some(json!({"provider": "github", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["session_token"], "mock-session-token");

    // 2. Simulate the OAuth flow completing: Nango now has this connection.
    ta.nango.seed("conn_abc123", "github");
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": "conn_abc123", "provider": "github", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["nango_connection_id"], "conn_abc123");
    assert_eq!(body["provider"], "github");
    assert_eq!(body["status"], "active");
    // The raw token must never appear anywhere in the response.
    assert!(body.get("token").is_none());
    assert!(body.get("secret_enc").is_none());
    let conn_id = body["id"].as_str().unwrap().to_string();

    // 3. It shows up in the list.
    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().any(|c| c["id"] == conn_id));

    // 4. Disconnect revokes with Nango and marks the row revoked (not deleted).
    let (st, body) = send_h(
        &ta.router,
        "DELETE",
        &format!("/api/connections/{conn_id}"),
        None,
        &[("x-workspace-id", &ws)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "revoked");

    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let row = list.as_array().unwrap().iter().find(|c| c["id"] == conn_id).unwrap();
    assert_eq!(row["status"], "revoked", "revoked row is kept for audit, not deleted");
}

#[tokio::test]
async fn complete_rejects_connection_id_nango_never_saw() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "conn-unverified").await;

    // No `seed()` call - the mock Nango has never heard of this connectionId.
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": "conn_never_happened", "provider": "github", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body:?}");
}

#[tokio::test]
async fn workspace_b_cannot_see_or_disconnect_workspace_a_connection() {
    let ta = test_app(true).await;
    let (ws_a, _) = register(&ta.router, "conn-tenant-a").await;
    let (ws_b, _) = register(&ta.router, "conn-tenant-b").await;

    ta.nango.seed("conn_a_only", "github");
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": "conn_a_only", "provider": "github", "workspace_id": ws_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let conn_id = body["id"].as_str().unwrap().to_string();

    // B's list must not contain A's connection.
    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|c| c["id"] != conn_id));

    // B cannot disconnect A's connection by id.
    let (st, _) = send_h(
        &ta.router,
        "DELETE",
        &format!("/api/connections/{conn_id}"),
        None,
        &[("x-workspace-id", &ws_b)],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A can still see it, untouched.
    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws_a}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let row = list.as_array().unwrap().iter().find(|c| c["id"] == conn_id).unwrap();
    assert_eq!(row["status"], "active");
}

#[tokio::test]
async fn unknown_workspace_on_session_returns_400() {
    let ta = test_app(true).await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/session",
        Some(json!({"provider": "github", "workspace_id": "ws-does-not-exist"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("workspace"));
}
