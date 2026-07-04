//! `/api/storage/backends` HTTP-level tests (issue #107,
//! docs/STORAGE-BACKENDS.md §3-§4): configs draft as `pending_approval`
//! (gated - adding/switching a backend moves user data), key material is
//! envelope-encrypted into `connections.secret_enc` and never appears in
//! entity attrs, and unknown kinds are rejected.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state_with_nango, config::Config, crypto, ids::new_id, routes};
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
        secret_encryption_key: Some(
            crypto::parse_key("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=").expect("test key"),
        ),
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
        .join(format!("lifeos_storage_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state_with_nango(base_config(&db_path), None).await.expect("build state");
    TestApp { router: routes::router(state), db_path }
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
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

async fn register(app: &Router, name: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        Some(json!({
            "email": format!("{name}@test.example"),
            "name": name,
            "password": "test-password-123",
            "workspace_name": name
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn creating_a_backend_only_drafts_it_pending_approval() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-gated").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({
            "kind": "s3",
            "folder": "lifeos",
            "default": true,
            "keys": {"bucket": "my-bucket", "access_key_id": "AKIA", "secret_access_key": "sekrit"},
            "workspace_id": ws
        })),
    )
    .await;

    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "pending_approval");
    assert_eq!(body["module"], "storage");
    assert_eq!(body["type"], "storage_backend");
}

#[tokio::test]
async fn key_material_never_lands_in_entity_attrs() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-secrets").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({
            "kind": "webdav",
            "keys": {"base_url": "https://dav.example.com", "username": "chayan", "password": "hunter2"},
            "workspace_id": ws
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let attrs_str = serde_json::to_string(&body["attrs"]).unwrap();
    assert!(!attrs_str.contains("hunter2"), "secret leaked into attrs: {attrs_str}");
    assert!(!attrs_str.contains("dav.example.com"), "keys blob leaked into attrs: {attrs_str}");
    assert!(body["attrs"]["connection_id"].as_str().is_some(), "attrs should reference the connection: {body:?}");

    // The listing surface leaks nothing either.
    let (st, listed) = send(&ta.router, "GET", &format!("/api/storage/backends?workspace_id={ws}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!listed.to_string().contains("hunter2"));
}

#[tokio::test]
async fn unknown_kind_is_rejected() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-badkind").await;

    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({"kind": "carrier-pigeon", "workspace_id": ws})),
    )
    .await;

    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn non_local_backend_without_credentials_is_rejected() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-nocreds").await;

    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({"kind": "google-drive", "workspace_id": ws})),
    )
    .await;

    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_is_workspace_scoped() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "storage-ws-a").await;
    let ws_b = register(&ta.router, "storage-ws-b").await;

    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({"kind": "local-fs", "workspace_id": ws_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (_, listed_a) = send(&ta.router, "GET", &format!("/api/storage/backends?workspace_id={ws_a}"), None).await;
    let (_, listed_b) = send(&ta.router, "GET", &format!("/api/storage/backends?workspace_id={ws_b}"), None).await;
    assert_eq!(listed_a.as_array().unwrap().len(), 1);
    assert_eq!(listed_b.as_array().unwrap().len(), 0);
}

/// The issue-#108 acceptance, end to end over HTTP: commit a blob (backend
/// X = the local CAS), approve a second backend Y, migrate, then read the
/// SAME blob_ref back after deleting X's bytes - served from Y, verified.
#[tokio::test]
async fn migrate_moves_blobs_and_reads_fall_back_by_the_same_blob_ref() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-migrate").await;

    // Commit real bytes through the VCS surface (lands in the local CAS).
    use base64::Engine as _;
    let content = "# Migration target\n\nsame hash, new home.".repeat(500);
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(&content);
    let (st, committed) = send(
        &ta.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "note.md", "mime": "text/markdown", "content_base64": content_b64, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{committed:?}");
    let blob_ref = committed["blob_ref"].as_str().unwrap().to_string();

    // Backend Y: a second local-fs root (pending -> human-approved active).
    let target_root = std::env::temp_dir().join(format!("lifeos_migrate_target_{}", new_id("t")));
    let (st, backend) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({"kind": "local-fs", "folder": target_root.to_string_lossy(), "default": false, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{backend:?}");
    let backend_id = backend["id"].as_str().unwrap().to_string();

    // Migrating to an unapproved backend is refused (gated).
    let (st, _) = send(
        &ta.router,
        "POST",
        "/api/storage/migrate",
        Some(json!({"target_backend_id": backend_id, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Approve (the human step), then migrate.
    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/entity/{backend_id}"),
        Some(json!({"status": "active", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, resp) = send(
        &ta.router,
        "POST",
        "/api/storage/migrate",
        Some(json!({"target_backend_id": backend_id, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{resp:?}");
    let job_id = resp["job_id"].as_str().unwrap().to_string();

    // The migration runs async - poll the job until it finishes.
    let mut status = String::new();
    for _ in 0..100 {
        let (_, jobs) = send(&ta.router, "GET", &format!("/api/jobs?workspace_id={ws}"), None).await;
        if let Some(job) = jobs.as_array().into_iter().flatten().find(|j| j["id"] == job_id.as_str()) {
            status = job["status"].as_str().unwrap_or("").to_string();
            if status == "done" || status == "failed" {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(status, "done");

    // Primary pointer flipped to the target.
    let (_, backends) = send(&ta.router, "GET", &format!("/api/storage/backends?workspace_id={ws}"), None).await;
    let target = backends.as_array().unwrap().iter().find(|b| b["id"] == backend_id.as_str()).unwrap();
    assert_eq!(target["attrs"]["default"], json!(true));

    // Wipe backend X (the local CAS) - the same blob_ref must now come from Y.
    let blob_root = format!("{}.blobs", ta.db_path);
    std::fs::remove_dir_all(&blob_root).unwrap();
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/vcs/blob?blob_ref={blob_ref}&workspace_id={ws}"))
        .body(Body::empty())
        .unwrap();
    let resp = ta.router.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), content.as_bytes());

    let _ = std::fs::remove_dir_all(&target_root);
}

/// Issue-#110 acceptance over the factory path: with encryption on, the
/// provider directory holds ciphertext only, and fetch decrypts + verifies
/// under the unchanged blob_ref.
#[tokio::test]
async fn encrypted_backend_stores_ciphertext_and_fetch_decrypts() {
    let ta = test_app().await;
    let ws = register(&ta.router, "storage-encrypted").await;

    use base64::Engine as _;
    let content = "TOP-SECRET-PLAINTEXT-MARKER ".repeat(2000);
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(&content);
    let (st, committed) = send(
        &ta.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "secret.md", "mime": "text/markdown", "content_base64": content_b64, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{committed:?}");
    let blob_ref = committed["blob_ref"].as_str().unwrap().to_string();

    let target_root = std::env::temp_dir().join(format!("lifeos_enc_target_{}", new_id("t")));
    let (st, backend) = send(
        &ta.router,
        "POST",
        "/api/storage/backends",
        Some(json!({"kind": "local-fs", "folder": target_root.to_string_lossy(), "encryption": true, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{backend:?}");
    let backend_id = backend["id"].as_str().unwrap().to_string();
    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/entity/{backend_id}"),
        Some(json!({"status": "active", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, resp) = send(
        &ta.router,
        "POST",
        "/api/storage/migrate",
        Some(json!({"target_backend_id": backend_id, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{resp:?}");
    let job_id = resp["job_id"].as_str().unwrap().to_string();
    let mut status = String::new();
    for _ in 0..100 {
        let (_, jobs) = send(&ta.router, "GET", &format!("/api/jobs?workspace_id={ws}"), None).await;
        if let Some(job) = jobs.as_array().into_iter().flatten().find(|j| j["id"] == job_id.as_str()) {
            status = job["status"].as_str().unwrap_or("").to_string();
            if status == "done" || status == "failed" {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(status, "done");

    // The provider's directory never contains the plaintext marker.
    let mut leaked = false;
    for entry in walk(&target_root) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        if bytes.windows(27).any(|w| w == b"TOP-SECRET-PLAINTEXT-MARKER") {
            leaked = true;
        }
    }
    assert!(!leaked, "plaintext found in the encrypted backend's storage");

    // Fetch after wiping the local CAS: decrypts + verifies, same blob_ref.
    let blob_root = format!("{}.blobs", ta.db_path);
    std::fs::remove_dir_all(&blob_root).unwrap();
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/vcs/blob?blob_ref={blob_ref}&workspace_id={ws}"))
        .body(Body::empty())
        .unwrap();
    let resp = ta.router.clone().oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), content.as_bytes());

    let _ = std::fs::remove_dir_all(&target_root);
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return files };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
