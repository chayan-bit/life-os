//! HTTP-level verification harness for the cognitive memory subsystem
//! (issue #120, docs/AI-MEMORY.md §11 "must-pass checks"):
//!
//! 1. rebuild-equivalence: wipe read models, replay `events`, identical recall
//! 2. supersede correctness: old fact never current; point-in-time recovers it
//! 3. provenance: every recalled memory resolves to source_event_ids
//! 4. self-RAG gate + abstention (no confabulation), both on the ledger
//! 5. workspace isolation
//! 6. no-secret-in-memory
//! 7. memory collects from EVERYWHERE: bot/platform/terminal writes all fold
//!    into one recallable store (they share the `events` spine)
//!
//! The LongMemEval/LoCoMo-style ranked-quality fixtures (vs a flat top-k
//! baseline) live crate-side in lifeos-memory/tests/bench_fixtures.rs.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use lifeos_api::{build_state, config::Config, ids::new_id, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

const WS: &str = "default-personal-workspace";

struct TestApp {
    router: Router,
    db_path: String,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}.derived", self.db_path));
        let _ = std::fs::remove_dir_all(format!("{}.blobs", self.db_path));
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
        .join(format!("lifeos_memory_{}.db", new_id("t")))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_file(&db_path);
    let state = build_state(base_config(&db_path)).await.expect("build state");
    TestApp { router: routes::router(state), db_path }
}

async fn send(app: &Router, method: &str, uri: &str, ws: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(ws) = ws {
        builder = builder.header("x-workspace-id", ws);
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn ingest(app: &Router, source: &str, event_type: Option<&str>, content: &str) {
    let mut body = json!({ "content": content, "source": source });
    if let Some(t) = event_type {
        body["type"] = json!(t);
    }
    let (st, resp) = send(app, "POST", "/api/memory/ingest", None, Some(body)).await;
    assert_eq!(st, StatusCode::OK, "ingest: {resp:?}");
}

/// Consolidation only consumes SETTLED events (>15 min old). Tests backdate
/// timestamps directly in the DB file - pure test scaffolding, not an API
/// mutation path (the API itself still has no event UPDATE route).
async fn backdate_events(db_path: &str, secs: i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute("UPDATE events SET ts = ts - ?1", libsql::params![secs]).await.unwrap();
}

fn recalled(body: &Value) -> Vec<(String, String, Vec<String>)> {
    body["memories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_string(),
                m["content"].as_str().unwrap().to_string(),
                m["source_event_ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// Must-pass #7 + #1 + #3: events from every tier (telegram bot relay,
/// platform API write, terminal hook) land in one memory; recall carries
/// provenance for each; a full rebuild reproduces identical results.
#[tokio::test]
async fn collects_from_everywhere_with_provenance_and_rebuilds_identically() {
    let app = test_app().await;

    // Telegram-tier write: the bot appends events via POST /api/event.
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "task.completed", "actor": "bot",
                    "attrs": {"note": "telegram capture: paid the electricity bill"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Platform-tier write: same spine, different actor.
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "study.review", "actor": "user",
                    "attrs": {"topic": "platform capture: spectral graph theory"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Terminal-tier write: the universal capture endpoint.
    ingest(&app.router, "terminal", None, "terminal capture: benchmarked the drain loop").await;

    let query = json!({"query": "what did I capture recently across sources?", "no_gate": true, "top_k": 20});
    let (st, body) =
        send(&app.router, "POST", "/api/memory/recall", None, Some(query.clone())).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["outcome"], "recalled");
    let first = recalled(&body);
    let contents: Vec<&String> = first.iter().map(|(_, c, _)| c).collect();
    assert!(contents.iter().any(|c| c.contains("telegram capture")), "{contents:?}");
    assert!(contents.iter().any(|c| c.contains("platform capture")), "{contents:?}");
    assert!(contents.iter().any(|c| c.contains("terminal capture")), "{contents:?}");
    // Provenance on every recalled memory (must-pass #3).
    for (_, content, sources) in &first {
        assert!(!sources.is_empty(), "no provenance on: {content}");
    }

    // Must-pass #1: rebuild-equivalence.
    let (st, stats) = send(&app.router, "POST", "/api/memory/rebuild", None, Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{stats:?}");
    let (st, body2) = send(&app.router, "POST", "/api/memory/recall", None, Some(query)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first, recalled(&body2), "wipe + replay must return identical recall");
}

/// Must-pass #2: "moved cities" - the superseded fact is never current truth,
/// but a point-in-time query still recovers it.
#[tokio::test]
async fn supersede_is_bi_temporal_over_http() {
    let app = test_app().await;

    // Create the profile entity, then two updates about it.
    let (st, entity) = send(
        &app.router, "POST", "/api/entity", None,
        Some(json!({"module": "profile", "type": "fact", "title": "Home city"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{entity:?}");
    let entity_id = entity["id"].as_str().unwrap().to_string();

    // Two-step backdating puts the facts ~200ks apart with a clean window
    // between them for the point-in-time probe: Delhi at ~now-300k,
    // Bangalore at ~now-100k.
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "profile.updated", "actor": "user", "entity_id": entity_id,
                    "attrs": {"home city": "Delhi"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    backdate_events(&app.db_path, 200_000).await;
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "profile.updated", "actor": "user", "entity_id": entity_id,
                    "attrs": {"home city": "Bangalore"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    backdate_events(&app.db_path, 100_000).await;

    let (st, report) = send(&app.router, "POST", "/api/memory/sleep", None, Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{report:?}");
    assert!(report["supersedes"].as_i64().unwrap() >= 1, "{report:?}");

    // Current truth: only Bangalore.
    let (_, now_body) = send(
        &app.router, "POST", "/api/memory/recall", None,
        Some(json!({"query": "what is my home city?", "no_gate": true})),
    )
    .await;
    assert_eq!(now_body["outcome"], "recalled", "{now_body:?}");
    // Scope to FACT nodes: episode summaries are historical digests and may
    // legitimately mention Delhi; temporal truth lives on the fact layer.
    let city_mems: Vec<(String, String, Vec<String>)> = recalled(&now_body)
        .into_iter()
        .filter(|(_, c, _)| c.contains("home city") && !c.starts_with("[summary"))
        .collect();
    assert!(!city_mems.is_empty());
    assert!(city_mems.iter().all(|(_, c, _)| !c.contains("Delhi")), "{city_mems:?}");
    assert!(city_mems.iter().any(|(_, c, _)| c.contains("Bangalore")));

    // Point-in-time (between the two facts): Delhi comes back.
    let as_of = now_secs() - 200_000;
    let (_, then_body) = send(
        &app.router, "POST", "/api/memory/recall", None,
        Some(json!({"query": "what is my home city?", "no_gate": true, "as_of": as_of})),
    )
    .await;
    assert_eq!(then_body["outcome"], "recalled", "{then_body:?}");
    let then_contents: Vec<String> =
        recalled(&then_body).into_iter().map(|(_, c, _)| c).collect();
    assert!(then_contents.iter().any(|c| c.contains("Delhi")), "{then_contents:?}");
    assert!(then_contents.iter().all(|c| !c.contains("Bangalore")), "{then_contents:?}");
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Must-pass #4: smalltalk skips retrieval; unknown facts abstain; both are
/// visible on the inspector's ledger feed.
#[tokio::test]
async fn gate_abstention_and_ledger_visibility() {
    let app = test_app().await;
    ingest(&app.router, "terminal", None, "the deploy pipeline uses blue-green rollouts").await;

    let (_, skipped) = send(
        &app.router, "POST", "/api/memory/recall", None, Some(json!({"query": "thanks!"})),
    )
    .await;
    assert_eq!(skipped["outcome"], "skipped", "{skipped:?}");

    let (_, abstained) = send(
        &app.router, "POST", "/api/memory/recall", None,
        Some(json!({"query": "what is grandma's secret biryani recipe?"})),
    )
    .await;
    assert_eq!(abstained["outcome"], "abstained", "{abstained:?}");

    let (st, inspect) = send(&app.router, "GET", "/api/memory/inspect", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let types: Vec<String> = inspect["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect();
    assert!(types.contains(&"memory.recall.skipped".to_string()), "{types:?}");
    assert!(types.contains(&"memory.recall.abstained".to_string()), "{types:?}");
}

/// Must-pass #5: a second workspace recalls nothing of the first's.
#[tokio::test]
async fn workspace_isolation_over_http() {
    let app = test_app().await;
    ingest(&app.router, "terminal", None, "workspace one's private trading notes").await;

    // Second workspace, via register.
    let (st, reg) = send(
        &app.router, "POST", "/api/register", None,
        Some(json!({"email": "two@test.example", "name": "two",
                    "password": "test-password-123", "workspace_name": "two"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{reg:?}");
    let ws2 = reg["workspace_id"].as_str().unwrap();

    let (_, body) = send(
        &app.router, "POST", "/api/memory/recall", Some(ws2),
        Some(json!({"query": "private trading notes", "no_gate": true})),
    )
    .await;
    assert_ne!(body["outcome"], "recalled", "ws2 must not see ws1 memory: {body:?}");

    // And the default workspace still finds it.
    let (_, body) = send(
        &app.router, "POST", "/api/memory/recall", Some(WS),
        Some(json!({"query": "private trading notes", "no_gate": true})),
    )
    .await;
    assert_eq!(body["outcome"], "recalled");
}

/// Must-pass #6: secrets never surface through memory - credential events are
/// skipped wholesale, secret-named attrs are redacted.
#[tokio::test]
async fn no_secret_ever_enters_memory() {
    let app = test_app().await;
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "connection.created", "actor": "user",
                    "attrs": {"provider": "google", "access_token": "ya29.ULTRA-SECRET"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "task.created", "actor": "user",
                    "attrs": {"note": "rotate the broker keys", "api_key": "kite-HIDDEN-KEY"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (_, body) = send(
        &app.router, "POST", "/api/memory/recall", None,
        Some(json!({"query": "rotate the broker keys", "no_gate": true})),
    )
    .await;
    let dump = body.to_string();
    assert!(dump.contains("rotate the broker keys"), "{dump}");
    assert!(!dump.contains("ULTRA-SECRET"), "secret leaked into recall");
    assert!(!dump.contains("HIDDEN-KEY"), "secret attr leaked into recall");

    // Nor via the inspector/ledger surface.
    let (_, inspect) = send(&app.router, "GET", "/api/memory/inspect", None, None).await;
    let dump = inspect.to_string();
    assert!(!dump.contains("ULTRA-SECRET") && !dump.contains("HIDDEN-KEY"), "{dump}");
}

/// Issues #116 + #118 end-to-end: feedback becomes a procedural rule via a
/// sleep cycle, and the compiled context (deterministic, budgeted) carries it
/// - a learned rule measurably changes what later turns see.
#[tokio::test]
async fn learned_rule_feeds_the_compiled_context() {
    let app = test_app().await;
    let (st, _) = send(
        &app.router, "POST", "/api/event", None,
        Some(json!({"type": "feedback.given", "actor": "user",
                    "attrs": {"feedback": "always lead with the TLDR"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    ingest(&app.router, "terminal", None, "shipped the memory subsystem today").await;
    backdate_events(&app.db_path, 100_000).await;

    let (st, report) = send(&app.router, "POST", "/api/memory/sleep", None, Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(report["rules_added"].as_i64(), Some(1), "{report:?}");

    let (_, rules) = send(&app.router, "GET", "/api/memory/rules", None, None).await;
    assert_eq!(rules["rules"][0].as_str(), Some("always lead with the TLDR"));

    let (st, ctx) = send(
        &app.router, "POST", "/api/memory/context", None,
        Some(json!({
            "query": "what did I ship today?",
            "recent_turns": [{"role": "user", "content": "what did I ship today?"}],
            "budget_tokens": 1200
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ctx:?}");
    let text = ctx["context"]["text"].as_str().unwrap();
    assert!(text.contains("always lead with the TLDR"), "{text}");
    assert!(text.contains("shipped the memory subsystem"), "{text}");
    assert!(ctx["context"]["tokens_used"].as_u64().unwrap() <= 1200);

    // Deterministic: compiling again for the same state+query is identical.
    let (_, ctx2) = send(
        &app.router, "POST", "/api/memory/context", None,
        Some(json!({
            "query": "what did I ship today?",
            "recent_turns": [{"role": "user", "content": "what did I ship today?"}],
            "budget_tokens": 1200
        })),
    )
    .await;
    assert_eq!(ctx["context"]["text"], ctx2["context"]["text"]);
}

/// Audit #1: with LIFEOS_MEMVEC unset (the test default), the newly-wired
/// vector lane must degrade to lexical-only - recall still works, never errors.
/// This is the "recall degrades cleanly when the semantic lane is absent" guard
/// for the MemvecSearcher wiring.
#[tokio::test]
async fn recall_degrades_cleanly_without_the_vector_lane() {
    let app = test_app().await;
    ingest(&app.router, "terminal", None, "the release loop promotes candidate configs after eval").await;

    let (st, body) = send(
        &app.router, "POST", "/api/memory/recall", None,
        Some(json!({"query": "how does the release loop promote configs?", "no_gate": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["outcome"], "recalled", "lexical-only recall must still succeed: {body:?}");
    let contents: Vec<String> = recalled(&body).into_iter().map(|(_, c, _)| c).collect();
    assert!(contents.iter().any(|c| c.contains("release loop")), "{contents:?}");
}

/// Issue #139 (GraphRAG global queries, docs/AGENT-CORE.md §13): the sleep
/// cycle rebuilds a community map over two clusters, `/api/memory/network`
/// lists both with non-empty summaries, and `/api/memory/network/ask` ranks
/// the trading cluster first for a trading-themed question.
#[tokio::test]
async fn network_map_rebuilds_on_sleep_and_ask_ranks_the_relevant_cluster() {
    let app = test_app().await;

    let (_, trading) = send(
        &app.router, "POST", "/api/entity", None,
        Some(json!({"module": "trading", "type": "desk", "title": "Banknifty swing desk"})),
    )
    .await;
    let trading_id = trading["id"].as_str().unwrap().to_string();
    let (_, learning) = send(
        &app.router, "POST", "/api/entity", None,
        Some(json!({"module": "learning", "type": "topic", "title": "Market microstructure"})),
    )
    .await;
    let learning_id = learning["id"].as_str().unwrap().to_string();

    for i in 0..3 {
        let (st, _) = send(
            &app.router, "POST", "/api/event", None,
            Some(json!({"type": "trade.closed", "actor": "user", "entity_id": trading_id,
                        "attrs": {"note": format!("trade {i} closed")}})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    for i in 0..3 {
        let (st, _) = send(
            &app.router, "POST", "/api/event", None,
            Some(json!({"type": "study.review", "actor": "user", "entity_id": learning_id,
                        "attrs": {"note": format!("reviewed topic {i}")}})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    backdate_events(&app.db_path, 1_000).await;

    let (st, report) = send(&app.router, "POST", "/api/memory/sleep", None, Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{report:?}");
    assert_eq!(report["communities"].as_i64().unwrap(), 2, "{report:?}");

    let (st, map) = send(&app.router, "GET", "/api/memory/network", None, None).await;
    assert_eq!(st, StatusCode::OK, "{map:?}");
    let communities = map["communities"].as_array().unwrap();
    assert_eq!(communities.len(), 2);
    assert!(communities.iter().all(|c| !c["summary"].as_str().unwrap().is_empty()));

    let (st, ask) = send(
        &app.router, "POST", "/api/memory/network/ask", None,
        Some(json!({"question": "which parts of my world touch trading?", "top_k": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ask:?}");
    let hits = ask["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0]["summary"].as_str().unwrap().to_lowercase().contains("trade"),
        "{hits:?}"
    );
}
