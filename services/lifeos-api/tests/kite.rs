//! `/api/connections/kite/*` + `/api/broker/positions` HTTP-level tests
//! (issue #51) against a mock Kite client - no real Kite app/account needed.
//! Also the load-bearing proof that no order-placement route exists: every
//! test only ever calls GET/POST on the routes above, and `positions` is the
//! only Kite data route registered in `routes/mod.rs`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::config::Config;
use lifeos_api::crypto;
use lifeos_api::ids::new_id;
use lifeos_api::kite::mock::MockKiteClient;
use lifeos_api::{build_state_with_kite, routes};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    kite: Arc<MockKiteClient>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

/// `with_kite = false` builds an app with no Kite client/key configured, to
/// exercise the NotImplemented path.
async fn test_app(with_kite: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_kite_{}.db", new_id("t")))
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
        kite_api_key: with_kite.then(|| "test-api-key".to_string()),
        kite_api_secret: with_kite.then(|| "test-api-secret".to_string()),
        secret_encryption_key: with_kite.then(|| crypto::parse_key(&base64_key()).unwrap()),
        gowa_base_url: None,
        gowa_basic_auth: None,
        gowa_webhook_secret: None,
        browser_script_path: None,
    vcs_blob_root: format!("{db_path}.blobs"),
    marketplace_signing_key: None,
            turso_platform_api_token: None,
            turso_org_slug: None,
    };
    let kite = Arc::new(MockKiteClient::new());
    let state = build_state_with_kite(config, if with_kite { Some(kite.clone()) } else { None })
        .await
        .expect("build state");
    TestApp { router: routes::router(state), db_path, kite }
}

fn base64_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode([3u8; 32])
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
async fn without_kite_configured_routes_return_not_implemented() {
    let ta = test_app(false).await;
    let (st, _) = send(&ta.router, "GET", "/api/connections/kite/login-url", None).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/connections/kite/complete",
        Some(json!({"request_token": "rt"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    let (st, _) = send(&ta.router, "GET", "/api/broker/positions", None).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn full_login_complete_positions_flow() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "kite-flow").await;

    // 1. Login URL carries the configured api_key.
    let (st, body) = send(&ta.router, "GET", &format!("/api/connections/kite/login-url?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert!(body["login_url"].as_str().unwrap().contains("api_key=test-api-key"));

    // 2. Simulate Kite's daily login redirect handing us a request_token.
    ta.kite.seed_session("req-token-123", "access-token-abc", "AB1234");
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/kite/complete",
        Some(json!({"request_token": "req-token-123", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["provider"], "kite");
    assert_eq!(body["account_handle"], "AB1234");
    assert_eq!(body["status"], "active");
    // The raw access token must never appear anywhere in the response.
    assert!(body.get("secret_enc").is_none());
    assert!(!body.to_string().contains("access-token-abc"));

    // 3. Reading positions decrypts the stored token server-side and proxies it.
    ta.kite.seed_positions("access-token-abc", json!({"net": [{"tradingsymbol": "INFY", "quantity": 10}]}));
    let (st, body) = send(&ta.router, "GET", &format!("/api/broker/positions?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["net"][0]["tradingsymbol"], "INFY");
}

#[tokio::test]
async fn complete_rejects_request_token_kite_never_issued() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "kite-bad-token").await;

    // No `seed_session()` call - the mock Kite has never heard of this request_token.
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/connections/kite/complete",
        Some(json!({"request_token": "never-happened", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn positions_404_when_no_active_connection() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "kite-no-conn").await;
    let (st, _) = send(&ta.router, "GET", &format!("/api/broker/positions?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workspace_b_cannot_read_workspace_a_positions() {
    let ta = test_app(true).await;
    let (ws_a, _) = register(&ta.router, "kite-tenant-a").await;
    let (ws_b, _) = register(&ta.router, "kite-tenant-b").await;

    ta.kite.seed_session("rt-a", "token-a", "USER_A");
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/connections/kite/complete",
        Some(json!({"request_token": "rt-a", "workspace_id": ws_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    ta.kite.seed_positions("token-a", json!({"net": []}));

    // Workspace B has no Kite connection of its own - reading positions 404s,
    // it never falls back to workspace A's token.
    let (st, _) = send(&ta.router, "GET", &format!("/api/broker/positions?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn no_order_route_exists_on_the_router() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "kite-no-orders").await;
    ta.kite.seed_session("rt-order", "token-order", "USER_O");
    send(
        &ta.router,
        "POST",
        "/api/connections/kite/complete",
        Some(json!({"request_token": "rt-order", "workspace_id": ws})),
    )
    .await;

    // Trading is read-only for any agent/bot (docs/SECURITY.md §1): none of
    // these plausible order-route shapes are wired to anything.
    for path in [
        "/api/broker/orders",
        "/api/broker/order",
        "/api/broker/place_order",
        "/api/broker/gtt",
    ] {
        let (st, _) = send(&ta.router, "POST", path, Some(json!({"workspace_id": ws}))).await;
        assert!(
            st == StatusCode::NOT_FOUND || st == StatusCode::METHOD_NOT_ALLOWED,
            "expected {path} to be unrouted, got {st}"
        );
    }
}
