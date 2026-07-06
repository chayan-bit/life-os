//! Workspace membership, roles, and invites (issue #146). Covers role
//! resolution (legacy single-user vs explicit-membership modes), the strict-
//! mode write middleware (viewer/editor/owner), the full invite lifecycle
//! (accept happy path, expired, single-use CAS double-accept), last-owner
//! protection, owner-only enforcement, and workspace isolation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::auth::{issue_token, resolve_role, Role};
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &str = "test-secret";

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

fn base_config(db_path: &str, trust_workspace_header: bool) -> Config {
    Config {
        db_path: db_path.to_string(),
        turso_url: None,
        turso_token: None,
        sync_interval_secs: 60,
        derived_db_path: format!("{db_path}.derived"),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        jwt_secret: SECRET.into(),
        trust_workspace_header,
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

async fn test_app(trust_workspace_header: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_memb_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path, trust_workspace_header))
        .await
        .expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
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

/// Register a fresh tenant; returns (workspace_id, user_id, key_token).
async fn register(app: &Router, name: &str) -> (String, String, String) {
    let (st, body) = send_h(
        app,
        "POST",
        "/api/register",
        Some(json!({
            "email": format!("{name}@test.example"),
            "name": name,
            "password": "test-password-123",
            "workspace_name": name,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "register {name}: {body:?}");
    (
        body["workspace_id"].as_str().unwrap().to_string(),
        body["user_id"].as_str().unwrap().to_string(),
        body["key_token"].as_str().unwrap().to_string(),
    )
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Directly seat a member row (bypassing invites) so a test can arrange a
/// specific role matrix without the full invite dance.
async fn seat_member(db_path: &str, workspace: &str, user_id: &str, role: &str) {
    let raw = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    conn.execute(
        "INSERT INTO workspace_members (workspace_id, user_id, role, invited_by, created_at) \
         VALUES (?1, ?2, ?3, NULL, 1) ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
        libsql::params![workspace, user_id, role],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn resolve_role_legacy_owner_member_and_stranger_modes() {
    let app = test_app(true).await;
    let (workspace, user_id, _tok) = register(&app.router, "role-res").await;

    let raw = libsql::Builder::new_local(&app.db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();

    // Legacy single-user mode: no workspace_members rows at all -> Owner, for
    // any caller (personal deployments keep working untouched).
    assert_eq!(
        resolve_role(&conn, &workspace, Some(&user_id)).await.unwrap(),
        Role::Owner
    );
    assert_eq!(
        resolve_role(&conn, &workspace, Some("anyone")).await.unwrap(),
        Role::Owner
    );

    // With an explicit membership row, the stored role is returned...
    seat_member(&app.db_path, &workspace, &user_id, "editor").await;
    assert_eq!(
        resolve_role(&conn, &workspace, Some(&user_id)).await.unwrap(),
        Role::Editor
    );
    // ...and now that rows exist, a non-member (or no identity) is denied.
    assert!(matches!(
        resolve_role(&conn, &workspace, Some("stranger")).await,
        Err(lifeos_api::error::ApiError::Forbidden(_))
    ));
    assert!(matches!(
        resolve_role(&conn, &workspace, None).await,
        Err(lifeos_api::error::ApiError::Forbidden(_))
    ));
}

#[tokio::test]
async fn invite_accept_happy_path_seats_the_member() {
    let app = test_app(true).await;
    let (workspace, owner_id, tok_o) = register(&app.router, "inv-owner").await;

    // Owner creates an invite (materializes them as owner in the process).
    let (st, inv) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "joiner@x.com", "role": "editor"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{inv:?}");
    let token = inv["token"].as_str().unwrap().to_string();
    assert!(inv["accept_url"].as_str().unwrap().contains(&token));

    // A second, logged-in user accepts it.
    let (_w2, joiner_id, tok_e) = register(&app.router, "joiner").await;
    let (st, joined) = send_h(
        &app.router,
        "POST",
        "/api/invite/accept",
        Some(json!({"token": token})),
        &[("authorization", &bearer(&tok_e))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{joined:?}");
    assert_eq!(joined["workspace_id"], workspace);
    assert_eq!(joined["role"], "editor");

    // The member list now shows both, with roles, and the owner's own role.
    let (st, body) = send_h(
        &app.router,
        "GET",
        "/api/members",
        None,
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["your_role"], "owner");
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert!(members
        .iter()
        .any(|m| m["user_id"] == json!(owner_id) && m["role"] == "owner"));
    assert!(members
        .iter()
        .any(|m| m["user_id"] == json!(joiner_id) && m["role"] == "editor"));
}

#[tokio::test]
async fn invite_accept_rejects_an_expired_invite() {
    let app = test_app(true).await;
    let (workspace, _owner_id, tok_o) = register(&app.router, "exp-owner").await;
    let (st, inv) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "late@x.com", "role": "viewer"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{inv:?}");
    let token = inv["token"].as_str().unwrap().to_string();

    // Force expiry out-of-band.
    let raw = libsql::Builder::new_local(&app.db_path).build().await.unwrap();
    let conn = raw.connect().unwrap();
    conn.execute(
        "UPDATE invites SET expires_at = 1 WHERE workspace_id = ?1",
        libsql::params![workspace],
    )
    .await
    .unwrap();

    let (_w, _uid, tok_e) = register(&app.router, "late-joiner").await;
    let (st, body) = send_h(
        &app.router,
        "POST",
        "/api/invite/accept",
        Some(json!({"token": token})),
        &[("authorization", &bearer(&tok_e))],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("expired"));
}

#[tokio::test]
async fn invite_accept_is_single_use_via_cas() {
    let app = test_app(true).await;
    let (_workspace, _owner_id, tok_o) = register(&app.router, "cas-owner").await;
    let (st, inv) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "twice@x.com", "role": "viewer"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{inv:?}");
    let token = inv["token"].as_str().unwrap().to_string();

    let (_w, _uid, tok_e) = register(&app.router, "twice-joiner").await;
    let hdr = [("authorization", bearer(&tok_e))];
    let hdr_ref: Vec<(&str, &str)> = hdr.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let (st, _) = send_h(&app.router, "POST", "/api/invite/accept", Some(json!({"token": token})), &hdr_ref).await;
    assert_eq!(st, StatusCode::OK, "first accept succeeds");

    let (st, body) = send_h(&app.router, "POST", "/api/invite/accept", Some(json!({"token": token})), &hdr_ref).await;
    assert_eq!(st, StatusCode::CONFLICT, "a second accept must lose the CAS: {body:?}");
}

#[tokio::test]
async fn the_last_owner_cannot_be_demoted_or_removed() {
    let app = test_app(true).await;
    let (_workspace, owner_id, tok_o) = register(&app.router, "last-owner").await;
    // Create any invite to materialize the owner into workspace_members.
    send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "x@x.com", "role": "viewer"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;

    let (st, body) = send_h(
        &app.router,
        "POST",
        &format!("/api/member/{owner_id}/role"),
        Some(json!({"role": "editor"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(body["error"].as_str().unwrap().contains("last owner"));

    let (st, body) = send_h(
        &app.router,
        "DELETE",
        &format!("/api/member/{owner_id}"),
        None,
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(body["error"].as_str().unwrap().contains("last owner"));
}

#[tokio::test]
async fn a_non_owner_cannot_invite_or_change_roles() {
    let app = test_app(true).await;
    let (workspace, owner_id, _tok_o) = register(&app.router, "no-owner").await;
    let (_we, editor_id, _tok_e_home) = register(&app.router, "an-editor").await;

    // Seat the owner + an editor explicitly.
    seat_member(&app.db_path, &workspace, &owner_id, "owner").await;
    seat_member(&app.db_path, &workspace, &editor_id, "editor").await;
    let tok_e = issue_token(SECRET, &editor_id, &workspace, "an-editor@test.example");

    // Editor cannot create an invite (owner-only, enforced in the handler even
    // in local-first mode where the middleware is a pass-through).
    let (st, _) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "z@x.com", "role": "viewer"})),
        &[("authorization", &bearer(&tok_e))],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Editor cannot change another member's role either.
    let (st, _) = send_h(
        &app.router,
        "POST",
        &format!("/api/member/{owner_id}/role"),
        Some(json!({"role": "viewer"})),
        &[("authorization", &bearer(&tok_e))],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn strict_mode_gates_writes_by_role() {
    let app = test_app(false).await;
    let (workspace, owner_id, tok_o) = register(&app.router, "strict-owner").await;
    let (_we, viewer_id, _t1) = register(&app.router, "strict-viewer").await;
    let (_wd, editor_id, _t2) = register(&app.router, "strict-editor").await;

    seat_member(&app.db_path, &workspace, &owner_id, "owner").await;
    seat_member(&app.db_path, &workspace, &viewer_id, "viewer").await;
    seat_member(&app.db_path, &workspace, &editor_id, "editor").await;

    // Tokens scoped to `workspace` (as a real "switch workspace" flow would mint).
    let tok_viewer = issue_token(SECRET, &viewer_id, &workspace, "strict-viewer@test.example");
    let tok_editor = issue_token(SECRET, &editor_id, &workspace, "strict-editor@test.example");

    let entity = json!({"module": "tasks", "type": "task", "title": "t"});

    // Viewer: reads free, all writes denied.
    let (st, _) = send_h(&app.router, "GET", "/api/entity", None, &[("authorization", &bearer(&tok_viewer))]).await;
    assert_eq!(st, StatusCode::OK, "viewer reads are free");
    let (st, _) = send_h(&app.router, "POST", "/api/entity", Some(entity.clone()), &[("authorization", &bearer(&tok_viewer))]).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "viewer writes are denied");

    // Editor: ordinary writes allowed, security-sensitive writes denied.
    let (st, _) = send_h(&app.router, "POST", "/api/entity", Some(entity.clone()), &[("authorization", &bearer(&tok_editor))]).await;
    assert_eq!(st, StatusCode::OK, "editor ordinary write allowed");
    let (st, _) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "e@x.com", "role": "viewer"})),
        &[("authorization", &bearer(&tok_editor))],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "editor denied on security-sensitive route");

    // Owner: ordinary and security-sensitive writes both allowed.
    let (st, _) = send_h(&app.router, "POST", "/api/entity", Some(entity), &[("authorization", &bearer(&tok_o))]).await;
    assert_eq!(st, StatusCode::OK, "owner ordinary write allowed");
    let (st, body) = send_h(
        &app.router,
        "POST",
        "/api/invite",
        Some(json!({"email": "e2@x.com", "role": "editor"})),
        &[("authorization", &bearer(&tok_o))],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "owner security-sensitive write allowed: {body:?}");
}

#[tokio::test]
async fn members_are_workspace_isolated() {
    let app = test_app(true).await;
    let (_wa, owner_a, tok_a) = register(&app.router, "iso-a").await;
    let (_wb, owner_b, tok_b) = register(&app.router, "iso-b").await;

    // Give each workspace explicit membership.
    send_h(&app.router, "POST", "/api/invite", Some(json!({"email": "a@x.com", "role": "viewer"})), &[("authorization", &bearer(&tok_a))]).await;
    send_h(&app.router, "POST", "/api/invite", Some(json!({"email": "b@x.com", "role": "viewer"})), &[("authorization", &bearer(&tok_b))]).await;

    // A's member list contains A's owner and never B's.
    let (st, body) = send_h(&app.router, "GET", "/api/members", None, &[("authorization", &bearer(&tok_a))]).await;
    assert_eq!(st, StatusCode::OK);
    let ids: Vec<&str> = body["members"].as_array().unwrap().iter().map(|m| m["user_id"].as_str().unwrap()).collect();
    assert!(ids.contains(&owner_a.as_str()));
    assert!(!ids.contains(&owner_b.as_str()), "workspace B's owner leaked into A's member list");
}
