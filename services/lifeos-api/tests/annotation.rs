//! `/api/annotation` CRUD + workspace-isolation suite. Annotations are the
//! reader/note layer (docs/DATA-MODEL.md §2.4): mutable, workspace-scoped, and
//! (unlike events) fully CRUD-able. This locks down create/list-filter/
//! update/delete, tenant isolation (A's row invisible + unmutable to B), and
//! the 404/400 behavior the rest of the API shares.

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

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_annot_{}.db", new_id("t")))
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
    let state = build_state(config).await.expect("build state");
    TestApp { router: routes::router(state), db_path }
}

async fn send_h(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    hdrs: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in hdrs {
        builder = builder.header(*k, *v);
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

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    send_h(app, method, uri, body, &[]).await
}

async fn register(app: &Router, name: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/register",
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

async fn create_annotation(app: &Router, ws: &str, entity_id: &str, kind: &str, body: &str) -> String {
    let (st, resp) = send(
        app,
        "POST",
        "/api/annotation",
        Some(json!({
            "workspace_id": ws,
            "entity_id": entity_id,
            "kind": kind,
            "body": body,
            "anchor": { "type": "selection", "quote": "some quote" },
            "attrs": { "color": "yellow" }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create_annotation: {resp:?}");
    resp["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn create_returns_persisted_shape() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-create").await;

    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/annotation",
        Some(json!({
            "workspace_id": ws,
            "entity_id": "topic-sockets",
            "kind": "comment",
            "body": "great point",
            "anchor": { "type": "selection", "quote": "BSD sockets" }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert!(body["id"].as_str().unwrap().starts_with("ann_"));
    assert_eq!(body["kind"], "comment");
    assert_eq!(body["body"], "great point");
    assert_eq!(body["entity_id"], "topic-sockets");
    assert_eq!(body["anchor"]["quote"], "BSD sockets");
    assert_eq!(body["created_by"], "user");
}

#[tokio::test]
async fn kind_defaults_to_note() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-default-kind").await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/annotation",
        Some(json!({"workspace_id": ws, "entity_id": "t1", "body": "no kind given"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["kind"], "note");
}

#[tokio::test]
async fn empty_shell_is_rejected() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-empty").await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/annotation",
        Some(json!({"workspace_id": ws, "kind": "note"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("body"));
}

#[tokio::test]
async fn unknown_workspace_create_returns_400() {
    let ta = test_app().await;
    let (st, body) = send(
        &ta.router,
        "POST",
        "/api/annotation",
        Some(json!({"workspace_id": "ws-nope", "entity_id": "x", "body": "hi"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("workspace"));
}

#[tokio::test]
async fn list_filters_by_entity_and_kind() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-filter").await;
    create_annotation(&ta.router, &ws, "topic-a", "note", "a-note").await;
    create_annotation(&ta.router, &ws, "topic-a", "question", "a-question").await;
    create_annotation(&ta.router, &ws, "topic-b", "note", "b-note").await;

    // by entity_id
    let (st, list) =
        send(&ta.router, "GET", &format!("/api/annotation?workspace_id={ws}&entity_id=topic-a"), None)
            .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2, "topic-a should have 2");

    // by entity_id + kind
    let (st, list) = send(
        &ta.router,
        "GET",
        &format!("/api/annotation?workspace_id={ws}&entity_id=topic-a&kind=question"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["body"], "a-question");
}

#[tokio::test]
async fn update_mutates_body_and_attrs() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-update").await;
    let id = create_annotation(&ta.router, &ws, "t1", "question", "why?").await;

    let (st, body) = send(
        &ta.router,
        "PATCH",
        &format!("/api/annotation/{id}"),
        Some(json!({"workspace_id": ws, "attrs": {"answer": "because", "answeredAt": "2026-07-06"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["attrs"]["answer"], "because");
    // omitted fields are preserved (COALESCE)
    assert_eq!(body["body"], "why?");
    assert_eq!(body["kind"], "question");
}

#[tokio::test]
async fn delete_removes_the_row() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-delete").await;
    let id = create_annotation(&ta.router, &ws, "t1", "note", "temp").await;

    let (st, _) =
        send_h(&ta.router, "DELETE", &format!("/api/annotation/{id}"), None, &[("x-workspace-id", &ws)]).await;
    assert_eq!(st, StatusCode::OK);

    // gone from the list
    let (_, list) = send(&ta.router, "GET", &format!("/api/annotation?workspace_id={ws}"), None).await;
    assert!(list.as_array().unwrap().iter().all(|a| a["id"] != id), "row still present after delete");
}

#[tokio::test]
async fn missing_annotation_patch_and_delete_404() {
    let ta = test_app().await;
    let ws = register(&ta.router, "annot-404").await;
    let (st, _) = send(
        &ta.router,
        "PATCH",
        "/api/annotation/ann_ghost",
        Some(json!({"workspace_id": ws, "body": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) =
        send_h(&ta.router, "DELETE", "/api/annotation/ann_ghost", None, &[("x-workspace-id", &ws)]).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workspace_b_cannot_see_or_mutate_workspace_a_annotation() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "annot-tenant-a").await;
    let ws_b = register(&ta.router, "annot-tenant-b").await;
    let id = create_annotation(&ta.router, &ws_a, "t1", "note", "secret").await;

    // B's list excludes A's annotation.
    let (st, list) = send(&ta.router, "GET", &format!("/api/annotation?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().all(|a| a["id"] != id), "A's annotation leaked to B");

    // B cannot PATCH A's annotation.
    let (st, _) = send(
        &ta.router,
        "PATCH",
        &format!("/api/annotation/{id}"),
        Some(json!({"workspace_id": ws_b, "body": "hacked"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "B patched A's annotation");

    // B cannot DELETE A's annotation.
    let (st, _) =
        send_h(&ta.router, "DELETE", &format!("/api/annotation/{id}"), None, &[("x-workspace-id", &ws_b)]).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "B deleted A's annotation");

    // A's row survived untouched.
    let (_, list) = send(&ta.router, "GET", &format!("/api/annotation?workspace_id={ws_a}&entity_id=t1"), None).await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["body"], "secret");
}
