//! Finding 2(b): with `LIFEOS_ADMIN_PASSWORD` set at first boot, the seeded
//! owner is bootstrapped with that password and can log in immediately.
//!
//! This lives in its own single-test binary on purpose: `LIFEOS_ADMIN_PASSWORD`
//! is a process-global env var read by `db::seed`, and mutating it from one of
//! several parallel tests in a shared binary would race other tests' seeds. As
//! the only test in this binary it mutates the env with no concurrent reader.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

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

async fn send(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn admin_password_env_seeds_a_loginable_owner() {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_adminseed_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);

    // Set BEFORE building state so the very first seed picks it up. Sequenced
    // before the `build_state` await below, so no thread reads it concurrently.
    std::env::set_var("LIFEOS_ADMIN_PASSWORD", "correct-admin-password-1");
    let state = build_state(base_config(&db_path)).await.expect("build state");
    std::env::remove_var("LIFEOS_ADMIN_PASSWORD");
    let app = routes::router(state);

    // The owner logs in with the admin password.
    let (st, body) = send(
        &app,
        "/api/login",
        json!({"email": "chayan@lifeos.app", "password": "correct-admin-password-1"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "owner must log in with LIFEOS_ADMIN_PASSWORD");
    assert!(body["key_token"].as_str().unwrap().len() > 10);
    assert!(body["refresh_token"].as_str().unwrap().len() > 10);

    // A different password still fails - the seed hashed the real one, not a
    // wildcard.
    let (st, _) = send(
        &app,
        "/api/login",
        json!({"email": "chayan@lifeos.app", "password": "not-the-admin-password"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a wrong password must still be rejected");

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}
