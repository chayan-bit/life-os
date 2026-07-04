//! Acceptance test for issue #9: "find topic about CAP theorem" returns the
//! right entity via hybrid search. Runs lexical-only (no LIFEOS_MEMVEC set in
//! CI), proving the FTS5 + RRF path resolves the query end-to-end through HTTP.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    derived: String,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(&self.derived);
    }
}

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_search_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let derived = format!("{db_path}.derived");
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&derived);
    let config = Config {
        db_path: db_path.clone(),
        turso_url: None,
        turso_token: None,
        sync_interval_secs: 60,
        derived_db_path: derived.clone(),
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
    let state = build_state(config).await.expect("build state");
    TestApp { router: routes::router(state), db_path, derived }
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
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn create_topic(app: &Router, title: &str, attrs: Value) -> String {
    let (status, v) = send(
        app,
        "POST",
        "/api/entity",
        Some(json!({ "module": "learning", "type": "topic", "title": title, "attrs": attrs })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {v}");
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn hybrid_search_finds_cap_theorem_topic() {
    let app = test_app().await;

    let cap = create_topic(
        &app.router,
        "CAP theorem",
        json!({ "summary": "consistency, availability, partition tolerance in distributed systems" }),
    )
    .await;
    create_topic(&app.router, "Gradient descent", json!({ "summary": "optimization for ML" })).await;
    create_topic(&app.router, "Raft consensus", json!({ "summary": "leader election log replication" })).await;

    // The natural-language query is sanitized to FTS terms; CAP must rank first.
    let (status, body) = send(
        &app.router,
        "GET",
        "/api/search?q=find%20topic%20about%20CAP%20theorem",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "lexical", "no memvec in CI -> lexical mode");

    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least one hit");
    assert_eq!(
        results[0]["id"].as_str().unwrap(),
        cap,
        "CAP theorem topic should rank first, got: {body}"
    );
    assert_eq!(results[0]["title"], "CAP theorem");
}

#[tokio::test]
async fn search_matches_on_flattened_attrs_text() {
    let app = test_app().await;
    let id = create_topic(
        &app.router,
        "Distributed databases",
        json!({ "keywords": "partition tolerance quorum" }),
    )
    .await;

    // "quorum" appears only inside attrs, proving the attrs_text flatten indexes
    // JSON values (not just the title).
    let (status, body) = send(&app.router, "GET", "/api/search?q=quorum", None).await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["id"].as_str().unwrap(), id);
}

#[tokio::test]
async fn empty_query_returns_no_results() {
    let app = test_app().await;
    let (status, body) = send(&app.router, "GET", "/api/search?q=%20%21%21", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "empty");
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
}
