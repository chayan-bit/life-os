//! HTTP-level integration tests. Each test builds the real router against a
//! throwaway libSQL file and drives it with `tower::ServiceExt::oneshot` - no
//! network, no port binding.

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
        .join(format!("lifeos_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path)).await.expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

/// Same as `test_app`, but with a marketplace signing key configured so
/// `/api/marketplace/publish` doesn't 501.
async fn test_app_with_marketplace() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let mut config = base_config(&db_path);
    config.marketplace_signing_key = Some(lifeos_api::marketplace_sign::generate_signing_key());
    let state = build_state(config).await.expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

/// Send a request, return `(status, parsed-json-or-Null)`.
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

#[tokio::test]
async fn health_is_healthy_and_touches_db() {
    let app = test_app().await;
    let (status, body) = send(&app.router, "GET", "/api/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "healthy");
    assert!(body["workspace_id"].is_string());
}

#[tokio::test]
async fn register_persists_and_rejects_duplicate_email() {
    let app = test_app().await;
    let payload = json!({"email": "x@y.z", "name": "X", "password": "test-password-123", "workspace_name": "WS"});

    let (status, body) = send(&app.router, "POST", "/api/register", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let ws = body["workspace_id"].as_str().unwrap().to_string();
    assert!(body["key_token"].as_str().unwrap().len() > 10);
    assert!(body["refresh_token"].as_str().unwrap().len() > 10);
    assert_eq!(body["status"], "registered");

    // The workspace must really exist: creating an entity scoped to it succeeds.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": ws, "module": "tasks", "type": "task", "title": "t"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Re-registering the same email is a real conflict now (issue #100) -
    // no longer a soft "re-issue a token for whoever owns this email".
    let (status2, _) = send(&app.router, "POST", "/api/register", Some(payload)).await;
    assert_eq!(status2, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_verifies_password_and_refresh_rotates_the_session() {
    let app = test_app().await;
    let payload = json!({"email": "login@y.z", "name": "L", "password": "correct-horse-battery", "workspace_name": "WS"});
    let (_, reg) = send(&app.router, "POST", "/api/register", Some(payload)).await;
    let workspace_id = reg["workspace_id"].as_str().unwrap().to_string();

    // Wrong password rejected.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "login@y.z", "password": "wrong"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Correct password logs in.
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "login@y.z", "password": "correct-horse-battery"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["workspace_id"], workspace_id);
    let refresh1 = body["refresh_token"].as_str().unwrap().to_string();

    // Refresh rotates: old token can never be reused, new one is different and works.
    let (st, refreshed) = send(
        &app.router,
        "POST",
        "/api/session/refresh",
        Some(json!({"refresh_token": refresh1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let refresh2 = refreshed["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(refresh1, refresh2);

    let (st, _) = send(
        &app.router,
        "POST",
        "/api/session/refresh",
        Some(json!({"refresh_token": refresh1})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a rotated-away refresh token must not be reusable");

    let (st, _) = send(
        &app.router,
        "POST",
        "/api/session/refresh",
        Some(json!({"refresh_token": refresh2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn logout_revokes_the_session_and_set_password_only_works_once() {
    let app = test_app().await;
    let payload = json!({"email": "logout@y.z", "name": "L", "password": "some-password-here", "workspace_name": "WS"});
    let (_, reg) = send(&app.router, "POST", "/api/register", Some(payload)).await;
    let refresh = reg["refresh_token"].as_str().unwrap().to_string();

    let (st, _) = send(&app.router, "POST", "/api/logout", Some(json!({"refresh_token": refresh.clone()}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, _) = send(&app.router, "POST", "/api/session/refresh", Some(json!({"refresh_token": refresh}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a revoked session must not be refreshable");

    // set-password only ever works for a passwordless account.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "logout@y.z", "password": "another-password"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "an account with a password already set must reject set-password");
}

#[tokio::test]
async fn entity_create_list_and_workspace_scoping() {
    let app = test_app().await;
    // Create in the default workspace.
    let (st, ent) = send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"module": "tasks", "type": "task", "title": "Buy milk", "attrs": {"priority": "high"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ent["module"], "tasks");
    assert_eq!(ent["attrs"]["priority"], "high");
    assert!(ent["id"].as_str().unwrap().starts_with("ent_"));

    // Listing the default workspace returns it.
    let (st, list) = send(&app.router, "GET", "/api/entity?module=tasks", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // A different (non-existent) workspace sees nothing - tenant isolation.
    let (st, other) = send(
        &app.router,
        "GET",
        "/api/entity?workspace_id=ws_someone_else",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(other.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn entity_get_and_update_emits_event() {
    let app = test_app().await;
    let (_, ent) = send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"module": "tasks", "type": "task", "title": "T", "status": "todo"})),
    )
    .await;
    let id = ent["id"].as_str().unwrap();

    // PATCH the status.
    let (st, updated) = send(
        &app.router,
        "PATCH",
        &format!("/api/entity/{id}"),
        Some(json!({"status": "completed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(updated["status"], "completed");
    assert_eq!(updated["title"], "T"); // untouched field preserved

    // GET reflects the update.
    let (st, fetched) = send(&app.router, "GET", &format!("/api/entity/{id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fetched["status"], "completed");

    // An entity.updated event was logged.
    let (_, events) = send(&app.router, "GET", "/api/event?type=entity.updated", None).await;
    assert_eq!(events.as_array().unwrap().len(), 1);

    // Missing entity -> 404.
    let (st, _) = send(&app.router, "GET", "/api/entity/ent_missing", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn entity_list_paginates_with_limit_and_offset() {
    let app = test_app().await;
    for i in 0..3 {
        send(
            &app.router,
            "POST",
            "/api/entity",
            Some(json!({"module": "tasks", "type": "task", "title": format!("t{i}")})),
        )
        .await;
    }
    // First page (limit 2) and second page (offset 2) partition the 3 rows.
    let (_, page1) = send(&app.router, "GET", "/api/entity?module=tasks&limit=2", None).await;
    let (_, page2) = send(&app.router, "GET", "/api/entity?module=tasks&limit=2&offset=2", None).await;
    assert_eq!(page1.as_array().unwrap().len(), 2);
    assert_eq!(page2.as_array().unwrap().len(), 1);
    // No overlap between pages.
    let id_a = page1[0]["id"].as_str().unwrap();
    let id_b = page2[0]["id"].as_str().unwrap();
    assert_ne!(id_a, id_b);
}

#[tokio::test]
async fn edge_state_lifecycle_filter_and_transition() {
    let app = test_app().await;
    let (_, a) = send(&app.router, "POST", "/api/entity", Some(json!({"module": "tasks", "type": "task"}))).await;
    let (_, b) = send(&app.router, "POST", "/api/entity", Some(json!({"module": "tasks", "type": "task"}))).await;
    let (src, dst) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());

    // Create a pending edge.
    let (st, edge) = send(
        &app.router,
        "POST",
        "/api/edge",
        Some(json!({"src_id": src, "dst_id": dst, "rel": "depends_on", "state": "pending"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(edge["state"], "pending");
    let edge_id = edge["id"].as_str().unwrap();

    // Filter by state surfaces it under pending, not accepted.
    let (_, pending) = send(&app.router, "GET", "/api/edge?state=pending", None).await;
    assert_eq!(pending.as_array().unwrap().len(), 1);
    let (_, accepted) = send(&app.router, "GET", "/api/edge?state=accepted", None).await;
    assert_eq!(accepted.as_array().unwrap().len(), 0);

    // Transition pending -> accepted.
    let (st, moved) = send(
        &app.router,
        "PATCH",
        &format!("/api/edge/{edge_id}"),
        Some(json!({"state": "accepted"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(moved["state"], "accepted");

    // Now it lists under accepted.
    let (_, accepted2) = send(&app.router, "GET", "/api/edge?state=accepted", None).await;
    assert_eq!(accepted2.as_array().unwrap().len(), 1);

    // Patching a missing edge -> 404.
    let (st, _) = send(&app.router, "PATCH", "/api/edge/edg_missing", Some(json!({"state": "accepted"}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn events_are_append_only_no_mutation_routes() {
    let app = test_app().await;
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/event",
        Some(json!({"type": "study.review", "actor": "user"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // No update/delete route exists -> 405.
    for method in ["PUT", "PATCH", "DELETE"] {
        let (st, _) = send(&app.router, method, "/api/event", None).await;
        assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED, "{method} must be 405");
    }
}

#[tokio::test]
async fn events_can_be_filtered_by_run_id() {
    // Issue #92: the frontend polls a pipeline run's stage events by job id
    // (`run_id`) before it knows the `pipeline_run` entity id
    // `process_pipeline_job` creates internally.
    let app = test_app().await;
    send(
        &app.router,
        "POST",
        "/api/event",
        Some(json!({"type": "pipeline.stage.completed", "run_id": "job_run_a"})),
    )
    .await;
    send(
        &app.router,
        "POST",
        "/api/event",
        Some(json!({"type": "pipeline.stage.completed", "run_id": "job_run_b"})),
    )
    .await;

    let (st, body) = send(&app.router, "GET", "/api/event?run_id=job_run_a", None).await;
    assert_eq!(st, StatusCode::OK);
    let events = body.as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["run_id"], "job_run_a");
}

#[tokio::test]
async fn module_request_queues_a_build_job() {
    let app = test_app().await;
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/module-request",
        Some(json!({"prompt": "add a reading module"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "queued");

    let (_, jobs) = send(&app.router, "GET", "/api/jobs?kind=module_build", None).await;
    assert_eq!(jobs.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn module_request_get_one_returns_the_queued_request_a_requester_polls() {
    let app = test_app().await;
    let (_, created) = send(
        &app.router,
        "POST",
        "/api/module-request",
        Some(json!({"prompt": "add a reading module"})),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (st, body) = send(&app.router, "GET", &format!("/api/module-request/{id}"), None).await;

    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["status"], "queued");
    assert_eq!(body["prompt"], "add a reading module");
    assert!(body["error"].is_null());
}

#[tokio::test]
async fn module_request_get_one_404s_for_an_unknown_id() {
    let app = test_app().await;
    let (st, _) = send(&app.router, "GET", "/api/module-request/req_does_not_exist", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agents_endpoint_lists_detected_agents() {
    let app = test_app().await;
    let (st, body) = send(&app.router, "GET", "/api/agents", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["agents"].is_array());
    // `default` is null only if no agent CLI is installed; either way the key exists.
    assert!(body.get("default").is_some());
}

#[tokio::test]
async fn metrics_aggregates_over_the_workspace() {
    let app = test_app().await;
    send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"module": "tasks", "type": "task", "title": "a"})),
    )
    .await;
    let (st, body) = send(&app.router, "GET", "/api/metrics", None).await;
    assert_eq!(st, StatusCode::OK);
    // One entity created + its entity.created event.
    assert_eq!(body["entities"], 1);
    assert!(body["events"].as_i64().unwrap() >= 1);
    assert_eq!(body["entities_by_module"]["tasks"], 1);
    // Completeness (#6): grouped by type, and events/jobs rollups present.
    assert_eq!(body["entities_by_type"]["task"], 1);
    assert_eq!(body["events_by_type"]["entity.created"], 1);
    assert!(body["jobs_by_status"].is_object());
    // Harness rollup keys exist and are numeric (COALESCE'd, never null).
    assert!(body["tokens_in"].is_number());
    assert!(body["cost"].is_number());
    assert!(body["gated_actions"].is_number());
}

#[tokio::test]
async fn metrics_breaks_down_events_by_tier_and_phase() {
    let app = test_app().await;
    send(
        &app.router,
        "POST",
        "/api/event",
        Some(json!({"type": "harness.run", "tier": "mac"})),
    )
    .await;
    send(
        &app.router,
        "POST",
        "/api/event",
        Some(json!({"type": "pipeline.stage.completed", "attrs": {"stage": "verify"}})),
    )
    .await;
    let (st, body) = send(&app.router, "GET", "/api/metrics", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["events_by_tier"]["mac"], 1);
    assert_eq!(body["events_by_phase"]["verify"], 1);
}

#[tokio::test]
async fn planned_routes_are_honest() {
    let app = test_app().await;
    // ingest enqueues a job -> 202.
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/ingest",
        Some(json!({"uri": "s3://x/a.mp4", "kind": "video"})),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "queued");
}

/// Issue #94: the frontend inspects registered pipelines' DAG stages
/// instead of hardcoding them - the route just mirrors
/// `lifeos_pipelines::pipeline_registry()` as JSON.
#[tokio::test]
async fn pipeline_registry_lists_post_from_topic_with_its_four_stages() {
    let app = test_app().await;
    let (st, body) = send(&app.router, "GET", "/api/pipeline/registry", None).await;
    assert_eq!(st, StatusCode::OK);
    let pipelines = body.as_array().unwrap();
    let post_from_topic = pipelines.iter().find(|p| p["id"] == "post-from-topic").unwrap();
    let stages = post_from_topic["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 4);
    let names: Vec<&str> = stages.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["research", "draft", "verify", "publish"]);
    assert_eq!(stages[3]["gated"], true);
}

/// Issue #88: `entity_id` round-trips into the enqueued job's payload so
/// `lifeos-drain`/`lifeos-ingest` know which entity to attach segments to.
#[tokio::test]
async fn ingest_carries_entity_id_into_the_enqueued_job_payload() {
    let app = test_app().await;
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/ingest",
        Some(json!({"entity_id": "ent_notes", "uri": "s3://x/notes.txt", "kind": "text"})),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let job_id = body["job_id"].as_str().unwrap();

    let (_, jobs) = send(&app.router, "GET", "/api/jobs", None).await;
    let job = jobs.as_array().unwrap().iter().find(|j| j["id"] == job_id).unwrap();
    assert_eq!(job["payload"]["entity_id"], "ent_notes");
}

/// `/api/vcs/*` used to be a 501 stub here; it's real now (issue #86) -
/// commit -> history -> checkout end-to-end through the actual CAS store.
#[tokio::test]
async fn vcs_routes_version_a_file_end_to_end() {
    let app = test_app().await;
    use base64::Engine as _;
    let content_base64 = base64::engine::general_purpose::STANDARD.encode(b"hello vcs");

    let (st, commit_body) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "notes.txt", "content_base64": content_base64, "message": "first"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let entity_id = commit_body["id"].as_str().unwrap().to_string();
    let blob_ref = commit_body["blob_ref"].as_str().unwrap().to_string();

    let (st, history_body) = send(&app.router, "GET", &format!("/api/vcs/history?entity_id={entity_id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(history_body.as_array().unwrap().len(), 1);
    assert_eq!(history_body[0]["blob_ref"], blob_ref);

    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/vcs/checkout?entity_id={entity_id}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"hello vcs");
}

/// `/api/vcs/diff` (issue #87): real text diff between two committed
/// versions of the same file.
#[tokio::test]
async fn vcs_diff_computes_a_real_diff_between_two_text_versions() {
    let app = test_app().await;
    use base64::Engine as _;
    let encode = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);

    let (_, first) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "notes.txt", "content_base64": encode("line one\n"), "message": "first"})),
    )
    .await;
    let entity_id = first["id"].as_str().unwrap().to_string();
    let old_ref = first["blob_ref"].as_str().unwrap().to_string();

    let (st, second) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"entity_id": entity_id, "name": "notes.txt", "content_base64": encode("line one\nline two\n"), "message": "second"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let new_ref = second["blob_ref"].as_str().unwrap().to_string();

    let (st, diff_body) = send(
        &app.router,
        "GET",
        &format!("/api/vcs/diff?entity_id={entity_id}&old={old_ref}&new={new_ref}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(diff_body["supported"], true);
    assert_eq!(diff_body["inserted"], 1);
    assert_eq!(diff_body["deleted"], 0);
    assert_eq!(diff_body["summary"], "1 line added");
}

/// A binary-ish extension gets an honest `supported: false` + the specific
/// blocking issue rather than a silent no-op or a fake diff (issue #85/#87).
#[tokio::test]
async fn vcs_diff_names_the_blocking_issue_for_unsupported_types() {
    let app = test_app().await;
    use base64::Engine as _;
    let encode = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);

    let (_, first) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "cover.png", "content_base64": encode("fake image bytes a"), "message": "first"})),
    )
    .await;
    let entity_id = first["id"].as_str().unwrap().to_string();
    let old_ref = first["blob_ref"].as_str().unwrap().to_string();

    let (_, second) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"entity_id": entity_id, "name": "cover.png", "content_base64": encode("fake image bytes b"), "message": "second"})),
    )
    .await;
    let new_ref = second["blob_ref"].as_str().unwrap().to_string();

    let (st, diff_body) = send(
        &app.router,
        "GET",
        &format!("/api/vcs/diff?entity_id={entity_id}&old={old_ref}&new={new_ref}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(diff_body["supported"], false);
    assert_eq!(diff_body["kind"], "image");
    assert!(diff_body["blocked_by"].as_str().unwrap().contains("#90"));
}

/// `/api/vcs/branch`, `/api/vcs/tag`, `/api/vcs/refs`, `/api/vcs/snapshot`
/// (issue #87): forward-only ref creation + read-back, no branch-force route
/// exists anywhere (docs/AGENT-CONTROL.md §1).
#[tokio::test]
async fn vcs_branch_and_tag_snapshot_current_state_and_are_listable() {
    let app = test_app().await;
    use base64::Engine as _;
    let (_, commit_body) = send(
        &app.router,
        "POST",
        "/api/vcs/commit",
        Some(json!({"name": "notes.txt", "content_base64": base64::engine::general_purpose::STANDARD.encode("v1"), "message": "first"})),
    )
    .await;
    let entity_id = commit_body["id"].as_str().unwrap().to_string();
    let blob_ref = commit_body["blob_ref"].as_str().unwrap().to_string();

    let (st, branch_body) = send(&app.router, "POST", "/api/vcs/branch", Some(json!({"name": "main"}))).await;
    assert_eq!(st, StatusCode::OK);
    let snapshot_ref = branch_body["snapshot_ref"].as_str().unwrap().to_string();

    let (st, tag_body) = send(&app.router, "POST", "/api/vcs/tag", Some(json!({"name": "v1"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(tag_body["name"], "v1");

    // Setting the same tag again to the identical current state is a no-op, not an error.
    let (st, _) = send(&app.router, "POST", "/api/vcs/tag", Some(json!({"name": "v1"}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, branches) = send(&app.router, "GET", "/api/vcs/refs?kind=branch", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(branches[0]["name"], "main");

    let (st, tags) = send(&app.router, "GET", "/api/vcs/refs?kind=tag", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(tags[0]["name"], "v1");

    let (st, manifest) = send(&app.router, "GET", &format!("/api/vcs/snapshot?snapshot_ref={snapshot_ref}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(manifest["entries"][entity_id.as_str()], blob_ref);
}

#[tokio::test]
async fn unknown_workspace_is_rejected() {
    let app = test_app().await;
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"workspace_id": "ws_nope", "module": "tasks", "type": "task"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("workspace"));
}

/// docs/DATA-MODEL.md §4.2: sync is last-push-wins over the whole `attrs`
/// blob, not LWW on `updated_at`. Force a row-level conflict (two divergent
/// writers patching the same entity) and confirm POST .../reconcile repairs
/// the row from the append-only `events` log.
#[tokio::test]
async fn reconcile_replays_events_after_forced_conflict() {
    let app = test_app().await;
    let (_, ent) = send(
        &app.router,
        "POST",
        "/api/entity",
        Some(json!({"module": "tasks", "type": "task", "attrs": {}})),
    )
    .await;
    let id = ent["id"].as_str().unwrap();

    // Two divergent writers (bot lane, Mac lane) each PATCH attrs - both
    // events land in the log, conflict-free.
    let (_, after_bot) = send(
        &app.router,
        "PATCH",
        &format!("/api/entity/{id}"),
        Some(json!({"attrs": {"status": "todo"}})),
    )
    .await;
    assert_eq!(after_bot["attrs"]["status"], "todo");

    let (_, after_mac) = send(
        &app.router,
        "PATCH",
        &format!("/api/entity/{id}"),
        Some(json!({"attrs": {"status": "done"}})),
    )
    .await;
    assert_eq!(after_mac["attrs"]["status"], "done");

    // Force the conflict the way it actually happens: an out-of-band Turso
    // sync pull overwrites the row directly (no API call, no new event) with
    // a stale push - the bot's older write - landing after the Mac's,
    // because last-push-wins cares about sync arrival order, not causal
    // order. Simulate that with a raw connection to the same file, bypassing
    // the handler/event-emit path entirely.
    let raw = libsql::Builder::new_local(&app.db_path).build().await.unwrap();
    let raw_conn = raw.connect().unwrap();
    raw_conn
        .execute(
            "UPDATE entities SET attrs = '{\"status\":\"todo\"}' WHERE id = ?1",
            libsql::params![id],
        )
        .await
        .unwrap();
    let (_, forced) = send(&app.router, "GET", &format!("/api/entity/{id}"), None).await;
    assert_eq!(forced["attrs"]["status"], "todo");

    // Reconcile replays events in causal order; the last attrs-bearing event
    // wins, restoring "done" - the intended final state.
    let (st, reconciled) = send(&app.router, "POST", &format!("/api/entity/{id}/reconcile"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(reconciled["attrs"]["status"], "done");

    let (_, fetched) = send(&app.router, "GET", &format!("/api/entity/{id}"), None).await;
    assert_eq!(fetched["attrs"]["status"], "done");
}

#[tokio::test]
async fn config_draft_shadow_promote_rollback_flips_the_active_pointer() {
    let app = test_app().await;

    // Draft.
    let (st, cfg1) = send(
        &app.router,
        "POST",
        "/api/configs",
        Some(json!({"kind": "route_prior", "payload": {"bias": {"skill:foo": -0.3}}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cfg1["status"], "draft");
    let id1 = cfg1["id"].as_str().unwrap().to_string();

    // Shadow.
    let (st, cfg1) = send(
        &app.router,
        "POST",
        &format!("/api/configs/{id1}/shadow"),
        Some(json!({"shadow_summary": {"entries_replayed": 40, "rank_changes": 3}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cfg1["status"], "shadow");
    assert_eq!(cfg1["shadow_summary"]["entries_replayed"], 40);

    // Promote.
    let (st, cfg1) = send(&app.router, "POST", &format!("/api/configs/{id1}/promote"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cfg1["status"], "promoted");

    let (_, listing) = send(&app.router, "GET", "/api/configs?kind=route_prior", None).await;
    assert_eq!(listing["active"]["route_prior"], id1);

    let (_, events) = send(&app.router, "GET", "/api/event?type=config.promoted", None).await;
    assert_eq!(events[0]["attrs"]["config_id"], id1);

    // A second candidate, promoted - becomes active.
    let (_, cfg2) = send(
        &app.router,
        "POST",
        "/api/configs",
        Some(json!({"kind": "route_prior", "payload": {"bias": {"skill:bar": -0.5}}})),
    )
    .await;
    let id2 = cfg2["id"].as_str().unwrap().to_string();
    send(&app.router, "POST", &format!("/api/configs/{id2}/promote"), None).await;

    let (_, listing) = send(&app.router, "GET", "/api/configs?kind=route_prior", None).await;
    assert_eq!(listing["active"]["route_prior"], id2);

    // Rollback reverts the active pointer to the first promoted config.
    let (st, rolled) = send(
        &app.router,
        "POST",
        "/api/configs/rollback",
        Some(json!({"kind": "route_prior"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rolled["id"], id1);

    let (_, listing) = send(&app.router, "GET", "/api/configs?kind=route_prior", None).await;
    assert_eq!(listing["active"]["route_prior"], id1);

    let (_, events) = send(&app.router, "GET", "/api/event?type=config.rolledback", None).await;
    assert_eq!(events[0]["attrs"]["config_id"], id1);
}

#[tokio::test]
async fn config_rollback_with_no_prior_promotion_is_rejected() {
    let app = test_app().await;
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/configs/rollback",
        Some(json!({"kind": "route_prior"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// Issue #101: marketplace publish/verify are honest 501 without a
/// configured signing key, rather than signing with an implicit key.
#[tokio::test]
async fn marketplace_publish_is_not_implemented_without_a_signing_key() {
    let app = test_app().await;
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/marketplace/publish",
        Some(json!({"module_id": "reading", "version": "1.0.0", "manifest": {"id": "reading", "version": "1.0.0"}})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
}

/// Issues #101/#102: publish signs a manifest, install re-verifies it, and
/// a tampered manifest fails verification rather than silently installing.
#[tokio::test]
async fn marketplace_publish_verify_install_and_tamper_detection() {
    let app = test_app_with_marketplace().await;
    let manifest = json!({"id": "reading", "version": "1.0.0", "views": ["list"]});

    let (st, published) = send(
        &app.router,
        "POST",
        "/api/marketplace/publish",
        Some(json!({"module_id": "reading", "version": "1.0.0", "manifest": manifest})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let package_id = published["package_id"].as_str().unwrap().to_string();
    let signature = published["signature"].as_str().unwrap().to_string();
    let pubkey = published["pubkey"].as_str().unwrap().to_string();

    // Publish rejects a manifest.id/version mismatch (structural check).
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/marketplace/publish",
        Some(json!({"module_id": "reading", "version": "1.0.0", "manifest": {"id": "wrong", "version": "1.0.0"}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // The published package appears in the browse listing.
    let (st, listed) = send(&app.router, "GET", "/api/marketplace/packages", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(listed["packages"].as_array().unwrap().len(), 1);

    // Direct verify: correct manifest+signature+pubkey passes.
    let (st, verified) = send(
        &app.router,
        "POST",
        "/api/marketplace/verify",
        Some(json!({"manifest": manifest, "signature": signature, "pubkey": pubkey})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(verified["valid"], true);

    // A tampered manifest fails verification (issue #101 acceptance).
    let tampered = json!({"id": "reading", "version": "9.9.9", "views": ["list"]});
    let (st, verified) = send(
        &app.router,
        "POST",
        "/api/marketplace/verify",
        Some(json!({"manifest": tampered, "signature": signature, "pubkey": pubkey})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(verified["valid"], false);

    // Install re-verifies the stored package and records the event.
    let (st, installed) = send(
        &app.router,
        "POST",
        "/api/marketplace/install",
        Some(json!({"package_id": package_id})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(installed["installed"], true);

    let (_, events) = send(&app.router, "GET", "/api/event?type=marketplace.installed", None).await;
    assert_eq!(events.as_array().unwrap().len(), 1);

    // Installing an unknown package -> 404.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/marketplace/install",
        Some(json!({"package_id": "pkg_does_not_exist"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Issue #101: `/api/marketplace/pubkey` exposes the platform's public key
/// once configured.
#[tokio::test]
async fn marketplace_pubkey_is_exposed_once_configured() {
    let app = test_app_with_marketplace().await;
    let (st, body) = send(&app.router, "GET", "/api/marketplace/pubkey", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["pubkey"].as_str().unwrap().len() > 10);
}

/// Issue #103: subscribe is idempotent (re-subscribing the same endpoint
/// upserts, doesn't duplicate) and unsubscribe removes it.
#[tokio::test]
async fn push_subscribe_upserts_and_unsubscribe_removes() {
    let app = test_app().await;
    let sub = json!({"endpoint": "https://push.example/abc", "keys": {"p256dh": "x", "auth": "y"}});

    let (st, body) = send(&app.router, "POST", "/api/push/subscribe", Some(sub.clone())).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["subscribed"], true);

    // Re-subscribing the same endpoint is a no-op upsert, not an error.
    let (st, _) = send(&app.router, "POST", "/api/push/subscribe", Some(sub)).await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = send(
        &app.router,
        "POST",
        "/api/push/unsubscribe",
        Some(json!({"endpoint": "https://push.example/abc"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["unsubscribed"], true);
}

/// Issue #104: no billing/quota gating - `/api/workspace/provision-db` is
/// an honest 501 without Turso platform credentials, never a fake success,
/// and there is no plan/quota check anywhere in the path.
#[tokio::test]
async fn provision_db_is_not_implemented_without_turso_platform_credentials() {
    let app = test_app().await;
    let (st, _) = send(&app.router, "POST", "/api/workspace/provision-db", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);

    let (st, body) = send(&app.router, "GET", "/api/workspace/database", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["provisioned"], false);
}
