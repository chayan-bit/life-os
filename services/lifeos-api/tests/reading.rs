//! `POST /api/reading/save` and `/api/reading/highlight` HTTP-level tests
//! (issue #61, docs/MODULES.md §3.6) against a mock article fetcher - no
//! real network needed. Covers: parsing, naive summarization, topic
//! linking, idempotent re-save, highlight capture, and workspace isolation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::reading::mock::MockArticleFetcher;
use lifeos_api::{build_state_with_reading, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    reading: Arc<MockArticleFetcher>,
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
        .join(format!("lifeos_reading_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let reading = Arc::new(MockArticleFetcher::new());
    let state = build_state_with_reading(base_config(&db_path), Some(reading.clone())).await.expect("build state");
    TestApp { router: routes::router(state), db_path, reading }
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

const ARTICLE_HTML: &str = r#"
<html>
<head><title>The Future of Rust Async</title></head>
<body>
<article>
<p>Rust's async ecosystem has matured a lot. This is the first paragraph explaining the state of things today.</p>
<p>Machine learning workloads increasingly rely on Rust for performance-critical paths.</p>
</article>
</body>
</html>
"#;

#[tokio::test]
async fn without_reading_configured_save_returns_not_implemented() {
    let db_path = std::env::temp_dir().join(format!("lifeos_reading_{}.db", new_id("t"))).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state_with_reading(base_config(&db_path), None).await.expect("build state");
    let router = routes::router(state);
    let (st, _) = send_h(&router, "POST", "/api/reading/save", Some(json!({"url": "https://example.com/a"}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}

#[tokio::test]
async fn save_parses_and_summarizes_an_article() {
    let ta = test_app().await;
    let ws = register(&ta.router, "reading-save").await;
    ta.reading.seed("https://example.com/rust-async", ARTICLE_HTML);

    let (st, body) =
        send_h(&ta.router, "POST", "/api/reading/save", Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws})))
            .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["module"], "reading");
    assert_eq!(body["type"], "article");
    assert_eq!(body["attrs"]["title"], "The Future of Rust Async");
    assert_eq!(body["attrs"]["read_state"], "unread");
    assert!(body["attrs"]["est_minutes"].as_i64().unwrap() >= 1);
    assert!(body["attrs"]["summary"].as_str().unwrap().contains("Rust's async ecosystem"));
    assert!(body["attrs"]["excerpt"].as_str().unwrap().contains("Rust's async ecosystem"));

    let (st, sources) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=reading&type=source"), None).await;
    assert_eq!(st, StatusCode::OK);
    let sources = sources.as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["attrs"]["domain"], "example.com");

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=article.saved"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn save_links_to_matching_existing_topics() {
    let ta = test_app().await;
    let ws = register(&ta.router, "reading-topics").await;
    ta.reading.seed("https://example.com/rust-async", ARTICLE_HTML);

    let (st, topic) = send_h(
        &ta.router,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": ws, "module": "learning", "type": "topic", "title": "Rust"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let topic_id = topic["id"].as_str().unwrap().to_string();

    let (st, article) = send_h(
        &ta.router,
        "POST",
        "/api/reading/save",
        Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let article_id = article["id"].as_str().unwrap().to_string();

    let (st, edges) =
        send_h(&ta.router, "GET", &format!("/api/edge?workspace_id={ws}&src_id={article_id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let edges = edges.as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["rel"], "derived_from");
    assert_eq!(edges[0]["dst_id"], topic_id);
}

#[tokio::test]
async fn resaving_the_same_url_is_idempotent() {
    let ta = test_app().await;
    let ws = register(&ta.router, "reading-resave").await;
    ta.reading.seed("https://example.com/rust-async", ARTICLE_HTML);

    let (st, first) = send_h(
        &ta.router,
        "POST",
        "/api/reading/save",
        Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let first_id = first["id"].as_str().unwrap().to_string();

    let (st, second) = send_h(
        &ta.router,
        "POST",
        "/api/reading/save",
        Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["id"], first_id, "re-saving the same URL must return the same article entity");

    let (st, articles) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws}&module=reading&type=article"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(articles.as_array().unwrap().len(), 1, "no duplicate article after a re-save");

    let (st, events) =
        send_h(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=article.saved"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1, "no duplicate article.saved event on re-save");

    assert_eq!(ta.reading.calls.lock().unwrap().len(), 2, "fetch still happens each save call (idempotency is on write)");
}

#[tokio::test]
async fn highlight_requires_an_existing_article() {
    let ta = test_app().await;
    let ws = register(&ta.router, "reading-highlight-404").await;

    let (st, _) = send_h(
        &ta.router,
        "POST",
        "/api/reading/highlight",
        Some(json!({"article_id": "does_not_exist", "quote": "hello", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn highlight_creates_a_highlight_entity_under_the_article() {
    let ta = test_app().await;
    let ws = register(&ta.router, "reading-highlight").await;
    ta.reading.seed("https://example.com/rust-async", ARTICLE_HTML);

    let (_, article) = send_h(
        &ta.router,
        "POST",
        "/api/reading/save",
        Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws})),
    )
    .await;
    let article_id = article["id"].as_str().unwrap().to_string();

    let (st, hl) = send_h(
        &ta.router,
        "POST",
        "/api/reading/highlight",
        Some(json!({"article_id": article_id, "quote": "maturing async ecosystem", "color": "yellow", "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hl:?}");
    assert_eq!(hl["module"], "reading");
    assert_eq!(hl["type"], "highlight");
    assert_eq!(hl["parent_id"], article_id);
    assert_eq!(hl["attrs"]["quote"], "maturing async ecosystem");
    assert_eq!(hl["attrs"]["color"], "yellow");
}

#[tokio::test]
async fn workspace_b_has_no_workspace_as_saved_articles() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "reading-tenant-a").await;
    let ws_b = register(&ta.router, "reading-tenant-b").await;
    ta.reading.seed("https://example.com/rust-async", ARTICLE_HTML);

    let (st, _) = send_h(
        &ta.router,
        "POST",
        "/api/reading/save",
        Some(json!({"url": "https://example.com/rust-async", "workspace_id": ws_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, articles) =
        send_h(&ta.router, "GET", &format!("/api/entity?workspace_id={ws_b}&module=reading&type=article"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(articles.as_array().unwrap().is_empty());
}
