//! `/api/approvals` + `/api/approval/:id/{approve,deny}` HTTP tests (issue #142).
//!
//! The API mirror of the Worker's approval semantics: workspace-scoped listing,
//! a CAS transition that makes double-approve safe (409 on the loser), an
//! `execute_approval` job enqueued only on approve, and server-enforced typed
//! confirmation for T5-style gates (`attrs.requires_typed_confirm`).

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
        .join(format!("lifeos_appr_{}.db", new_id("t")))
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
        Some(json!({"email": format!("{name}@test.example"), "name": name, "password": "test-password-123", "workspace_name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    body["workspace_id"].as_str().unwrap().to_string()
}

/// Seeds a pending_approval draft entity via the generic entity route.
async fn seed_draft(app: &Router, ws: &str) -> String {
    let (st, body) = send(
        app,
        "POST",
        "/api/entity",
        Some(json!({
            "workspace_id": ws, "module": "bot", "type": "draft",
            "status": "pending_approval", "attrs": {"text": "announce the launch"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "seed_draft: {body:?}");
    body["id"].as_str().unwrap().to_string()
}

async fn jobs_of(app: &Router, ws: &str, kind: &str) -> usize {
    let (_, list) = send(app, "GET", &format!("/api/jobs?workspace_id={ws}&kind={kind}"), None).await;
    list.as_array().map(|a| a.len()).unwrap_or(0)
}

#[tokio::test]
async fn approve_transitions_emits_and_enqueues_execute_approval() {
    let ta = test_app().await;
    let ws = register(&ta.router, "appr-approve").await;
    let id = seed_draft(&ta.router, &ws).await;

    let (st, body) = send(
        &ta.router,
        "POST",
        &format!("/api/approval/{id}/approve"),
        Some(json!({"workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "approved");

    // One execute_approval job for the drain.
    assert_eq!(jobs_of(&ta.router, &ws, "execute_approval").await, 1);

    // A draft.approved event was appended.
    let (_, events) = send(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=draft.approved"), None).await;
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn deny_transitions_and_enqueues_nothing() {
    let ta = test_app().await;
    let ws = register(&ta.router, "appr-deny").await;
    let id = seed_draft(&ta.router, &ws).await;

    let (st, body) = send(
        &ta.router,
        "POST",
        &format!("/api/approval/{id}/deny"),
        Some(json!({"workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(jobs_of(&ta.router, &ws, "execute_approval").await, 0);

    let (_, events) = send(&ta.router, "GET", &format!("/api/event?workspace_id={ws}&type=draft.rejected"), None).await;
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn double_approve_is_a_409_and_never_double_enqueues() {
    let ta = test_app().await;
    let ws = register(&ta.router, "appr-double").await;
    let id = seed_draft(&ta.router, &ws).await;

    let (st1, _) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st1, StatusCode::OK);

    let (st2, body2) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st2, StatusCode::CONFLICT, "{body2:?}");

    // Exactly one job, one event - the second approve did nothing.
    assert_eq!(jobs_of(&ta.router, &ws, "execute_approval").await, 1);
}

#[tokio::test]
async fn list_is_workspace_scoped() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "appr-ws-a").await;
    let ws_b = register(&ta.router, "appr-ws-b").await;
    seed_draft(&ta.router, &ws_a).await;

    let (_, list_a) = send(&ta.router, "GET", &format!("/api/approvals?workspace_id={ws_a}"), None).await;
    let (_, list_b) = send(&ta.router, "GET", &format!("/api/approvals?workspace_id={ws_b}"), None).await;
    assert_eq!(list_a.as_array().unwrap().len(), 1);
    assert_eq!(list_b.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn cannot_approve_another_workspaces_pending() {
    let ta = test_app().await;
    let ws_a = register(&ta.router, "appr-iso-a").await;
    let ws_b = register(&ta.router, "appr-iso-b").await;
    let id = seed_draft(&ta.router, &ws_a).await;

    // B tries to approve A's draft -> 404 (invisible), and A's draft stays pending.
    let (st, _) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws_b}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (_, list_a) = send(&ta.router, "GET", &format!("/api/approvals?workspace_id={ws_a}"), None).await;
    assert_eq!(list_a.as_array().unwrap().len(), 1, "A's draft must remain pending");
}

#[tokio::test]
async fn typed_confirm_gate_refuses_without_exact_phrase() {
    let ta = test_app().await;
    let ws = register(&ta.router, "appr-typed").await;

    // A T5-style gate: awaiting_approval + requires_typed_confirm + node name.
    let (st, gate) = send(
        &ta.router,
        "POST",
        "/api/entity",
        Some(json!({
            "workspace_id": ws, "module": "pipelines", "type": "pending_approval",
            "title": "T5 crate", "status": "awaiting_approval",
            "attrs": {"requires_typed_confirm": true, "node": "t5", "tier": "T5"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{gate:?}");
    let id = gate["id"].as_str().unwrap().to_string();

    // No typed -> 400, nothing enqueued, still awaiting.
    let (st, _) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Wrong typed -> 400.
    let (st, _) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws, "typed": "nope"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(jobs_of(&ta.router, &ws, "execute_approval").await, 0);

    // Exact phrase -> approved, one job.
    let (st, body) = send(&ta.router, "POST", &format!("/api/approval/{id}/approve"), Some(json!({"workspace_id": ws, "typed": "t5"}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["status"], "approved");
    assert_eq!(jobs_of(&ta.router, &ws, "execute_approval").await, 1);
}
