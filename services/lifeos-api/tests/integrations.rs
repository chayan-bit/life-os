//! `/api/{gmail,calendar,drive,notion,slack}/*` HTTP-level tests (issue #53)
//! against a mock Nango client - no real Nango deployment or provider
//! account needed. Covers: reads proxy straight through with the token never
//! leaving Nango; writes only ever draft an entity and never call the mock
//! proxy at all (the load-bearing "gated by construction" assertion).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::nango::mock::MockNangoClient;
use lifeos_api::{build_state_with_nango, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db_path: String,
    nango: Arc<MockNangoClient>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
    }
}

async fn test_app(with_nango: bool) -> TestApp {
    let db_path = std::env::temp_dir()
        .join(format!("lifeos_integrations_{}.db", new_id("t")))
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
    let nango = Arc::new(MockNangoClient::new());
    let state = build_state_with_nango(config, if with_nango { Some(nango.clone()) } else { None })
        .await
        .expect("build state");
    TestApp { router: routes::router(state), db_path, nango }
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
        Some(b) => builder.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
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

/// Runs the connect/complete flow so a `connections` row exists for
/// `provider`, the same way a real OAuth completion would.
async fn connect(app: &Router, nango: &MockNangoClient, ws: &str, provider: &str) {
    let connection_id = format!("conn_{provider}_{ws}");
    nango.seed(&connection_id, provider);
    let (st, body) = send(
        app,
        "POST",
        "/api/connections/complete",
        Some(json!({"connection_id": connection_id, "provider": provider, "workspace_id": ws})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "connect {provider}: {body:?}");
}

struct ProviderCase {
    provider: &'static str,
    list_path: &'static str,
    proxy_method: &'static str,
    proxy_endpoint: &'static str,
    write_path: &'static str,
    write_body: Value,
    write_event_type: &'static str,
}

fn cases() -> Vec<ProviderCase> {
    vec![
        ProviderCase {
            provider: "google-mail",
            list_path: "/api/gmail/list",
            proxy_method: "GET",
            proxy_endpoint: "gmail/v1/users/me/messages",
            write_path: "/api/gmail/send",
            write_body: json!({"to": "a@example.com", "subject": "hi"}),
            write_event_type: "gmail.send.drafted",
        },
        ProviderCase {
            provider: "google-calendar",
            list_path: "/api/calendar/list",
            proxy_method: "GET",
            proxy_endpoint: "calendar/v3/calendars/primary/events",
            write_path: "/api/calendar/create",
            write_body: json!({"summary": "sync", "start": "2026-07-01T09:00:00Z", "end": "2026-07-01T09:30:00Z"}),
            write_event_type: "calendar.create.drafted",
        },
        ProviderCase {
            provider: "google-drive",
            list_path: "/api/drive/list",
            proxy_method: "GET",
            proxy_endpoint: "drive/v3/files",
            write_path: "/api/drive/upload",
            write_body: json!({"name": "notes.pdf", "source_ref": "blob:abc123"}),
            write_event_type: "drive.upload.drafted",
        },
        ProviderCase {
            provider: "notion",
            list_path: "/api/notion/list",
            proxy_method: "POST",
            proxy_endpoint: "v1/search",
            write_path: "/api/notion/create",
            write_body: json!({"parent_id": "page_1", "title": "New page"}),
            write_event_type: "notion.create.drafted",
        },
        ProviderCase {
            provider: "slack",
            list_path: "/api/slack/list",
            proxy_method: "GET",
            proxy_endpoint: "conversations.list",
            write_path: "/api/slack/post",
            write_body: json!({"channel": "#general", "text": "hi"}),
            write_event_type: "slack.post.drafted",
        },
    ]
}

#[tokio::test]
async fn without_nango_configured_all_provider_routes_return_not_implemented() {
    let ta = test_app(false).await;
    for c in cases() {
        let (st, _) = send(&ta.router, "GET", c.list_path, None).await;
        assert_eq!(st, StatusCode::NOT_IMPLEMENTED, "{}", c.list_path);
    }
}

#[tokio::test]
async fn list_proxies_through_nango_and_returns_provider_response_verbatim() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "int-list").await;

    for c in cases() {
        connect(&ta.router, &ta.nango, &ws, c.provider).await;
        ta.nango.seed_proxy(c.provider, c.proxy_method, c.proxy_endpoint, json!({"ok": true, "provider": c.provider}));

        let (st, body) = send(&ta.router, "GET", &format!("{}?workspace_id={ws}", c.list_path), None).await;
        assert_eq!(st, StatusCode::OK, "{}: {body:?}", c.list_path);
        assert_eq!(body["ok"], true);
        assert_eq!(body["provider"], c.provider);
    }
}

#[tokio::test]
async fn list_without_an_active_connection_404s() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "int-no-conn").await;

    for c in cases() {
        let (st, _) = send(&ta.router, "GET", &format!("{}?workspace_id={ws}", c.list_path), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{}", c.list_path);
    }
}

#[tokio::test]
async fn write_paths_only_draft_and_never_call_the_provider() {
    let ta = test_app(true).await;
    let ws = register(&ta.router, "int-drafts").await;

    for c in cases() {
        connect(&ta.router, &ta.nango, &ws, c.provider).await;

        let mut body = c.write_body.clone();
        body["workspace_id"] = json!(ws);
        let (st, entity) = send(&ta.router, "POST", c.write_path, Some(body)).await;
        assert_eq!(st, StatusCode::OK, "{}: {entity:?}", c.write_path);
        assert_eq!(entity["status"], "pending_approval");
        assert_eq!(entity["module"], "integrations");

        let (st, events) = send(
            &ta.router,
            "GET",
            &format!(
                "/api/event?workspace_id={ws}&type={}&entity_id={}",
                c.write_event_type,
                entity["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(events.as_array().unwrap().len(), 1, "{}", c.write_path);
    }

    // The mock proxy was never called by any write path above - only by the
    // separate list test. Gated by construction: `draft_action` has no
    // reference to `state.nango` at all.
    assert!(
        ta.nango.calls.lock().unwrap().is_empty(),
        "write paths must never reach the provider proxy: {:?}",
        ta.nango.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn workspace_b_cannot_use_workspace_a_connection() {
    let ta = test_app(true).await;
    let ws_a = register(&ta.router, "int-tenant-a").await;
    let ws_b = register(&ta.router, "int-tenant-b").await;

    connect(&ta.router, &ta.nango, &ws_a, "slack").await;
    ta.nango.seed_proxy("slack", "GET", "conversations.list", json!({"channels": []}));

    let (st, _) = send(&ta.router, "GET", &format!("/api/slack/list?workspace_id={ws_b}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "workspace B has no slack connection of its own");
}
