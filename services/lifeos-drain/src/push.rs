//! Web push fan-out (issue #151): the sender half of `push_subscriptions`
//! (issue #103, `services/lifeos-api/src/routes/push.rs`) - that route only
//! ever stored a subscription; nothing sent an actual push. This module
//! watches `events` for approval/build-gate/brief notifications and pushes to
//! every subscription in the firing workspace, VAPID-signed via the
//! `web-push` crate, pruning subscriptions the push service reports dead.
//!
//! Scan shape mirrors `lifeos_actions::run_action_engine_tick` exactly: one
//! `entities`-row cursor per workspace (`module='push', type='cursor'`),
//! `WHERE id > cursor ORDER BY id ASC` (events.id is a ULID, so this is a
//! correct incremental scan), advance past the whole batch regardless of
//! whether anything matched. This crate has no dependency on `lifeos-actions`
//! (standalone-crate convention, see `lib.rs`'s `emit_event` doc comment), so
//! the cursor helpers are duplicated in miniature rather than shared.
//!
//! Event kinds notified on - verified against the emitting code, not guessed:
//!   - `entity.created` (`services/lifeos-api/src/routes/entity.rs`), filtered
//!     to entities whose current `status` is `pending_approval` or
//!     `awaiting_approval` (the same two statuses
//!     `services/lifeos-api/src/routes/approval.rs`'s `PENDING_STATUSES`
//!     lists). Covers `server/build/gate.js`'s `createPendingApproval`, which
//!     POSTs to `/api/entity`, and any future caller of the generic entity
//!     route that drafts a pending approval.
//!   - `pipeline.stage.gated` (`services/lifeos-pipelines/src/lib.rs`) - a
//!     gated pipeline stage inserts its `pending_approval` entity directly by
//!     SQL (bypassing `/api/entity`), so it never fires `entity.created` and
//!     needs its own rule.
//!   - `build.node.gated` (`server/build/gate.js`'s `emitBuildEvent`) - a T3+
//!     self-extension build node halted awaiting approval.
//!   - `brief.sent` (this crate's own `run_daily_brief`, `lib.rs`) - the daily
//!     brief ledger event.
//!
//! VAPID keys (`LIFEOS_VAPID_PUBLIC_KEY` / `LIFEOS_VAPID_PRIVATE_KEY` /
//! `LIFEOS_VAPID_SUBJECT`) are read once by `main.rs`; without all three the
//! sender is disabled - `NoopPushSender` sends nothing and never errors, the
//! same graceful-degradation posture `NoopNotifier`/`NoopEmbedder`/etc. use
//! for every other optional lane in this crate.

use async_trait::async_trait;
use libsql::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ------------------------------------------------------------------ config

/// VAPID credentials for signing push requests (RFC 8292). All three fields
/// are required together - see `vapid_config_from`'s doc comment for why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VapidConfig {
    pub private_key: String,
    pub subject: String,
}

impl VapidConfig {
    /// Reads `LIFEOS_VAPID_PUBLIC_KEY` / `LIFEOS_VAPID_PRIVATE_KEY` /
    /// `LIFEOS_VAPID_SUBJECT` from the environment. The public key isn't
    /// actually needed to *sign* a push (the private key alone derives it),
    /// but its presence is still required here so a half-configured setup
    /// (e.g. a public key handed to the frontend with no matching private key
    /// on the drain) fails closed rather than silently signing with the
    /// wrong keypair.
    pub fn from_env() -> Option<Self> {
        vapid_config_from(
            non_empty_env("LIFEOS_VAPID_PUBLIC_KEY"),
            non_empty_env("LIFEOS_VAPID_PRIVATE_KEY"),
            non_empty_env("LIFEOS_VAPID_SUBJECT"),
        )
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Pure all-or-nothing gate behind [`VapidConfig::from_env`], split out so the
/// precedence rule is directly unit-testable without touching process env
/// (the same reasoning `node.rs`'s `resolve_node_id` documents).
fn vapid_config_from(public: Option<String>, private: Option<String>, subject: Option<String>) -> Option<VapidConfig> {
    let _public = public?; // presence-checked only, see doc comment above
    Some(VapidConfig { private_key: private?, subject: subject? })
}

// ------------------------------------------------------------------- model

/// One workspace's push subscription row (mirrors `push_subscriptions`,
/// `migrations/0010_push_subscriptions.sql`), with `keys_json` already
/// unpacked into the two keys a push request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscription {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// The notification a browser's service worker renders
/// (`frontend/public/sw.js`'s `push` handler reads exactly these three
/// fields). `url` is the deep-link `notificationclick` opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPayload {
    pub title: String,
    pub body: String,
    pub url: String,
}

/// The two statuses a resolvable entity sits in while awaiting a human
/// decision. Mirrors `services/lifeos-api/src/routes/approval.rs`'s
/// `PENDING_STATUSES` verbatim (that crate isn't a dependency of this one, so
/// the list is duplicated rather than imported - same standalone convention
/// as this module's cursor helpers).
const PENDING_STATUSES: [&str; 2] = ["pending_approval", "awaiting_approval"];

/// What kind of push a matched event produces - each maps to one deep-link
/// destination in the PWA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationKind {
    /// A pending_approval/awaiting_approval entity was created -> the
    /// approvals inbox.
    Approval,
    /// A build node or pipeline stage halted awaiting approval -> the observe
    /// page, where the run itself can be inspected.
    BuildGate,
    /// The daily brief is ready -> the dashboard, where briefs render.
    Brief,
}

impl NotificationKind {
    fn payload(self, attrs: &Value) -> NotificationPayload {
        match self {
            NotificationKind::Approval => NotificationPayload {
                title: "Approval needed".to_string(),
                body: "Something is waiting for your review.".to_string(),
                url: "/approvals".to_string(),
            },
            NotificationKind::BuildGate => {
                // `build.node.gated` stamps `attrs.node`; `pipeline.stage.gated`
                // stamps `attrs.stage` - check both so either source gets a
                // named body rather than a generic one.
                let subject = attrs
                    .get("node")
                    .and_then(Value::as_str)
                    .or_else(|| attrs.get("stage").and_then(Value::as_str));
                let body = match subject {
                    Some(name) => format!("'{name}' is awaiting your approval."),
                    None => "A build gate is awaiting your approval.".to_string(),
                };
                NotificationPayload { title: "Build gate reached".to_string(), body, url: "/observe".to_string() }
            }
            NotificationKind::Brief => NotificationPayload {
                title: "Daily brief ready".to_string(),
                body: "Your daily brief is ready to read.".to_string(),
                url: "/dashboard".to_string(),
            },
        }
    }
}

/// Classifies one `events` row into a notification, if it should produce one.
/// `entity_status` is only consulted for `entity.created` (the caller looks it
/// up on demand, since every other kind below carries everything it needs in
/// its own `attrs`).
fn notification_for_event(event_type: &str, entity_status: Option<&str>) -> Option<NotificationKind> {
    match event_type {
        "entity.created" => {
            let status = entity_status?;
            PENDING_STATUSES.contains(&status).then_some(NotificationKind::Approval)
        }
        "pipeline.stage.gated" | "build.node.gated" => Some(NotificationKind::BuildGate),
        "brief.sent" => Some(NotificationKind::Brief),
        _ => None,
    }
}

// --------------------------------------------------------------- sending

/// The outcome of one push attempt, from the fan-out orchestration's point of
/// view - only "gone" (prune the row) is actionable; `Failed`/`Disabled` both
/// leave the subscription alone (a transient failure may succeed next time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Sent,
    /// The push service returned 404/410 - this subscription is dead.
    Gone,
    /// Any other failure (network, malformed keys, 5xx, ...); logged by the
    /// sender, not propagated.
    Failed,
    /// No VAPID keys configured - `NoopPushSender`'s only outcome.
    Disabled,
}

/// Delivers one push. Injected so the fan-out orchestration is unit-testable
/// without a network call or real VAPID keys - the same DI seam
/// `Notifier`/`ModuleBuilder` use elsewhere in this crate.
#[async_trait]
pub trait PushSender: Send + Sync {
    async fn send(&self, sub: &PushSubscription, payload: &NotificationPayload) -> PushOutcome;
}

/// Used when `LIFEOS_VAPID_*` isn't fully configured - the fan-out tick still
/// scans and advances cursors correctly, it just never actually pushes.
pub struct NoopPushSender;

#[async_trait]
impl PushSender for NoopPushSender {
    async fn send(&self, _sub: &PushSubscription, _payload: &NotificationPayload) -> PushOutcome {
        PushOutcome::Disabled
    }
}

/// Real sender: VAPID-signs and delivers via the `web-push` crate's
/// encryption/signing (RFC 8291/8292), using `request_builder::build_request`
/// to get a generic `http::Request` and this crate's own `reqwest::Client` to
/// actually send it - `web-push` is built with `default-features = false`
/// (no `isahc-client`/`hyper-client`) specifically so it pulls in neither
/// HTTP stack, reusing the `reqwest` this crate already depends on
/// (`TelegramNotifier` does the same). `web-push`'s own `http` dependency is a
/// different major version than `reqwest`'s, so headers are copied over by
/// name/value rather than passed as a `HeaderMap` directly.
pub struct WebPushSender {
    vapid: VapidConfig,
    http: reqwest::Client,
}

impl WebPushSender {
    pub fn new(vapid: VapidConfig) -> Self {
        Self { vapid, http: reqwest::Client::new() }
    }

    async fn send_inner(&self, sub: &PushSubscription, payload: &NotificationPayload) -> Result<PushOutcome, String> {
        use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};

        let subscription_info = SubscriptionInfo::new(sub.endpoint.as_str(), sub.p256dh.as_str(), sub.auth.as_str());
        let mut sig_builder = VapidSignatureBuilder::from_base64(&self.vapid.private_key, &subscription_info)
            .map_err(|e| format!("invalid VAPID private key: {e}"))?;
        sig_builder.add_claim("sub", self.vapid.subject.as_str());
        let vapid_signature = sig_builder.build().map_err(|e| format!("failed to sign VAPID claim: {e}"))?;

        let body = serde_json::to_vec(payload).map_err(|e| format!("failed to encode push payload: {e}"))?;
        let mut builder = WebPushMessageBuilder::new(&subscription_info);
        builder.set_payload(ContentEncoding::Aes128Gcm, &body);
        builder.set_vapid_signature(vapid_signature);
        let message = builder.build().map_err(|e| format!("failed to build push message: {e}"))?;

        let request = web_push::request_builder::build_request::<Vec<u8>>(message);
        let (parts, body) = request.into_parts();
        let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
        let mut req = self.http.request(method, parts.uri.to_string());
        for (name, value) in parts.headers.iter() {
            req = req.header(name.as_str(), value.as_bytes());
        }
        let resp = req.body(body).send().await.map_err(|e| format!("push request failed: {e}"))?;

        let status = resp.status();
        if status.is_success() {
            Ok(PushOutcome::Sent)
        } else if status.as_u16() == 404 || status.as_u16() == 410 {
            Ok(PushOutcome::Gone)
        } else {
            let text = resp.text().await.unwrap_or_default();
            Err(format!("push service returned {status}: {text}"))
        }
    }
}

#[async_trait]
impl PushSender for WebPushSender {
    async fn send(&self, sub: &PushSubscription, payload: &NotificationPayload) -> PushOutcome {
        match self.send_inner(sub, payload).await {
            Ok(outcome) => outcome,
            Err(e) => {
                eprintln!("lifeos-drain: push to {} failed: {e}", sub.endpoint);
                PushOutcome::Failed
            }
        }
    }
}

// ------------------------------------------------------------------ cursor
//
// Identical shape to `lifeos_actions`'s `get_cursor`/`set_cursor`, just a
// different cursor-entity prefix (`push_cursor_` vs `actions_cursor_`) so the
// two engines' independent scans of the same `events` table never collide.

fn cursor_entity_id(workspace_id: &str) -> String {
    format!("push_cursor_{workspace_id}")
}

async fn get_cursor(conn: &Connection, workspace_id: &str) -> Result<String, String> {
    let mut rows = conn
        .query("SELECT attrs FROM entities WHERE id = ?1", params![cursor_entity_id(workspace_id)])
        .await
        .map_err(|e| format!("failed to read push cursor: {e}"))?;
    match rows.next().await.map_err(|e| format!("failed to read push cursor: {e}"))? {
        Some(row) => {
            let attrs_str: String = row.get(0).map_err(|e| e.to_string())?;
            let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or(Value::Null);
            Ok(attrs.get("last_event_id").and_then(Value::as_str).unwrap_or("").to_string())
        }
        None => Ok(String::new()),
    }
}

async fn set_cursor(conn: &Connection, workspace_id: &str, event_id: &str, now: i64) -> Result<(), String> {
    let attrs = serde_json::json!({ "last_event_id": event_id });
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, attrs, source, created_at, updated_at) \
         VALUES (?1, ?2, 'push', 'cursor', ?3, 'lifeos-drain', ?4, ?4) \
         ON CONFLICT(id) DO UPDATE SET attrs = excluded.attrs, updated_at = excluded.updated_at",
        params![
            cursor_entity_id(workspace_id),
            workspace_id,
            serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into()),
            now
        ],
    )
    .await
    .map_err(|e| format!("failed to write push cursor: {e}"))?;
    Ok(())
}

// --------------------------------------------------------------- fan-out

const SCAN_BATCH_LIMIT: i64 = 200;

/// The `status` of an entity, if it exists. Only ever queried for
/// `entity.created` events, since every other notified kind carries what it
/// needs in its own `attrs`.
async fn entity_status(conn: &Connection, entity_id: &str) -> Result<Option<String>, String> {
    let mut rows = conn
        .query("SELECT status FROM entities WHERE id = ?1", params![entity_id])
        .await
        .map_err(|e| format!("failed to read entity status: {e}"))?;
    match rows.next().await.map_err(|e| format!("failed to read entity status: {e}"))? {
        Some(row) => row.get::<Option<String>>(0).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

async fn list_subscriptions(conn: &Connection, workspace_id: &str) -> Result<Vec<PushSubscription>, String> {
    let mut rows = conn
        .query(
            "SELECT id, endpoint, keys_json FROM push_subscriptions WHERE workspace_id = ?1",
            params![workspace_id],
        )
        .await
        .map_err(|e| format!("failed to list push subscriptions: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| format!("failed to list push subscriptions: {e}"))? {
        let id: String = row.get(0).map_err(|e| e.to_string())?;
        let endpoint: String = row.get(1).map_err(|e| e.to_string())?;
        let keys_str: String = row.get(2).map_err(|e| e.to_string())?;
        let keys: Value = serde_json::from_str(&keys_str).unwrap_or(Value::Null);
        let p256dh = keys.get("p256dh").and_then(Value::as_str).unwrap_or("").to_string();
        let auth = keys.get("auth").and_then(Value::as_str).unwrap_or("").to_string();
        if p256dh.is_empty() || auth.is_empty() {
            // A malformed subscription row must never crash the whole tick -
            // skip it, leave it in place (a resubscribe will overwrite it via
            // routes/push.rs's upsert).
            continue;
        }
        out.push(PushSubscription { id, endpoint, p256dh, auth });
    }
    Ok(out)
}

async fn delete_subscription(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM push_subscriptions WHERE id = ?1", params![id])
        .await
        .map_err(|e| format!("failed to delete push subscription {id}: {e}"))?;
    Ok(())
}

/// Sends `payload` to every subscription in `workspace_id`, pruning any the
/// push service reports gone. Returns the number actually delivered.
async fn notify_workspace_subscriptions(
    conn: &Connection,
    workspace_id: &str,
    sender: &dyn PushSender,
    payload: &NotificationPayload,
) -> Result<usize, String> {
    let subs = list_subscriptions(conn, workspace_id).await?;
    let mut sent = 0usize;
    for sub in subs {
        match sender.send(&sub, payload).await {
            PushOutcome::Sent => sent += 1,
            PushOutcome::Gone => {
                if let Err(e) = delete_subscription(conn, &sub.id).await {
                    eprintln!("lifeos-drain: failed to prune dead push subscription {}: {e}", sub.id);
                }
            }
            PushOutcome::Failed | PushOutcome::Disabled => {}
        }
    }
    Ok(sent)
}

/// Scans new `events` for one workspace since its cursor, sends a push for
/// each matching row (to every subscription in the workspace), and advances
/// the cursor past the whole batch regardless of whether anything matched -
/// identical discipline to `lifeos_actions::process_workspace_events`.
/// Returns the number of pushes actually delivered.
pub async fn process_workspace_notifications(
    conn: &Connection,
    workspace_id: &str,
    sender: &dyn PushSender,
    now: i64,
) -> Result<usize, String> {
    let cursor = get_cursor(conn, workspace_id).await?;
    let mut rows = conn
        .query(
            "SELECT id, type, entity_id, attrs FROM events \
             WHERE workspace_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
            params![workspace_id, cursor, SCAN_BATCH_LIMIT],
        )
        .await
        .map_err(|e| format!("failed to scan events: {e}"))?;

    let mut sent = 0usize;
    let mut last_id: Option<String> = None;

    while let Some(row) = rows.next().await.map_err(|e| format!("failed to scan events: {e}"))? {
        let event_id: String = row.get(0).map_err(|e| e.to_string())?;
        let event_type: String = row.get(1).map_err(|e| e.to_string())?;
        let entity_id: Option<String> = row.get(2).map_err(|e| e.to_string())?;
        let attrs_str: String = row.get(3).map_err(|e| e.to_string())?;
        let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or(Value::Null);

        let status = if event_type == "entity.created" {
            match &entity_id {
                Some(id) => entity_status(conn, id).await?,
                None => None,
            }
        } else {
            None
        };

        if let Some(kind) = notification_for_event(&event_type, status.as_deref()) {
            let payload = kind.payload(&attrs);
            sent += notify_workspace_subscriptions(conn, workspace_id, sender, &payload).await?;
        }

        last_id = Some(event_id);
    }

    if let Some(id) = last_id {
        set_cursor(conn, workspace_id, &id, now).await?;
    }

    Ok(sent)
}

/// The function `lifeos-drain`'s poll loop calls once per tick: scans every
/// workspace independently (each keeps its own cursor) and sums the pushes
/// delivered across all of them. Mirrors
/// `lifeos_actions::run_action_engine_tick`'s per-tick shape exactly.
pub async fn run_push_notification_tick(conn: &Connection, sender: &dyn PushSender, now: i64) -> Result<usize, String> {
    let mut rows = conn
        .query("SELECT id FROM workspaces", ())
        .await
        .map_err(|e| format!("failed to list workspaces: {e}"))?;
    let mut workspace_ids = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| format!("failed to list workspaces: {e}"))? {
        workspace_ids.push(row.get::<String>(0).map_err(|e| e.to_string())?);
    }

    let mut total = 0usize;
    for workspace_id in workspace_ids {
        total += process_workspace_notifications(conn, &workspace_id, sender, now).await?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Builder;
    use std::sync::Mutex;

    async fn fresh_conn(path: &str) -> Connection {
        let _ = std::fs::remove_file(path);
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT)", ())
            .await
            .unwrap();
        conn.execute(
            "CREATE TABLE entities (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT, type TEXT, \
                status TEXT, attrs TEXT NOT NULL DEFAULT '{}', source TEXT, \
                created_at INTEGER, updated_at INTEGER)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER, type TEXT, \
                entity_id TEXT, actor TEXT, attrs TEXT)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE push_subscriptions (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, endpoint TEXT NOT NULL, \
                keys_json TEXT NOT NULL, created_at INTEGER NOT NULL)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    async fn insert_workspace(conn: &Connection, id: &str) {
        conn.execute("INSERT INTO workspaces (id, name) VALUES (?1, 'w')", params![id]).await.unwrap();
    }

    async fn insert_entity(conn: &Connection, id: &str, workspace_id: &str, r#type: &str, status: &str) {
        conn.execute(
            "INSERT INTO entities (id, workspace_id, module, type, status, attrs, source, created_at, updated_at) \
             VALUES (?1, ?2, 'pipelines', ?3, ?4, '{}', 'api', 0, 0)",
            params![id, workspace_id, r#type, status],
        )
        .await
        .unwrap();
    }

    async fn insert_event(
        conn: &Connection,
        id: &str,
        workspace_id: &str,
        event_type: &str,
        entity_id: Option<&str>,
        attrs: &Value,
    ) {
        conn.execute(
            "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
             VALUES (?1, ?2, 0, ?3, ?4, 'api', ?5)",
            params![id, workspace_id, event_type, entity_id, serde_json::to_string(attrs).unwrap()],
        )
        .await
        .unwrap();
    }

    async fn insert_subscription(conn: &Connection, id: &str, workspace_id: &str, endpoint: &str) {
        let keys = serde_json::json!({ "p256dh": "p256dh-key", "auth": "auth-secret" });
        conn.execute(
            "INSERT INTO push_subscriptions (id, workspace_id, endpoint, keys_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![id, workspace_id, endpoint, serde_json::to_string(&keys).unwrap()],
        )
        .await
        .unwrap();
    }

    async fn subscription_count(conn: &Connection, workspace_id: &str) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM push_subscriptions WHERE workspace_id=?1", params![workspace_id])
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    /// Records every send call and returns a canned outcome per endpoint
    /// (default `Sent`) - the DI double every fan-out test drives, so no test
    /// ever touches the network or real VAPID keys.
    #[derive(Default)]
    struct MockPushSender {
        outcomes: std::collections::HashMap<String, PushOutcome>,
        calls: Mutex<Vec<(String, NotificationPayload)>>,
    }

    #[async_trait]
    impl PushSender for MockPushSender {
        async fn send(&self, sub: &PushSubscription, payload: &NotificationPayload) -> PushOutcome {
            self.calls.lock().unwrap().push((sub.endpoint.clone(), payload.clone()));
            self.outcomes.get(&sub.endpoint).copied().unwrap_or(PushOutcome::Sent)
        }
    }

    // --------------------------------------------------- classification

    #[test]
    fn entity_created_notifies_only_when_the_entity_is_pending() {
        assert_eq!(
            notification_for_event("entity.created", Some("awaiting_approval")),
            Some(NotificationKind::Approval)
        );
        assert_eq!(
            notification_for_event("entity.created", Some("pending_approval")),
            Some(NotificationKind::Approval)
        );
        assert_eq!(notification_for_event("entity.created", Some("approved")), None);
        assert_eq!(notification_for_event("entity.created", None), None);
    }

    #[test]
    fn build_and_brief_kinds_match_their_real_event_strings() {
        assert_eq!(notification_for_event("build.node.gated", None), Some(NotificationKind::BuildGate));
        assert_eq!(notification_for_event("pipeline.stage.gated", None), Some(NotificationKind::BuildGate));
        assert_eq!(notification_for_event("brief.sent", None), Some(NotificationKind::Brief));
        assert_eq!(notification_for_event("task.completed", None), None);
    }

    #[test]
    fn build_gate_payload_names_the_node_or_stage_when_present() {
        let with_node = NotificationKind::BuildGate.payload(&serde_json::json!({ "node": "T3-widgets" }));
        assert!(with_node.body.contains("T3-widgets"));
        assert_eq!(with_node.url, "/observe");

        let with_stage = NotificationKind::BuildGate.payload(&serde_json::json!({ "stage": "review" }));
        assert!(with_stage.body.contains("review"));

        let bare = NotificationKind::BuildGate.payload(&serde_json::json!({}));
        assert!(!bare.body.is_empty());
    }

    #[test]
    fn approval_and_brief_payloads_deep_link_to_the_right_page() {
        assert_eq!(NotificationKind::Approval.payload(&Value::Null).url, "/approvals");
        assert_eq!(NotificationKind::Brief.payload(&Value::Null).url, "/dashboard");
    }

    // ------------------------------------------------------------- vapid

    #[test]
    fn vapid_config_requires_all_three_env_vars() {
        assert!(vapid_config_from(Some("pub".into()), Some("priv".into()), Some("mailto:a@b.com".into())).is_some());
        assert!(vapid_config_from(None, Some("priv".into()), Some("mailto:a@b.com".into())).is_none());
        assert!(vapid_config_from(Some("pub".into()), None, Some("mailto:a@b.com".into())).is_none());
        assert!(vapid_config_from(Some("pub".into()), Some("priv".into()), None).is_none());
        assert!(vapid_config_from(None, None, None).is_none());
    }

    // ------------------------------------------------------------ fan-out

    #[tokio::test]
    async fn approval_entity_created_pushes_to_every_workspace_subscription() {
        let conn = fresh_conn("test_push_approval.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_entity(&conn, "ent_gate", "ws1", "pending_approval", "awaiting_approval").await;
        insert_event(&conn, "evt_0001", "ws1", "entity.created", Some("ent_gate"), &serde_json::json!({})).await;
        insert_subscription(&conn, "push_1", "ws1", "https://push.example/a").await;
        insert_subscription(&conn, "push_2", "ws1", "https://push.example/b").await;

        let sender = MockPushSender::default();
        let sent = process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap();

        assert_eq!(sent, 2);
        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|(_, payload)| payload.url == "/approvals"));

        let _ = std::fs::remove_file("test_push_approval.db");
    }

    #[tokio::test]
    async fn entity_created_for_a_non_pending_entity_sends_nothing() {
        let conn = fresh_conn("test_push_nonpending.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_entity(&conn, "ent_task", "ws1", "task", "done").await;
        insert_event(&conn, "evt_0001", "ws1", "entity.created", Some("ent_task"), &serde_json::json!({})).await;
        insert_subscription(&conn, "push_1", "ws1", "https://push.example/a").await;

        let sender = MockPushSender::default();
        let sent = process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap();

        assert_eq!(sent, 0);
        assert_eq!(sender.calls.lock().unwrap().len(), 0);

        let _ = std::fs::remove_file("test_push_nonpending.db");
    }

    #[tokio::test]
    async fn build_gate_and_brief_events_each_push_with_their_own_url() {
        let conn = fresh_conn("test_push_build_brief.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "build.node.gated", None, &serde_json::json!({ "node": "T3-x" })).await;
        insert_event(&conn, "evt_0002", "ws1", "pipeline.stage.gated", None, &serde_json::json!({ "stage": "review" })).await;
        insert_event(&conn, "evt_0003", "ws1", "brief.sent", Some("ent_brief"), &serde_json::json!({})).await;
        insert_subscription(&conn, "push_1", "ws1", "https://push.example/a").await;

        let sender = MockPushSender::default();
        let sent = process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap();

        assert_eq!(sent, 3);
        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls[0].1.url, "/observe");
        assert_eq!(calls[1].1.url, "/observe");
        assert_eq!(calls[2].1.url, "/dashboard");

        let _ = std::fs::remove_file("test_push_build_brief.db");
    }

    #[tokio::test]
    async fn non_matching_event_advances_cursor_without_sending() {
        let conn = fresh_conn("test_push_nomatch.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "task.completed", None, &serde_json::json!({})).await;

        let sender = MockPushSender::default();
        let sent = process_workspace_notifications(&conn, "ws1", &sender, 101).await.unwrap();

        assert_eq!(sent, 0);
        assert_eq!(get_cursor(&conn, "ws1").await.unwrap(), "evt_0001");

        let _ = std::fs::remove_file("test_push_nomatch.db");
    }

    #[tokio::test]
    async fn reprocessing_after_cursor_advance_does_not_resend() {
        let conn = fresh_conn("test_push_cursor.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "brief.sent", None, &serde_json::json!({})).await;
        insert_subscription(&conn, "push_1", "ws1", "https://push.example/a").await;

        let sender = MockPushSender::default();
        assert_eq!(process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap(), 1);
        // Second tick with no new events - must not re-push the same event.
        assert_eq!(process_workspace_notifications(&conn, "ws1", &sender, 101).await.unwrap(), 0);
        assert_eq!(sender.calls.lock().unwrap().len(), 1);

        let _ = std::fs::remove_file("test_push_cursor.db");
    }

    #[tokio::test]
    async fn a_gone_response_prunes_only_that_subscription() {
        let conn = fresh_conn("test_push_prune.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "brief.sent", None, &serde_json::json!({})).await;
        insert_subscription(&conn, "push_dead", "ws1", "https://push.example/dead").await;
        insert_subscription(&conn, "push_alive", "ws1", "https://push.example/alive").await;

        let mut outcomes = std::collections::HashMap::new();
        outcomes.insert("https://push.example/dead".to_string(), PushOutcome::Gone);
        let sender = MockPushSender { outcomes, calls: Mutex::new(vec![]) };

        let sent = process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap();

        assert_eq!(sent, 1, "only the alive subscription counts as delivered");
        assert_eq!(subscription_count(&conn, "ws1").await, 1, "the dead subscription was pruned");

        let _ = std::fs::remove_file("test_push_prune.db");
    }

    #[tokio::test]
    async fn a_failed_response_leaves_the_subscription_in_place() {
        let conn = fresh_conn("test_push_failed.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "brief.sent", None, &serde_json::json!({})).await;
        insert_subscription(&conn, "push_flaky", "ws1", "https://push.example/flaky").await;

        let mut outcomes = std::collections::HashMap::new();
        outcomes.insert("https://push.example/flaky".to_string(), PushOutcome::Failed);
        let sender = MockPushSender { outcomes, calls: Mutex::new(vec![]) };

        let sent = process_workspace_notifications(&conn, "ws1", &sender, 100).await.unwrap();

        assert_eq!(sent, 0);
        assert_eq!(subscription_count(&conn, "ws1").await, 1, "a transient failure must not prune the subscription");

        let _ = std::fs::remove_file("test_push_failed.db");
    }

    #[tokio::test]
    async fn tick_scans_every_workspace_with_independent_cursors() {
        let conn = fresh_conn("test_push_multiws.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_workspace(&conn, "ws2").await;
        insert_event(&conn, "evt_a1", "ws1", "brief.sent", None, &serde_json::json!({})).await;
        insert_event(&conn, "evt_b1", "ws2", "brief.sent", None, &serde_json::json!({})).await;
        insert_event(&conn, "evt_b2", "ws2", "brief.sent", None, &serde_json::json!({})).await;
        insert_subscription(&conn, "push_a", "ws1", "https://push.example/a").await;
        insert_subscription(&conn, "push_b", "ws2", "https://push.example/b").await;

        let sender = MockPushSender::default();
        let total = run_push_notification_tick(&conn, &sender, 102).await.unwrap();

        assert_eq!(total, 3);
        assert_eq!(run_push_notification_tick(&conn, &sender, 103).await.unwrap(), 0, "second tick sends nothing new");

        let _ = std::fs::remove_file("test_push_multiws.db");
    }

    #[tokio::test]
    async fn disabled_sender_advances_cursors_without_counting_as_sent() {
        let conn = fresh_conn("test_push_disabled.db").await;
        insert_workspace(&conn, "ws1").await;
        insert_event(&conn, "evt_0001", "ws1", "brief.sent", None, &serde_json::json!({})).await;
        insert_subscription(&conn, "push_1", "ws1", "https://push.example/a").await;

        let sent = process_workspace_notifications(&conn, "ws1", &NoopPushSender, 100).await.unwrap();

        assert_eq!(sent, 0);
        assert_eq!(get_cursor(&conn, "ws1").await.unwrap(), "evt_0001");
        assert_eq!(subscription_count(&conn, "ws1").await, 1, "disabled sender must never prune");

        let _ = std::fs::remove_file("test_push_disabled.db");
    }
}
