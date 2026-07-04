//! `/api/connections/whatsapp/*`, `/api/webhooks/whatsapp`,
//! `/api/whatsapp/send` HTTP-level tests (issue #52) against a mock GOWA
//! client - no real GOWA instance/phone needed. Also the load-bearing proof
//! that no send path exists: `whatsapp_send_never_calls_gowa` asserts the
//! mock client recorded zero calls after a send draft.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use hmac::{Hmac, Mac};
use lifeos_api::config::Config;
use lifeos_api::ids::new_id;
use lifeos_api::whatsapp::mock::MockWhatsAppClient;
use lifeos_api::{build_state_with_whatsapp, routes};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

const WEBHOOK_SECRET: &str = "test-webhook-secret-at-least-32-bytes-long";

struct TestApp {
    router: Router,
    db_path: String,
    whatsapp: Arc<MockWhatsAppClient>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

/// `with_whatsapp = false` builds an app with no GOWA client/key configured.
async fn test_app(with_whatsapp: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_wa_{}.db", new_id("t")))
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
        gowa_base_url: with_whatsapp.then(|| "http://127.0.0.1:0".to_string()),
        gowa_basic_auth: with_whatsapp.then(|| "lifeos:test-pass".to_string()),
        gowa_webhook_secret: with_whatsapp.then(|| WEBHOOK_SECRET.to_string()),
        browser_script_path: None,
    vcs_blob_root: format!("{db_path}.blobs"),
    marketplace_signing_key: None,
            turso_platform_api_token: None,
            turso_org_slug: None,
    };
    let whatsapp = Arc::new(MockWhatsAppClient::new());
    let state = build_state_with_whatsapp(config, if with_whatsapp { Some(whatsapp.clone()) } else { None })
        .await
        .expect("build state");
    TestApp { router: routes::router(state), db_path, whatsapp }
}

fn sign(body: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(WEBHOOK_SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
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

async fn post_webhook(app: &Router, payload: &str) -> StatusCode {
    let sig = sign(payload);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks/whatsapp")
                .header("content-type", "application/json")
                .header("x-hub-signature-256", sig)
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
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
async fn without_whatsapp_configured_routes_return_not_implemented() {
    let ta = test_app(false).await;
    let (st, _) = send(&ta.router, "POST", "/api/connections/whatsapp/session", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let (st, _) = send(&ta.router, "GET", "/api/connections/whatsapp/qr", None).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let (st, _) = send(&ta.router, "GET", "/api/connections/whatsapp/status", None).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let (st, _) = send(&ta.router, "POST", "/api/webhooks/whatsapp", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn session_creates_a_pending_connection_keyed_by_workspace_id() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "wa-flow").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/connections/whatsapp/session",
        Some(json!({"workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["provider"], "whatsapp");
    assert_eq!(body["account_handle"], ws);
    assert_eq!(body["status"], "pending");
    assert!(body.get("secret_enc").is_none());

    // GOWA's device_id is the workspace_id itself - no secret was minted.
    assert_eq!(ta.whatsapp.devices_snapshot(), vec![ws.clone()]);

    ta.whatsapp.seed_qr(&ws, "http://127.0.0.1:8082/statics/images/qrcode/mock.png");
    let (st, body) = send(&ta.router, "GET", &format!("/api/connections/whatsapp/qr?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["qr_link"], "http://127.0.0.1:8082/statics/images/qrcode/mock.png");
}

#[tokio::test]
async fn status_flips_connection_to_active_once_logged_in() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "wa-status").await;
    send(&ta.router, "POST", "/api/connections/whatsapp/session", Some(json!({"workspace_id": ws}))).await;

    ta.whatsapp.seed_logged_in(&ws, true);

    let (st, body) = send(&ta.router, "GET", &format!("/api/connections/whatsapp/status?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["connected"], true);
    assert_eq!(body["status"], "active");
}

#[tokio::test]
async fn webhook_rejects_missing_or_wrong_signature() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "wa-hmac").await;
    let payload = json!({"event": "message", "device_id": ws, "session_id": ws, "payload": {"body": "hi"}}).to_string();

    let (st, _) = send(&ta.router, "POST", "/api/webhooks/whatsapp", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "missing signature header must be rejected");

    let resp = ta
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/webhooks/whatsapp")
                .header("content-type", "application/json")
                .header("x-hub-signature-256", "sha256=0000")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(post_webhook(&ta.router, &payload).await, StatusCode::OK);
}

#[tokio::test]
async fn webhook_message_event_creates_an_entity_and_filters_own_echo() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "wa-inbound").await;

    let payload = json!({
        "event": "message",
        "device_id": format!("{ws}@s.whatsapp.net"),
        "session_id": ws,
        "payload": {"body": "hello from whatsapp", "from": "1555@s.whatsapp.net", "is_from_me": false}
    })
    .to_string();
    assert_eq!(post_webhook(&ta.router, &payload).await, StatusCode::OK);

    // An echo of our own outbound message (is_from_me=true) must not be captured.
    let echo = json!({
        "event": "message",
        "device_id": format!("{ws}@s.whatsapp.net"),
        "session_id": ws,
        "payload": {"body": "should not be captured", "is_from_me": true}
    })
    .to_string();
    assert_eq!(post_webhook(&ta.router, &echo).await, StatusCode::OK);

    let (st, list) = send(
        &ta.router,
        "GET",
        &format!("/api/entity?workspace_id={ws}&module=integrations&type=whatsapp_message"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list:?}");
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 1, "only the non-echo message should be captured: {items:?}");
    assert_eq!(items[0]["title"], "hello from whatsapp");
}

#[tokio::test]
async fn whatsapp_send_never_calls_gowa() {
    let ta = test_app(true).await;
    let (ws, _) = register(&ta.router, "wa-send").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/whatsapp/send",
        Some(json!({"to": "+15550001111", "message": "hi", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["type"], "whatsapp_send");

    // The load-bearing assertion: no GOWA call was ever made by /send.
    assert!(ta.whatsapp.calls.lock().unwrap().is_empty(), "send must never call gowa directly");
}

#[tokio::test]
async fn unknown_workspace_on_webhook_is_rejected() {
    let ta = test_app(true).await;
    let payload = json!({"event": "message", "device_id": "x", "session_id": "ws-does-not-exist", "payload": {}}).to_string();
    assert_eq!(post_webhook(&ta.router, &payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_missing_session_id_is_rejected() {
    let ta = test_app(true).await;
    let payload = json!({"event": "message", "device_id": "x", "payload": {}}).to_string();
    assert_eq!(post_webhook(&ta.router, &payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workspace_b_status_unaffected_by_workspace_a_login() {
    let ta = test_app(true).await;
    let (ws_a, _) = register(&ta.router, "wa-tenant-a").await;
    let (ws_b, _) = register(&ta.router, "wa-tenant-b").await;
    send(&ta.router, "POST", "/api/connections/whatsapp/session", Some(json!({"workspace_id": ws_a}))).await;
    send(&ta.router, "POST", "/api/connections/whatsapp/session", Some(json!({"workspace_id": ws_b}))).await;

    ta.whatsapp.seed_logged_in(&ws_a, true);
    // ws_b was never seeded as logged in.

    let (st, body) = send(&ta.router, "GET", &format!("/api/connections/whatsapp/status?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["connected"], false);
}
