//! `POST /api/gmail/sync` HTTP-level tests (issue #56, docs/MODULES.md
//! §3.1) against a mock Nango client - no real Gmail account needed. Covers:
//! materializing `email`/`email_thread` entities from Gmail's list+get
//! proxy responses, idempotent re-sync, and workspace isolation.

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

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_gmail_{}.db", new_id("t")))
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
    let nango = Arc::new(MockNangoClient::new());
    let state = build_state_with_nango(config, Some(nango.clone())).await.expect("build state");
    TestApp { router: routes::router(state), db_path, nango }
}

async fn send_h(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
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

async fn register(app: &Router, name: &str) -> String {
    let (st, body) = send_h(
        app,
        "POST",
        "/api/register",
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

async fn connect(app: &Router, nango: &MockNangoClient, ws: &str) {
    let connection_id = format!("conn_google-mail_{ws}");
    nango.seed(&connection_id, "google-mail");
    let (st, body) = send_h(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": "google-mail", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect: {body:?}");
}

fn seed_inbox(nango: &MockNangoClient) {
    nango.seed_proxy(
        "google-mail",
        "GET",
        "gmail/v1/users/me/messages",
        json!({ "messages": [{"id": "msg1", "threadId": "th1"}, {"id": "msg2", "threadId": "th1"}] }),
    );
    nango.seed_proxy(
        "google-mail",
        "GET",
        "gmail/v1/users/me/messages/msg1",
        json!({
            "id": "msg1", "threadId": "th1", "snippet": "hey there",
            "labelIds": ["INBOX", "UNREAD"],
            "payload": {"headers": [
                {"name": "From", "value": "alice@example.com"},
                {"name": "To", "value": "me@example.com"},
                {"name": "Subject", "value": "Project update"},
            ]},
        }),
    );
    nango.seed_proxy(
        "google-mail",
        "GET",
        "gmail/v1/users/me/messages/msg2",
        json!({
            "id": "msg2", "threadId": "th1", "snippet": "re: hey there",
            "labelIds": ["INBOX"],
            "payload": {"headers": [
                {"name": "From", "value": "bob@example.com"},
                {"name": "To", "value": "me@example.com"},
                {"name": "Subject", "value": "Re: Project update"},
            ]},
        }),
    );
}

#[tokio::test]
async fn without_nango_configured_sync_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_gmail_{}.db", new_id("t"))).to_string_lossy().to_string();
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
    let state = build_state_with_nango(config, None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/gmail/sync", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn sync_materializes_email_and_thread_entities() {
    let ta = test_app().await;
    let ws = register(&ta.router, "gmail-sync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_inbox(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/gmail/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["synced"], 2);
    assert_eq!(body["skipped"], 0);

    let (st, emails) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=email&type=email"), None).await;
    assert_eq!(st, StatusCode::OK);
    let emails = emails.as_array().unwrap();
    assert_eq!(emails.len(), 2);
    let msg1 = emails.iter().find(|e| e["attrs"]["gmail_id"] == "msg1").expect("msg1 present");
    assert_eq!(msg1["attrs"]["from"], "alice@example.com");
    assert_eq!(msg1["attrs"]["subject"], "Project update");
    assert_eq!(msg1["attrs"]["unread"], true);
    assert_eq!(msg1["status"], "now", "status (not attrs) drives the triage board's PATCH-to-move");
    assert_eq!(msg1["module"], "email");
    assert_eq!(msg1["type"], "email");

    let (st, threads) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=email&type=email_thread"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(threads.as_array().unwrap().len(), 1, "both messages share threadId th1");

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=email.received"), None).await;
    assert_eq!(st, StatusCode::OK);
    // 2 emails + 1 new thread = 3 email.received events.
    assert_eq!(events.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn resyncing_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "gmail-resync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_inbox(&ta.nango);

    let (st, first) = send_h(&ta.router, "POST", "/api/gmail/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["synced"], 2);

    let (st, second) = send_h(&ta.router, "POST", "/api/gmail/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["synced"], 0, "already-synced messages must not duplicate");
    assert_eq!(second["skipped"], 2);

    let (st, emails) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=email&type=email"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(emails.as_array().unwrap().len(), 2, "no duplicate rows after a second sync");
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_synced_emails() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "gmail-tenant-a").await;
    let ws_b = register(&ta.router, "gmail-tenant-b").await;
    connect(&ta.router, &ta.nango, &ws_a).await;
    seed_inbox(&ta.nango);

    let (st, _) = send_h(&ta.router, "POST", "/api/gmail/sync", Some(json!({"workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, emails) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=email&type=email"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(emails.as_array().unwrap().is_empty());
}
