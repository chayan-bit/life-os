//! Finding 20: the unauthenticated auth endpoints are per-IP rate limited.
//!
//! Its own single-test binary because it mutates the process-global
//! `LIFEOS_AUTH_RATE_PER_MIN` env var (read once when the router builds its
//! limiter); a sibling parallel test could otherwise observe a different limit.
//! The limit is injected low (3) so the window budget is exhausted with a
//! handful of requests - no real sleeping, fully deterministic. The client IP is
//! supplied via `X-Forwarded-For`, which the limiter honors in local-first mode
//! (`trust_workspace_header = true`), so distinct IPs are simulated under
//! `oneshot` without real sockets.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
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

/// A login attempt for an unknown account from `client_ip`. The login itself
/// fails (400), but the rate limiter runs first and either lets it through or
/// returns 429 - which is exactly what we assert on.
async fn login_from(app: &Router, client_ip: &str) -> StatusCode {
    let request = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header("content-type", "application/json")
        .header("x-forwarded-for", client_ip)
        .body(Body::from(
            serde_json::json!({ "email": "nobody@test.local", "password": "whatever-123" })
                .to_string(),
        ))
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn auth_endpoints_are_rate_limited_per_ip() {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_ratelimit_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);

    // Injected low limit, read when `routes::router` builds its limiter. Cleared
    // only AFTER the router is built (the limiter is constructed there, not in
    // `build_state`), so this single-test binary leaves no lingering global.
    std::env::set_var("LIFEOS_AUTH_RATE_PER_MIN", "3");
    let state = build_state(base_config(&db_path)).await.expect("build state");
    let app = routes::router(state);
    std::env::remove_var("LIFEOS_AUTH_RATE_PER_MIN");

    let ip_a = "198.51.100.7";
    let ip_b = "198.51.100.8";

    // A normal single request passes (never 429).
    assert_ne!(
        login_from(&app, ip_a).await,
        StatusCode::TOO_MANY_REQUESTS,
        "a single request must not be throttled"
    );
    // Two more from IP A stay within the budget of 3.
    assert_ne!(login_from(&app, ip_a).await, StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(login_from(&app, ip_a).await, StatusCode::TOO_MANY_REQUESTS);
    // The fourth from IP A exceeds the window budget.
    assert_eq!(
        login_from(&app, ip_a).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the over-budget request must be rejected with 429"
    );

    // A different IP is unaffected by IP A's traffic - its own budget is intact.
    assert_ne!(
        login_from(&app, ip_b).await,
        StatusCode::TOO_MANY_REQUESTS,
        "a different IP must not be throttled by another IP's traffic"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{db_path}.derived"));
}
