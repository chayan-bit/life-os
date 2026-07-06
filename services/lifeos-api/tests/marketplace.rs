//! `/api/marketplace/*` HTTP tests (issues #101/#102/#147).
//!
//! The load-bearing acceptance for #147 is that INSTALL re-validates: a package
//! is activated only if (1) its stored signature verifies AND (2) its manifest
//! still passes the Tier-0 validator chain. Both a forged-signature bundle and a
//! validly-signed-but-structurally-invalid manifest must be rejected, and a
//! clean install must land the `module_manifest` entity the live app renders.
//!
//! These tests shell the real `server/validators/validatePackage.js` (Node), so
//! `server_dir` is pointed at the repo-root `server/` dir rather than the
//! per-crate default - the validator is the thing under test, not a mock.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::{engine::general_purpose::STANDARD, Engine};
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

/// The repo-root `server/` dir, so the shelled `node validators/validatePackage.js`
/// resolves. `CARGO_MANIFEST_DIR` is `services/lifeos-api`.
fn real_server_dir() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server");
    std::fs::canonicalize(&p)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
}

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_mkt_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    // A fixed 32-byte seed - the marketplace needs a signing key configured, or
    // publish returns 501 (NotImplemented).
    let seed = STANDARD.encode([7u8; 32]);
    let signing_key = lifeos_api::marketplace_sign::parse_signing_key(&seed).unwrap();
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
        server_dir: real_server_dir(),
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
        marketplace_signing_key: Some(signing_key),
        turso_platform_api_token: None,
        turso_org_slug: None,
    };
    let state = build_state(config).await.expect("build state");
    TestApp { router: routes::router(state), db_path }
}

/// Sends a request, optionally stamping the tenant header install resolves from.
async fn send(app: &Router, method: &str, uri: &str, ws: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(ws) = ws {
        builder = builder.header("x-workspace-id", ws);
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

async fn register(app: &Router, name: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        None,
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

/// A structurally-complete T0 manifest that passes the real validator.
fn valid_manifest(id: &str, version: &str) -> Value {
    json!({
        "id": id,
        "name": "Reading",
        "icon": "Book",
        "color": "var(--neo-mint)",
        "version": version,
        "entityTypes": {
            "book": { "label": "Book", "plural": "Books", "icon": "Book",
                      "attrs": { "title": { "type": "text", "required": true } } }
        },
        "views": [{ "id": "all", "label": "All", "kind": "list", "type": "book" }]
    })
}

/// Publishes a package and returns its package_id.
async fn publish(app: &Router, ws: &str, module_id: &str, version: &str, manifest: Value) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/marketplace/publish",
        Some(ws),
        Some(json!({ "module_id": module_id, "version": version, "manifest": manifest, "workspace_id": ws })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "publish {module_id}@{version}: {body:?}");
    body["package_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn install_rejects_a_forged_signature() {
    let ta = test_app().await;
    let ws = register(&ta.router, "mkt-forged").await;

    // Insert a package row with a bogus signature directly - a forged registry
    // row / on-disk tamper the HTTP publish path would never produce.
    let db = libsql::Builder::new_local(&ta.db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "INSERT INTO module_packages (id, workspace_id, module_id, version, manifest_json, signature, publisher_pubkey, created_at) \
         VALUES ('pkg_forged', ?1, 'reading', '1.0.0', ?2, 'not-a-real-signature', 'not-a-real-pubkey', 0)",
        libsql::params![ws.clone(), valid_manifest("reading", "1.0.0").to_string()],
    )
    .await
    .unwrap();

    let (st, body) = send(&ta.router, "POST", "/api/marketplace/install", Some(&ws), Some(json!({"package_id": "pkg_forged"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(body["error"].as_str().unwrap().contains("signature verification"), "{body:?}");
}

#[tokio::test]
async fn install_rejects_a_signed_but_structurally_invalid_manifest() {
    let ta = test_app().await;
    let ws = register(&ta.router, "mkt-invalid").await;

    // Publish signs correctly (structural_check only matches id/version), so the
    // signature WILL verify - but the manifest is missing every required T0
    // field, so the validator re-run must still reject the install.
    let pkg = publish(&ta.router, &ws, "badmod", "1.0.0", json!({ "id": "badmod", "version": "1.0.0" })).await;

    let (st, body) = send(&ta.router, "POST", "/api/marketplace/install", Some(&ws), Some(json!({"package_id": pkg}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(body["error"].as_str().unwrap().contains("failed validation"), "{body:?}");
}

#[tokio::test]
async fn install_validates_and_lands_the_manifest_entity() {
    let ta = test_app().await;
    let ws = register(&ta.router, "mkt-install").await;
    let pkg = publish(&ta.router, &ws, "reading", "1.0.0", valid_manifest("reading", "1.0.0")).await;

    let (st, body) = send(&ta.router, "POST", "/api/marketplace/install", Some(&ws), Some(json!({"package_id": pkg}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["installed"], json!(true));

    // The module_manifest entity the live app reads must now exist in this ws.
    let (_, entities) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=system&type=module_manifest"), Some(&ws), None).await;
    let rows = entities.as_array().unwrap();
    assert_eq!(rows.len(), 1, "one module_manifest entity: {entities:?}");
    assert_eq!(rows[0]["title"], json!("module_manifest_reading"));
    assert_eq!(rows[0]["attrs"]["id"], json!("reading"));

    // And the install was recorded as an event.
    let (_, events) = send(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=marketplace.installed"), Some(&ws), None).await;
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn versions_lists_every_published_version_for_a_module() {
    let ta = test_app().await;
    let ws = register(&ta.router, "mkt-versions").await;
    publish(&ta.router, &ws, "reading", "1.0.0", valid_manifest("reading", "1.0.0")).await;
    publish(&ta.router, &ws, "reading", "1.1.0", valid_manifest("reading", "1.1.0")).await;

    let (st, body) = send(&ta.router, "GET", &format!("/api/marketplace/package/reading/versions?workspace_id={ws}"), Some(&ws), None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let versions = body["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    let got: std::collections::HashSet<&str> = versions.iter().map(|v| v["version"].as_str().unwrap()).collect();
    assert!(got.contains("1.0.0") && got.contains("1.1.0"), "{versions:?}");
}

#[tokio::test]
async fn versions_are_workspace_scoped() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "mkt-iso-a").await;
    let ws_b = register(&ta.router, "mkt-iso-b").await;
    publish(&ta.router, &ws_a, "reading", "1.0.0", valid_manifest("reading", "1.0.0")).await;

    let (_, a) = send(&ta.router, "GET", &format!("/api/marketplace/package/reading/versions?workspace_id={ws_a}"), Some(&ws_a), None).await;
    let (_, b) = send(&ta.router, "GET", &format!("/api/marketplace/package/reading/versions?workspace_id={ws_b}"), Some(&ws_b), None).await;
    assert_eq!(a["versions"].as_array().unwrap().len(), 1);
    assert_eq!(b["versions"].as_array().unwrap().len(), 0, "B must not see A's versions");
}

#[tokio::test]
async fn install_lands_the_manifest_in_the_callers_workspace_not_the_publishers() {
    let ta = test_app().await;
    let ws_pub = register(&ta.router, "mkt-publisher").await;
    let ws_inst = register(&ta.router, "mkt-installer").await;
    // Publisher publishes; a different tenant installs by package_id.
    let pkg = publish(&ta.router, &ws_pub, "reading", "1.0.0", valid_manifest("reading", "1.0.0")).await;

    let (st, _) = send(&ta.router, "POST", "/api/marketplace/install", Some(&ws_inst), Some(json!({"package_id": pkg}))).await;
    assert_eq!(st, StatusCode::OK);

    // The manifest entity lands in the installer's workspace only.
    let (_, inst_rows) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_inst}&module=system&type=module_manifest"), Some(&ws_inst), None).await;
    let (_, pub_rows) = send(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_pub}&module=system&type=module_manifest"), Some(&ws_pub), None).await;
    assert_eq!(inst_rows.as_array().unwrap().len(), 1, "installer got the manifest entity");
    assert_eq!(pub_rows.as_array().unwrap().len(), 0, "publisher's workspace stays untouched");
}
