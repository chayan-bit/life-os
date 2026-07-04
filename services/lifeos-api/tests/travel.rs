//! `POST /api/travel/book` and `/parse-emails` HTTP-level tests (issue #62,
//! docs/MODULES.md §3.7). No client to mock - `book` only ever drafts,
//! `parse_emails` only reads already-synced `email` entities.

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

fn base_config(db_path: &str) -> Config {
    Config {
        db_path: db_path.to_string(),
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
    }
}

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir().join(format!("lifeos_travel_{}.db", new_id("t"))).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path)).await.expect("build state");
    TestApp { router: routes::router(state), db_path }
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

async fn seed_email(app: &Router, ws: &str, subject: &str, snippet: &str) {
    let (st, body) = send_h(
        app,
        "POST",
        "/api/entity",
        Some(json!({
            "workspace_id": ws, "module": "email", "type": "email", "title": subject,
            "attrs": { "from": "airline@example.com", "to": "me@example.com", "subject": subject, "snippet": snippet, "unread": false }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
}

#[tokio::test]
async fn book_only_ever_drafts_never_calls_a_client() {
    let ta = test_app().await;
    let ws = register(&ta.router, "travel-book").await;

    let (st, entity) = send_h(
        &ta.router,
        "POST",
        "/api/travel/book",
        Some(json!({"trip_id": "trip_1", "provider": "united", "item": "Flight UA123", "cost": 450.0, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{entity:?}");
    assert_eq!(entity["module"], "integrations");
    assert_eq!(entity["type"], "travel_book");
    assert_eq!(entity["status"], "pending_approval");
    assert_eq!(entity["attrs"]["provider"], "united");

    let (st, events) = send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=travel.book.drafted"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn parse_emails_creates_booking_from_matching_email() {
    let ta = test_app().await;
    let ws = register(&ta.router, "travel-parse").await;
    seed_email(&ta.router, &ws, "Your flight confirmation ABC123", "Your itinerary for flight UA123 is attached.").await;

    let (st, result) = send_h(&ta.router, "POST", "/api/travel/parse-emails", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{result:?}");
    assert_eq!(result["created"], 1);

    let (st, bookings) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=travel&type=booking"), None).await;
    assert_eq!(st, StatusCode::OK);
    let bookings = bookings.as_array().unwrap();
    assert_eq!(bookings.len(), 1);
    assert_eq!(bookings[0]["attrs"]["confirmation"], "ABC123");

    let (st, events) = send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=booking.added"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn parse_emails_ignores_non_booking_emails() {
    let ta = test_app().await;
    let ws = register(&ta.router, "travel-ignore").await;
    seed_email(&ta.router, &ws, "Team standup notes", "Here are today's notes from the meeting.").await;

    let (st, result) = send_h(&ta.router, "POST", "/api/travel/parse-emails", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{result:?}");
    assert_eq!(result["created"], 0);

    let (st, bookings) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=travel&type=booking"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(bookings.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn reparsing_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "travel-reparse").await;
    seed_email(&ta.router, &ws, "Hotel reservation confirmed HTL789", "Your hotel booking is confirmed.").await;

    let (st, first) = send_h(&ta.router, "POST", "/api/travel/parse-emails", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["created"], 1);

    let (st, second) = send_h(&ta.router, "POST", "/api/travel/parse-emails", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["created"], 0, "no duplicate booking on re-parse");
    assert_eq!(second["skipped"], 1);

    let (st, bookings) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=travel&type=booking"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(bookings.as_array().unwrap().len(), 1);

    let (st, events) = send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=booking.added"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1, "no duplicate booking.added event on re-parse");
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_bookings() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "travel-tenant-a").await;
    let ws_b = register(&ta.router, "travel-tenant-b").await;
    seed_email(&ta.router, &ws_a, "Flight confirmation XYZ999", "Your itinerary is ready.").await;

    let (st, result) = send_h(&ta.router, "POST", "/api/travel/parse-emails", Some(json!({"workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(result["created"], 1);

    let (st, bookings) = send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=travel&type=booking"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(bookings.as_array().unwrap().is_empty());
}
