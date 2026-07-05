//! `POST /api/drive/sync`, `/api/drive/share`, and `/api/files/commit`
//! HTTP-level tests (issue #58, docs/MODULES.md §3.3, docs/VERSIONING.md).
//! Covers: materializing Drive files as `file` entities via a mock Nango
//! client, `share` only ever drafting, and the local content-addressed
//! commit/version-history path (no Nango/Drive involved at all).

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
        .join(format!("lifeos_files_{}.db", new_id("t")))
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
    let connection_id = format!("conn_google-drive_{ws}");
    nango.seed(&connection_id, "google-drive");
    let (st, body) = send_h(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": "google-drive", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect: {body:?}");
}

fn seed_files(nango: &MockNangoClient) {
    nango.seed_proxy(
        "google-drive",
        "GET",
        "drive/v3/files",
        json!({
            "files": [
                {"id": "f1", "name": "roadmap.md", "mimeType": "text/markdown", "size": "1024", "parents": ["folder1"]},
                {"id": "f2", "name": "budget.xlsx", "mimeType": "application/vnd.ms-excel", "size": "2048"},
            ]
        }),
    );
}

#[tokio::test]
async fn without_nango_configured_drive_sync_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_files_{}.db", new_id("t"))).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state_with_nango(base_config(&db_path), None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/drive/sync", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn sync_materializes_drive_files_as_file_entities() {
    let ta = test_app().await;
    let ws = register(&ta.router, "drive-sync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_files(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/drive/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["synced"], 2);
    assert_eq!(body["skipped"], 0);

    let (st, files) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=files&type=file"), None).await;
    assert_eq!(st, StatusCode::OK);
    let files = files.as_array().unwrap();
    assert_eq!(files.len(), 2);
    let f1 = files.iter().find(|f| f["attrs"]["drive_id"] == "f1").expect("f1 present");
    assert_eq!(f1["attrs"]["name"], "roadmap.md");
    assert_eq!(f1["attrs"]["parent_folder"], "folder1");
    assert_eq!(f1["module"], "files");
    assert_eq!(f1["type"], "file");
}

#[tokio::test]
async fn resyncing_drive_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "drive-resync").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_files(&ta.nango);

    let (st, first) = send_h(&ta.router, "POST", "/api/drive/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["synced"], 2);

    let (st, second) = send_h(&ta.router, "POST", "/api/drive/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["synced"], 0);
    assert_eq!(second["skipped"], 2);
}

#[tokio::test]
async fn share_only_ever_drafts_never_calls_drive() {
    let ta = test_app().await;
    let ws = register(&ta.router, "drive-share").await;
    connect(&ta.router, &ta.nango, &ws).await;

    let (st, body) = send_h(
        &ta.router,
        "POST",
        "/api/drive/share",
        Some(json!({"entity_id": "file_x", "target": "someone@example.com", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["type"], "drive_share");

    assert!(ta.nango.calls.lock().unwrap().is_empty(), "share must never call the Drive proxy");
}

#[tokio::test]
async fn commit_creates_a_file_and_a_version_created_event() {
    let ta = test_app().await;
    let ws = register(&ta.router, "commit-new").await;

    let (st, body) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"name": "notes.md", "mime": "text/markdown", "content": "# hello", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["attrs"]["name"], "notes.md");
    assert_eq!(body["attrs"]["version_no"], 1);
    let blob_ref = body["attrs"]["blob_ref"].as_str().unwrap().to_string();
    assert_eq!(body["blob_ref"], blob_ref);
    let id = body["id"].as_str().unwrap().to_string();

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&entity_id={id}&type=version.created"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    let events = events.as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["attrs"]["blob_ref"], blob_ref);
    assert!(events[0]["attrs"]["parent_blob_ref"].is_null());
}

#[tokio::test]
async fn recommitting_changed_content_chains_version_history() {
    let ta = test_app().await;
    let ws = register(&ta.router, "commit-chain").await;

    let (st, first) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"name": "notes.md", "content": "v1", "message": "initial", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let id = first["id"].as_str().unwrap().to_string();
    let v1_blob = first["attrs"]["blob_ref"].as_str().unwrap().to_string();

    let (st, second) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"entity_id": id, "name": "notes.md", "content": "v2", "message": "update", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{second:?}");
    assert_eq!(second["id"], id, "committing onto entity_id must not create a new entity");
    assert_eq!(second["attrs"]["version_no"], 2);
    assert_ne!(second["attrs"]["blob_ref"], v1_blob);

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&entity_id={id}&type=version.created"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    let events = events.as_array().unwrap();
    assert_eq!(events.len(), 2, "history is just a query over events - no separate table");
    let v2_event = events.iter().find(|e| e["attrs"]["message"] == "update").expect("v2 event present");
    assert_eq!(v2_event["attrs"]["parent_blob_ref"], v1_blob);
}

#[tokio::test]
async fn recommitting_identical_content_is_rejected() {
    let ta = test_app().await;
    let ws = register(&ta.router, "commit-noop").await;

    let (st, first) =
        send_h(&ta.router, "POST", "/api/files/commit", Some(json!({"name": "notes.md", "content": "same", "workspace_id": ws})))
            .await;
    assert_eq!(st, StatusCode::OK);
    let id = first["id"].as_str().unwrap().to_string();

    let (st, _) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"entity_id": id, "name": "notes.md", "content": "same", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn commit_auto_enqueues_an_ingest_job() {
    let ta = test_app().await;
    let ws = register(&ta.router, "commit-autoingest").await;

    let (st, body) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"name": "notes.md", "content": "auto-ingest me", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let id = body["id"].as_str().unwrap().to_string();
    let blob_ref = body["attrs"]["blob_ref"].as_str().unwrap().to_string();

    let (st, jobs) = send_h(&ta.router, "GET", &format!("/api/jobs?workspace_id={ws}&kind=ingest"), None).await;
    assert_eq!(st, StatusCode::OK);
    let jobs = jobs.as_array().unwrap();
    assert_eq!(jobs.len(), 1, "commit must auto-enqueue exactly one ingest job, {jobs:?}");
    assert_eq!(jobs[0]["payload"]["entity_id"], id);
    assert_eq!(jobs[0]["payload"]["blob_ref"], blob_ref);
}

#[tokio::test]
async fn drive_sync_does_not_enqueue_ingest_without_a_blob_ref() {
    let ta = test_app().await;
    let ws = register(&ta.router, "drive-sync-noingest").await;
    connect(&ta.router, &ta.nango, &ws).await;
    seed_files(&ta.nango);

    let (st, body) = send_h(&ta.router, "POST", "/api/drive/sync", Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let (st, jobs) = send_h(&ta.router, "GET", &format!("/api/jobs?workspace_id={ws}&kind=ingest"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        jobs.as_array().unwrap().is_empty(),
        "Drive-synced files have no blob_ref yet, so auto-ingest must not enqueue a job it can't process"
    );
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_files() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "files-tenant-a").await;
    let ws_b = register(&ta.router, "files-tenant-b").await;

    let (st, _) = send_h(
        &ta.router,
        "POST",
        "/api/files/commit",
        Some(json!({"name": "secret.md", "content": "shh", "workspace_id": ws_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, files) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=files&type=file"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(files.as_array().unwrap().is_empty());
}
