//! `POST /api/notion/sync` and `/api/notion/push` HTTP-level tests (issue
//! #59, docs/MODULES.md §3.4) against a mock Nango client - no real Notion
//! workspace needed. Covers: materializing pages/databases as mirror
//! entities plus a native `note ─mirrors→ notion_page` edge, idempotent
//! re-sync, and that `push` ("edits propagate back") only ever drafts.

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
        .join(format!("lifeos_notion_{}.db", new_id("t")))
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
    let connection_id = format!("conn_notion_{ws}");
    nango.seed(&connection_id, "notion");
    let (st, body) = send_h(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": "notion", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect: {body:?}");
}

fn seed_search(nango: &MockNangoClient) {
    nango.seed_proxy(
        "notion",
        "POST",
        "v1/search",
        json!({
            "results": [
                {
                    "object": "page",
                    "id": "page1",
                    "last_edited_time": "2026-06-01T00:00:00.000Z",
                    "properties": {
                        "title": {
                            "id": "title",
                            "type": "title",
                            "title": [{"type": "text", "text": {"content": "Meeting notes"}, "plain_text": "Meeting notes"}]
                        }
                    }
                },
                {
                    "object": "database",
                    "id": "db1",
                    "title": [{"type": "text", "text": {"content": "Projects"}, "plain_text": "Projects"}]
                },
            ]
        }),
    );
}

#[tokio::test]
async fn without_nango_configured_sync_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_notion_{}.db", new_id("t"))).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state_with_nango(base_config(&db_path), None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/notion/sync", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn sync_materializes_notion_pages_and_databases_with_a_mirror_edge() {
    let ta = test_app().await;
    let ws = register(&ta.router, "notion-sync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_search(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/notion/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["synced"], 2);
    assert_eq!(body["skipped"], 0);

    let (st, notes) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=notion&type=note"), None).await;
    assert_eq!(st, StatusCode::OK);
    let notes = notes.as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["title"], "Meeting notes");
    let note_id = notes[0]["id"].as_str().unwrap().to_string();

    let (st, pages) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=notion&type=notion_page"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    let pages = pages.as_array().unwrap();
    assert_eq!(pages.len(), 1);
    let page_id = pages[0]["id"].as_str().unwrap().to_string();
    assert_eq!(notes[0]["attrs"]["mirrors"], page_id);

    let (st, dbs) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=notion&type=notion_db"), None).await;
    assert_eq!(st, StatusCode::OK);
    let dbs = dbs.as_array().unwrap();
    assert_eq!(dbs.len(), 1);
    assert_eq!(dbs[0]["title"], "Projects");

    let (st, edges) = send_h(&ta.router, "GET", &format!("/api/edge?workspace_id={ws}&src_id={note_id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let edges = edges.as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["rel"], "mirrors");
    assert_eq!(edges[0]["dst_id"], page_id);
}

#[tokio::test]
async fn resyncing_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "notion-resync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_search(&ta.nango);

    let (st, first) = send_h(&ta.router, "POST", "/api/notion/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["synced"], 2);

    let (st, second) = send_h(&ta.router, "POST", "/api/notion/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["synced"], 0, "already-synced pages/databases must not duplicate");
    assert_eq!(second["skipped"], 2);

    let (st, notes) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=notion&type=note"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(notes.as_array().unwrap().len(), 1, "no duplicate note entity");

    let note_id = notes.as_array().unwrap()[0]["id"].as_str().unwrap().to_string();
    let (st, edges) = send_h(&ta.router, "GET", &format!("/api/edge?workspace_id={ws}&src_id={note_id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(edges.as_array().unwrap().len(), 1, "no duplicate mirror edge");
}

#[tokio::test]
async fn push_only_ever_drafts_never_calls_notion() {
    let ta = test_app().await;
    let ws = register(&ta.router, "notion-push").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_search(&ta.nango);

    let (st, _) = send_h(&ta.router, "POST", "/api/notion/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, notes) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=notion&type=note"), None).await;
    let note_id = notes.as_array().unwrap()[0]["id"].as_str().unwrap().to_string();

    ta.nango.calls.lock().unwrap().clear();

    let (st, body) =
        send_h(&ta.router, "POST", "/api/notion/push", Some(json!({"entity_id": note_id, "workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["type"], "notion_push");
    assert_eq!(body["attrs"]["title"], "Meeting notes");
    assert_eq!(body["attrs"]["notion_id"], "page1");

    assert!(ta.nango.calls.lock().unwrap().is_empty(), "push must never call the Notion proxy");
}

#[tokio::test]
async fn push_on_unknown_note_is_not_found() {
    let ta = test_app().await;
    let ws = register(&ta.router, "notion-push-404").await;
    connect(&ta.router, &ta.nango, &ws).await;

    let (st, _) = send_h(
        &ta.router,
        "POST",
        "/api/notion/push",
        Some(json!({"entity_id": "does_not_exist", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_synced_notes() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "notion-tenant-a").await;
    let ws_b = register(&ta.router, "notion-tenant-b").await;
    connect(&ta.router, &ta.nango, &ws_a).await;
    seed_search(&ta.nango);

    let (st, _) = send_h(&ta.router, "POST", "/api/notion/sync", Some(json!({"workspace_id": ws_a}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, notes) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=notion&type=note"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(notes.as_array().unwrap().is_empty());
}
