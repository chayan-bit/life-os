//! `POST /api/calendar/sync` and `/api/calendar/move` HTTP-level tests
//! (issue #57, docs/MODULES.md §3.2) against a mock Nango client - no real
//! Google Calendar account needed. Covers: materializing `calendar_event`
//! entities from Calendar's events.list proxy response, idempotent
//! re-sync, workspace isolation, and that `move` only ever drafts.

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
        .join(format!("lifeos_calendar_{}.db", new_id("t")))
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
    let connection_id = format!("conn_google-calendar_{ws}");
    nango.seed(&connection_id, "google-calendar");
    let (st, body) = send_h(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": "google-calendar", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect: {body:?}");
}

fn seed_events(nango: &MockNangoClient) {
    nango.seed_proxy(
        "google-calendar",
        "GET",
        "calendar/v3/calendars/primary/events",
        json!({
            "items": [
                {
                    "id": "evt1",
                    "summary": "Standup",
                    "start": {"dateTime": "2026-07-02T09:00:00+05:30"},
                    "end": {"dateTime": "2026-07-02T09:15:00+05:30"},
                    "location": "Zoom",
                    "attendees": [{"email": "alice@example.com"}, {"email": "bob@example.com"}],
                },
                {
                    "id": "evt2",
                    "summary": "Board review",
                    "start": {"dateTime": "2026-07-03T14:00:00+05:30"},
                    "end": {"dateTime": "2026-07-03T15:00:00+05:30"},
                },
            ]
        }),
    );
}

#[tokio::test]
async fn without_nango_configured_sync_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_calendar_{}.db", new_id("t"))).to_string_lossy().to_string();
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
    let state = build_state_with_nango(config, None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/calendar/sync", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn sync_materializes_calendar_event_entities() {
    let ta = test_app().await;
    let ws = register(&ta.router, "cal-sync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_events(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/calendar/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["synced"], 2);
    assert_eq!(body["skipped"], 0);

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=calendar&type=calendar_event"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    let events = events.as_array().unwrap();
    assert_eq!(events.len(), 2);
    let evt1 = events.iter().find(|e| e["attrs"]["source_uid"] == "evt1").expect("evt1 present");
    assert_eq!(evt1["attrs"]["title"], "Standup");
    assert_eq!(evt1["attrs"]["start"], "2026-07-02T09:00:00+05:30");
    assert_eq!(evt1["attrs"]["location"], "Zoom");
    assert_eq!(evt1["attrs"]["attendees"].as_array().unwrap().len(), 2);
    assert_eq!(evt1["module"], "calendar");
    assert_eq!(evt1["type"], "calendar_event");

    let (st, log) = send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=cal.synced"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(log.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn resyncing_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "cal-resync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_events(&ta.nango);

    let (st, first) = send_h(&ta.router, "POST", "/api/calendar/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["synced"], 2);

    let (st, second) = send_h(&ta.router, "POST", "/api/calendar/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["synced"], 0, "already-synced events must not duplicate");
    assert_eq!(second["skipped"], 2);

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=calendar&type=calendar_event"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 2, "no duplicate rows after a second sync");
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_synced_events() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "cal-tenant-a").await;
    let ws_b = register(&ta.router, "cal-tenant-b").await;
    connect(&ta.router, &ta.nango, &ws_a).await;
    seed_events(&ta.nango);

    let (st, _) = send_h(&ta.router, "POST", "/api/calendar/sync", Some(json!({"workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=calendar&type=calendar_event"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    assert!(events.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn move_only_ever_drafts_never_calls_calendar() {
    let ta = test_app().await;
    let ws = register(&ta.router, "cal-move").await;
    connect(&ta.router, &ta.nango, &ws).await;

    let (st, body) = send_h(
        &ta.router,
        "POST",
        "/api/calendar/move",
        Some(json!({"event_id": "evt1", "start": "2026-07-04T09:00:00+05:30", "end": "2026-07-04T09:15:00+05:30", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["type"], "calendar_move");

    let (st, drafted) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=calendar.move.drafted"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(drafted.as_array().unwrap().len(), 1);

    assert!(ta.nango.calls.lock().unwrap().is_empty(), "move must never call the Calendar proxy");
}
