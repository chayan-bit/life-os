//! Browser actuator routes (issue #54, docs/SECURITY.md §4). `scrape` is
//! free - the underlying client has no state-changing action available to
//! it at all (`browser::BrowserActuator`). `act` is gated: it only ever
//! creates a draft entity via `integrations::draft_action`, which has no
//! reference to `state.browser` - there is no code path from a gated
//! request to an actual browser session. `session` is the one interactive,
//! Mac-only exception, mirroring Kite's daily login / GOWA's QR pairing.

use crate::auth::resolve_workspace;
use crate::crypto;
use crate::db::workspace_exists;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::integrations::draft_action;
use crate::models::{read_connection, Connection, Entity};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};

fn browser_or_501(state: &AppState) -> ApiResult<&dyn crate::browser::BrowserActuator> {
    state
        .browser
        .as_deref()
        .ok_or_else(|| ApiError::NotImplemented("browser actuator is not configured - see docs/MANUAL-SETUP.md #54".into()))
}

fn encryption_key_or_501(state: &AppState) -> ApiResult<&crypto::EncryptionKey> {
    state
        .config
        .secret_encryption_key
        .as_ref()
        .ok_or_else(|| ApiError::NotImplemented("LIFEOS_SECRET_ENCRYPTION_KEY is not set - see docs/MANUAL-SETUP.md #54".into()))
}

#[derive(Deserialize)]
pub struct Scrape {
    url: String,
    task: String,
    workspace_id: Option<String>,
}

/// `POST /api/browser/scrape` - free: the browser-use process this calls
/// has click/type/submit/upload excluded from its action space entirely, so
/// it structurally cannot change any external state.
pub async fn scrape(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Scrape>,
) -> ApiResult<Json<Value>> {
    if req.url.trim().is_empty() || req.task.trim().is_empty() {
        return Err(ApiError::BadRequest("url and task are required".into()));
    }
    let browser = browser_or_501(&state)?;
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let result = browser.scrape(&req.url, &req.task).await?;
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct Act {
    task: String,
    site: Option<String>,
    workspace_id: Option<String>,
}

/// `POST /api/browser/act` - gated (docs/SECURITY.md §2): only creates a
/// draft entity, never touches the browser actuator.
pub async fn act(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Act>,
) -> ApiResult<Json<Entity>> {
    if req.task.trim().is_empty() {
        return Err(ApiError::BadRequest("task is required".into()));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    let attrs = json!({ "task": req.task, "site": req.site });
    let entity = draft_action(&state, &workspace_id, "browser", "act", attrs).await?;
    Ok(Json(entity))
}

#[derive(Deserialize)]
pub struct CaptureSession {
    site: String,
    workspace_id: Option<String>,
}

/// `POST /api/connections/browser/session` - interactive, Mac-only: opens a
/// real browser window for the user to log into `site` themselves, then
/// envelope-encrypts the captured session before it ever touches the DB.
/// The raw or encrypted session never appears in this handler's response.
pub async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CaptureSession>,
) -> ApiResult<Json<Connection>> {
    if req.site.trim().is_empty() {
        return Err(ApiError::BadRequest("site is required".into()));
    }
    let browser = browser_or_501(&state)?;
    let key = encryption_key_or_501(&state)?;
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }

    let raw_session = browser.capture_session(&req.site).await?;
    let secret_enc = crypto::encrypt(&raw_session, key)?;

    let provider = format!("browser:{}", req.site);
    let id = new_id("conn");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO connections \
             (id, workspace_id, provider, account_handle, nango_connection_id, secret_enc, scopes, expires_at, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, NULL, 'active', ?6)",
            libsql::params![id.clone(), workspace_id.clone(), provider.clone(), req.site.clone(), secret_enc, now],
        )
        .await?;

    crate::audit::emit(&state.conn, &workspace_id, "connection.connected", Some(&id), "api", &json!({ "provider": provider })).await?;

    fetch_connection(&state, &workspace_id, &id).await
}

async fn fetch_connection(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Json<Connection>> {
    let mut rows = state
        .conn
        .query(
            &format!("SELECT {} FROM connections WHERE id = ?1 AND workspace_id = ?2", crate::models::COLS_CONNECTION),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_connection(&row)?)),
        None => Err(ApiError::NotFound(format!("connection '{id}' not found"))),
    }
}
