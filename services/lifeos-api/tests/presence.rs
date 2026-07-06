//! Live activity feed + presence (issue #150). Covers the presence heartbeat
//! UPSERT + TTL/workspace-scoped listing over HTTP, and the activity feed's
//! since-cursor query directly (module/kind filters, workspace isolation, the
//! ULID `id > cursor` tail) - the SSE handler wraps exactly this query, so
//! testing the query is the meaningful coverage without driving a never-ending
//! stream end-to-end.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::routes::presence::{active_users, events_since};
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use libsql::Connection;
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

async fn test_app() -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_presence_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path, true))
        .await
        .expect("build state");
    TestApp {
        router: routes::router(state),
        db_path,
    }
}

/// A raw connection to the same file, for arranging fixtures / direct queries.
async fn raw_conn(db_path: &str) -> Connection {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    conn
}

async fn send(
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
    let (st, body) = send(
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

/// Seed an event row directly (bypassing the HTTP layer) with a controllable id
/// so cursor tests are deterministic. `id` must keep the `evt_<ULID>` shape so
/// string ordering matches production.
async fn seed_event(
    conn: &Connection,
    id: &str,
    workspace: &str,
    ev_type: &str,
    entity_id: Option<&str>,
) {
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
         VALUES (?1, ?2, 1, ?3, ?4, 'user', '{}')",
        libsql::params![id, workspace, ev_type, entity_id],
    )
    .await
    .unwrap();
}

async fn seed_entity(conn: &Connection, id: &str, workspace: &str, module: &str, ty: &str) {
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, attrs, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, '{}', 1, 1)",
        libsql::params![id, workspace, module, ty],
    )
    .await
    .unwrap();
}

// --------------------------------------------------------------- presence HTTP

#[tokio::test]
async fn ping_upserts_a_single_presence_row_and_list_returns_the_caller() {
    let app = test_app().await;
    let (workspace, user_id, tok) = register(&app.router, "pres-a").await;
    let auth = bearer(&tok);
    let hdrs: &[(&str, &str)] = &[("authorization", &auth)];

    // Two pings from the same user upsert one row (no append/flood).
    let (st1, _) = send(&app.router, "POST", "/api/presence/ping", Some(json!({})), hdrs).await;
    assert_eq!(st1, StatusCode::OK);
    let (st2, body2) = send(&app.router, "POST", "/api/presence/ping", Some(json!({})), hdrs).await;
    assert_eq!(st2, StatusCode::OK);
    assert_eq!(body2["user_id"].as_str().unwrap(), user_id);

    // Exactly one row for this (workspace, user) - the UPSERT collapsed both.
    let conn = raw_conn(&app.db_path).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM presence WHERE workspace_id = ?1 AND user_id = ?2",
            libsql::params![workspace.clone(), user_id.clone()],
        )
        .await
        .unwrap();
    let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(count, 1, "heartbeats must UPSERT, not append");

    // GET /api/presence surfaces the caller as present.
    let (st, body) = send(&app.router, "GET", "/api/presence", None, hdrs).await;
    assert_eq!(st, StatusCode::OK);
    let present = body["present"].as_array().unwrap();
    assert!(present.iter().any(|u| u["user_id"] == json!(user_id)));
}

#[tokio::test]
async fn presence_list_drops_users_past_the_ttl() {
    let app = test_app().await;
    let (workspace, _uid, _tok) = register(&app.router, "pres-ttl").await;
    let conn = raw_conn(&app.db_path).await;

    let now = 10_000i64;
    // fresh: seen 30s ago -> present; stale: seen 5min ago -> expired at ttl=120.
    conn.execute(
        "INSERT INTO presence (workspace_id, user_id, last_seen) VALUES (?1, 'u_fresh', ?2)",
        libsql::params![workspace.clone(), now - 30],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO presence (workspace_id, user_id, last_seen) VALUES (?1, 'u_stale', ?2)",
        libsql::params![workspace.clone(), now - 300],
    )
    .await
    .unwrap();

    let users = active_users(&conn, &workspace, now, 120).await.unwrap();
    let ids: Vec<&str> = users.iter().map(|u| u.user_id.as_str()).collect();
    assert!(ids.contains(&"u_fresh"), "recent heartbeat must be present");
    assert!(!ids.contains(&"u_stale"), "heartbeat older than the TTL must expire");
}

#[tokio::test]
async fn presence_is_workspace_isolated() {
    let app = test_app().await;
    let (ws_a, _ua, _ta) = register(&app.router, "pres-iso-a").await;
    let (ws_b, _ub, _tb) = register(&app.router, "pres-iso-b").await;
    let conn = raw_conn(&app.db_path).await;

    let now = 10_000i64;
    conn.execute(
        "INSERT INTO presence (workspace_id, user_id, last_seen) VALUES (?1, 'only_in_a', ?2)",
        libsql::params![ws_a.clone(), now],
    )
    .await
    .unwrap();

    let in_a = active_users(&conn, &ws_a, now, 120).await.unwrap();
    assert!(in_a.iter().any(|u| u.user_id == "only_in_a"));
    let in_b = active_users(&conn, &ws_b, now, 120).await.unwrap();
    assert!(in_b.is_empty(), "workspace B must not see workspace A's presence");
}

// ------------------------------------------------------- activity since-cursor

#[tokio::test]
async fn events_since_returns_only_events_after_the_cursor_in_order() {
    let app = test_app().await;
    let (workspace, _uid, _tok) = register(&app.router, "feed-cursor").await;
    let conn = raw_conn(&app.db_path).await;

    // Three ordered event ids (evt_ prefix + sortable body).
    seed_event(&conn, "evt_a1", &workspace, "entity.created", Some("ent_1")).await;
    seed_event(&conn, "evt_a2", &workspace, "entity.updated", Some("ent_1")).await;
    seed_event(&conn, "evt_a3", &workspace, "entity.updated", Some("ent_1")).await;

    // Cursor at evt_a1 -> only the two strictly-newer events, ascending.
    let after = events_since(&conn, &workspace, "evt_a1", None, None, 100).await.unwrap();
    let ids: Vec<&str> = after.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["evt_a2", "evt_a3"]);

    // Empty cursor -> the whole (small) log; caller with the tip sees nothing new.
    let all = events_since(&conn, &workspace, "", None, None, 100).await.unwrap();
    assert_eq!(all.len(), 3);
    let none = events_since(&conn, &workspace, "evt_a3", None, None, 100).await.unwrap();
    assert!(none.is_empty(), "cursor at the tip yields no new events");
}

#[tokio::test]
async fn events_since_filters_by_kind_and_module() {
    let app = test_app().await;
    let (workspace, _uid, _tok) = register(&app.router, "feed-filter").await;
    let conn = raw_conn(&app.db_path).await;

    seed_entity(&conn, "ent_task", &workspace, "tasks", "task").await;
    seed_entity(&conn, "ent_trade", &workspace, "trading", "trade").await;
    seed_event(&conn, "evt_f1", &workspace, "entity.created", Some("ent_task")).await;
    seed_event(&conn, "evt_f2", &workspace, "entity.updated", Some("ent_task")).await;
    seed_event(&conn, "evt_f3", &workspace, "entity.created", Some("ent_trade")).await;

    // kind (event type) filter.
    let created = events_since(&conn, &workspace, "", None, Some("entity.created"), 100)
        .await
        .unwrap();
    let ids: Vec<&str> = created.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["evt_f1", "evt_f3"]);

    // module filter (joins the subject entity's module).
    let tasks = events_since(&conn, &workspace, "", Some("tasks"), None, 100).await.unwrap();
    let ids: Vec<&str> = tasks.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["evt_f1", "evt_f2"]);

    // module + kind together.
    let trade_created = events_since(&conn, &workspace, "", Some("trading"), Some("entity.created"), 100)
        .await
        .unwrap();
    let ids: Vec<&str> = trade_created.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["evt_f3"]);
}

#[tokio::test]
async fn events_since_is_workspace_scoped() {
    let app = test_app().await;
    let (ws_a, _ua, _ta) = register(&app.router, "feed-iso-a").await;
    let (ws_b, _ub, _tb) = register(&app.router, "feed-iso-b").await;
    let conn = raw_conn(&app.db_path).await;

    seed_event(&conn, "evt_wa", &ws_a, "entity.created", None).await;
    seed_event(&conn, "evt_wb", &ws_b, "entity.created", None).await;

    let a = events_since(&conn, &ws_a, "", None, None, 100).await.unwrap();
    assert_eq!(a.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["evt_wa"]);
    let b = events_since(&conn, &ws_b, "", None, None, 100).await.unwrap();
    assert_eq!(b.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["evt_wb"]);
}

/// The SSE endpoint must at least resolve identity/scope and open a stream
/// (200 + text/event-stream). We don't drain it - the tail never terminates by
/// design; the since-cursor query above is the behavioral coverage.
#[tokio::test]
async fn events_stream_opens_as_an_sse_response() {
    let app = test_app().await;
    let (_ws, _uid, tok) = register(&app.router, "feed-sse").await;
    let auth = bearer(&tok);

    let req = Request::builder()
        .method("GET")
        .uri("/api/events/stream")
        .header("authorization", auth)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/event-stream"), "expected SSE content-type, got {ct}");
}
