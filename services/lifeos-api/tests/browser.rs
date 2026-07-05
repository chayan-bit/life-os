//! `/api/browser/*` + `/api/connections/browser/session` HTTP-level tests
//! (issue #54) against a mock browser actuator - no real Python/browser-use/
//! Chromium needed. Also the load-bearing proof that `act` never reaches the
//! actuator: it only ever creates a draft entity.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::browser::mock::MockBrowserActuator;
use lifeos_api::crypto;
use lifeos_api::ids::new_id;
use lifeos_api::{build_state_with_browser, config::Config, routes};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    browser: Arc<MockBrowserActuator>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

fn base64_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode([5u8; 32])
}

/// `with_browser = false` builds an app with no browser actuator configured,
/// to exercise the NotImplemented path.
async fn test_app(with_browser: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_browser_{}.db", new_id("t")))
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
        secret_encryption_key: with_browser.then(|| crypto::parse_key(&base64_key()).unwrap()),
        gowa_base_url: None,
        gowa_basic_auth: None,
        gowa_webhook_secret: None,
        browser_script_path: with_browser.then(|| "scripts/browser_actuator.py".to_string()),
    vcs_blob_root: format!("{db_path}.blobs"),
    marketplace_signing_key: None,
            turso_platform_api_token: None,
            turso_org_slug: None,
    };
    let browser = Arc::new(MockBrowserActuator::new());
    let state = build_state_with_browser(config, if with_browser { Some(browser.clone()) } else { None })
        .await
        .expect("build state");
    TestApp { router: routes::router(state), db_path, browser }
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
        Some(b) => builder.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
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

#[tokio::test]
async fn without_browser_configured_routes_return_not_implemented() {
    let ta = test_app(false).await;
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/browser/scrape",
        Some(json!({"url": "https://example.com", "task": "read the title"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    let (st, _) = send(&ta.router, "POST", "/api/connections/browser/session", Some(json!({"site": "example.com"}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn act_is_always_free_of_configuration_and_only_ever_drafts() {
    // `act` has no browser_or_501 gate at all - drafting an intent never
    // needs the actuator to be configured.
    let ta = test_app(false).await;
    let ws = register(&ta.router, "browser-act-nocfg").await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/browser/act",
        Some(json!({"task": "book a table", "site": "opentable.com", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
}

#[tokio::test]
async fn scrape_returns_the_actuators_result_verbatim() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "browser-scrape").await;
    ta.browser.seed_scrape("https://example.com", json!({"title": "Example Domain"}));

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/browser/scrape",
        Some(json!({"url": "https://example.com", "task": "read the title", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["title"], "Example Domain");
    assert_eq!(ta.browser.calls.lock().unwrap().as_slice(), ["scrape"]);
}

#[tokio::test]
async fn act_creates_a_draft_and_never_calls_the_actuator() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "browser-act").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/browser/act",
        Some(json!({"task": "cancel my subscription", "site": "example.com", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["module"], "integrations");
    assert_eq!(body["type"], "browser_act");

    let (st, events) =
        send(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=browser.act.drafted"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1);

    assert!(
        ta.browser.calls.lock().unwrap().is_empty(),
        "act must never reach the browser actuator: {:?}",
        ta.browser.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn session_capture_encrypts_and_never_returns_the_raw_session() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "browser-session").await;
    ta.browser.seed_session("example.com", "cookie=super-secret-session-value");

    let (st, body) =
        send(&ta.router, "POST", "/api/connections/browser/session", Some(json!({"site": "example.com", "workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["provider"], "browser:example.com");
    assert_eq!(body["status"], "active");
    assert!(body.get("secret_enc").is_none());
    assert!(!body.to_string().contains("super-secret-session-value"));

    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!list.to_string().contains("super-secret-session-value"));
}

#[tokio::test]
async fn workspace_b_has_no_browser_connection_of_its_own() {
    let ta = test_app(true).await;
    let ws_a = register(&ta.router, "browser-tenant-a").await;
    let ws_b = register(&ta.router, "browser-tenant-b").await;
    ta.browser.seed_session("example.com", "cookie=only-for-a");

    let (st, _) =
        send(&ta.router, "POST", "/api/connections/browser/session", Some(json!({"site": "example.com", "workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, list) = send(&ta.router, "GET", &format!("/api/connections?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|c| c["provider"] != "browser:example.com"));
}
