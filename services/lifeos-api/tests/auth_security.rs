//! Auth security-audit regression tests (findings 2, 17, 46).
//!
//! Each test builds the real router against a throwaway libSQL file and drives
//! it with `tower::ServiceExt::oneshot` - no network, no port binding. Where a
//! test needs a genuinely passwordless account (which the normal register/seed
//! flows no longer produce, by design), it opens a second raw connection to the
//! same file and inserts one directly - the same pattern the other integration
//! suites use.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, config::DEFAULT_WORKSPACE, ids::new_id, routes};
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
        .join(format!("lifeos_authsec_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path)).await.expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

/// Insert a genuinely passwordless account (NULL `password_hash`) plus its owner
/// membership, via a raw second connection to the same file. This models a
/// legacy pre-#100 row - the only kind of account the local set-password
/// bootstrap is meant to migrate.
async fn insert_passwordless_account(db_path: &str, user_id: &str, email: &str) {
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    conn.execute(
        "INSERT INTO users (id, email, name, created_at, updated_at) VALUES (?1, ?2, ?3, 1, 1)",
        libsql::params![user_id, email, "Legacy User"],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memberships (id, user_id, workspace_id, role, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'owner', 1, 1)",
        libsql::params![format!("memb_{user_id}"), user_id, DEFAULT_WORKSPACE],
    )
    .await
    .unwrap();
}

/// Send a request with optional extra headers; return `(status, parsed-json)`.
async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
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

// --- Finding 2(a): the local-only set-password bootstrap is loopback-gated. ---

#[tokio::test]
async fn set_password_is_rejected_from_a_non_loopback_origin() {
    let app = test_app().await;
    insert_passwordless_account(&app.db_path, "usr_legacy_a", "legacy-a@test.local").await;

    // A request a fronting proxy tagged with a public client IP must be refused,
    // even though the account is passwordless and would otherwise be eligible.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "legacy-a@test.local", "password": "remote-attacker-pw"})),
        &[("x-forwarded-for", "203.0.113.9")],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "set-password from a non-loopback origin must be rejected"
    );

    // The remote attempt must not have consumed the NULL slot: a loopback-origin
    // request can still legitimately set the password.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "legacy-a@test.local", "password": "owner-chosen-pw"})),
        &[("x-forwarded-for", "127.0.0.1")],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "a loopback-origin set-password on a passwordless account must succeed"
    );

    // And that password now logs in.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "legacy-a@test.local", "password": "owner-chosen-pw"})),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "the loopback-set password must authenticate");
}

// --- Finding 2(b): the seeded owner is sealed with a non-NULL hash. ---

#[tokio::test]
async fn seeded_owner_hash_is_non_null_so_the_bootstrap_can_never_match_it() {
    let app = test_app().await;

    // Probe the actual stored hash via a raw connection: it must be non-NULL.
    // Scope the connection (and its open statement) so all locks are released
    // before the set-password write below - an idle open reader would otherwise
    // contend with the writer on the shared SQLite file.
    let hash: Option<String> = {
        let raw = libsql::Builder::new_local(&app.db_path).build().await.unwrap();
        let conn = raw.connect().unwrap();
        let mut rows = conn
            .query(
                "SELECT password_hash FROM users WHERE email = ?1",
                libsql::params!["chayan@lifeos.app"],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("seeded owner exists");
        row.get(0).unwrap()
    };
    assert!(
        hash.is_some(),
        "the seeded owner must be sealed with a non-NULL password_hash (finding 2)"
    );

    // Consequently the unauthenticated NULL-guarded bootstrap cannot take it over.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "chayan@lifeos.app", "password": "attacker-chosen-pw"})),
        &[],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "the sealed seeded owner must reject set-password (no NULL slot to fill)"
    );
}

// --- Finding 2(c): the atomic `WHERE password_hash IS NULL` CAS is preserved. ---

#[tokio::test]
async fn set_password_cas_rejects_a_second_call_on_a_passwordless_account() {
    let app = test_app().await;
    insert_passwordless_account(&app.db_path, "usr_legacy_c", "legacy-c@test.local").await;

    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "legacy-c@test.local", "password": "first-password-here"})),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "first set-password on a passwordless account must succeed");

    // The atomic guard - not an earlier read-then-write - is what closes the
    // TOCTOU race a concurrent request could otherwise win.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/account/set-password",
        Some(json!({"email": "legacy-c@test.local", "password": "second-password-here"})),
        &[],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "a second set-password call must never overwrite an already-set password"
    );

    // The first password is the active one; the second was never applied.
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "legacy-c@test.local", "password": "first-password-here"})),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "only the first password may authenticate");
    let (st, _) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "legacy-c@test.local", "password": "second-password-here"})),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "the never-applied second password must not authenticate");
}

// --- Finding 17: refresh rotation is a compare-and-swap. ---

#[tokio::test]
async fn concurrent_double_refresh_rotates_exactly_one_session() {
    let app = test_app().await;
    let (_, reg) = send(
        &app.router,
        "POST",
        "/api/register",
        Some(json!({"email": "race@test.local", "name": "R", "password": "race-password-123", "workspace_name": "WS"})),
        &[],
    )
    .await;
    let refresh = reg["refresh_token"].as_str().unwrap().to_string();

    // Two concurrent replays of the SAME refresh token: without the CAS both
    // would pass the `revoked_at IS NULL` SELECT and both mint a fresh session
    // (two valid sessions from one token). With the CAS exactly one wins.
    let body = json!({ "refresh_token": refresh }).to_string();
    let req1 = Request::builder()
        .method("POST")
        .uri("/api/session/refresh")
        .header("content-type", "application/json")
        .body(Body::from(body.clone()))
        .unwrap();
    let req2 = Request::builder()
        .method("POST")
        .uri("/api/session/refresh")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let (r1, r2) = tokio::join!(app.router.clone().oneshot(req1), app.router.clone().oneshot(req2));
    let s1 = r1.unwrap().status();
    let s2 = r2.unwrap().status();

    let ok = [s1, s2].iter().filter(|s| **s == StatusCode::OK).count();
    let bad = [s1, s2].iter().filter(|s| **s == StatusCode::BAD_REQUEST).count();
    assert_eq!(ok, 1, "exactly one concurrent refresh may rotate the session (got {s1} and {s2})");
    assert_eq!(bad, 1, "the losing concurrent refresh must be rejected like any replay");
}

#[tokio::test]
async fn a_rotated_refresh_token_cannot_be_replayed() {
    let app = test_app().await;
    let (_, reg) = send(
        &app.router,
        "POST",
        "/api/register",
        Some(json!({"email": "replay@test.local", "name": "R", "password": "replay-password-1", "workspace_name": "WS"})),
        &[],
    )
    .await;
    let r0 = reg["refresh_token"].as_str().unwrap().to_string();

    let (st, refreshed) = send(&app.router, "POST", "/api/session/refresh", Some(json!({"refresh_token": r0})), &[]).await;
    assert_eq!(st, StatusCode::OK);
    let r1 = refreshed["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(r0, r1);

    // Replaying the now-rotated token is rejected.
    let (st, _) = send(&app.router, "POST", "/api/session/refresh", Some(json!({"refresh_token": r0})), &[]).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a rotated-away refresh token must never be reusable");
}

// --- Finding 46: no user enumeration, no upstream/DB error leakage. ---

#[tokio::test]
async fn login_returns_one_generic_error_for_unknown_email_and_wrong_password() {
    let app = test_app().await;
    let (_, _) = send(
        &app.router,
        "POST",
        "/api/register",
        Some(json!({"email": "known@test.local", "name": "K", "password": "known-password-1", "workspace_name": "WS"})),
        &[],
    )
    .await;

    let (st_unknown, body_unknown) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "does-not-exist@test.local", "password": "known-password-1"})),
        &[],
    )
    .await;
    let (st_wrong, body_wrong) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "known@test.local", "password": "wrong-password"})),
        &[],
    )
    .await;

    assert_eq!(st_unknown, StatusCode::BAD_REQUEST);
    assert_eq!(st_wrong, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_unknown, body_wrong,
        "an unknown email and a wrong password must return the identical error (no enumeration)"
    );
    assert_eq!(body_unknown["error"], "invalid email or password");
}

#[tokio::test]
async fn login_against_a_passwordless_account_returns_the_generic_error() {
    let app = test_app().await;
    insert_passwordless_account(&app.db_path, "usr_legacy_d", "legacy-d@test.local").await;

    // The old code returned a distinct "this account has no password set yet"
    // message here, leaking both that the email exists and that it is
    // un-bootstrapped. It must now be the same generic error as any other login
    // failure.
    let (st, body) = send(
        &app.router,
        "POST",
        "/api/login",
        Some(json!({"email": "legacy-d@test.local", "password": "anything-at-all"})),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid email or password");
}
