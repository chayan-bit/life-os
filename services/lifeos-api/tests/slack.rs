//! `POST /api/slack/sync` HTTP-level tests (issue #60, docs/MODULES.md
//! §3.5) against a mock Nango client - no real Slack workspace needed.
//! Covers: materializing channels/messages as entities, idempotent
//! re-sync, workspace isolation, and that `post` (from #53) still only ever
//! drafts.

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

fn base_config(db_path: &str) -> Config {
    Config {
        db_path: db_path.to_string(),
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
    }
}

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_slack_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let nango = Arc::new(MockNangoClient::new());
    let state = build_state_with_nango(base_config(&db_path), Some(nango.clone())).await.expect("build state");
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
    let connection_id = format!("conn_slack_{ws}");
    nango.seed(&connection_id, "slack");
    let (st, body) = send_h(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": "slack", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect: {body:?}");
}

fn seed_slack(nango: &MockNangoClient) {
    nango.seed_proxy(
        "slack",
        "GET",
        "conversations.list",
        json!({ "channels": [{"id": "C1", "name": "general"}] }),
    );
    nango.seed_proxy(
        "slack",
        "GET",
        "conversations.history",
        json!({
            "messages": [
                {"ts": "1000.001", "user": "U1", "text": "hello team"},
                {"ts": "1000.002", "user": "U2", "text": "hi there"},
            ]
        }),
    );
}

#[tokio::test]
async fn without_nango_configured_sync_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_slack_{}.db", new_id("t"))).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state_with_nango(base_config(&db_path), None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/slack/sync", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn sync_materializes_channels_and_messages() {
    let ta = test_app().await;
    let ws = register(&ta.router, "slack-sync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_slack(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/slack/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["synced"], 3, "1 channel + 2 messages");
    assert_eq!(body["skipped"], 0);

    let (st, channels) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=slack&type=channel"), None).await;
    assert_eq!(st, StatusCode::OK);
    let channels = channels.as_array().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["title"], "general");
    assert_eq!(channels[0]["attrs"]["channel_id"], "C1");

    let (st, messages) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=slack&type=message"), None).await;
    assert_eq!(st, StatusCode::OK);
    let messages = messages.as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let m1 = messages.iter().find(|m| m["attrs"]["ts"] == "1000.001").expect("m1 present");
    assert_eq!(m1["attrs"]["user"], "U1");
    assert_eq!(m1["title"], "hello team");
}

#[tokio::test]
async fn resyncing_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "slack-resync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_slack(&ta.nango);

    let (st, first) = send_h(&ta.router, "POST", "/api/slack/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["synced"], 3);

    let (st, second) = send_h(&ta.router, "POST", "/api/slack/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["synced"], 0, "already-synced channel/messages must not duplicate");
    assert_eq!(second["skipped"], 3);

    let (st, messages) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=slack&type=message"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(messages.as_array().unwrap().len(), 2, "no duplicate rows after a second sync");
}

#[tokio::test]
async fn post_only_ever_drafts_never_calls_slack() {
    let ta = test_app().await;
    let ws = register(&ta.router, "slack-post").await;
    connect(&ta.router, &ta.nango, &ws).await;

    let (st, body) =
        send_h(&ta.router, "POST", "/api/slack/post", Some(json!({"channel": "C1", "text": "hi", "workspace_id": ws})))
            .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["type"], "slack_post");

    assert!(ta.nango.calls.lock().unwrap().is_empty(), "post must never call the Slack proxy");
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_synced_messages() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "slack-tenant-a").await;
    let ws_b = register(&ta.router, "slack-tenant-b").await;
    connect(&ta.router, &ta.nango, &ws_a).await;
    seed_slack(&ta.nango);

    let (st, _) = send_h(&ta.router, "POST", "/api/slack/sync", Some(json!({"workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, messages) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=slack&type=message"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(messages.as_array().unwrap().is_empty());
}
