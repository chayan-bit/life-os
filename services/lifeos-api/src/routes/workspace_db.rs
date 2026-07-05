//! Database-per-workspace provisioning + per-workspace envelope key (issue
//! #104, `docs/DATA-MODEL.md`, `docs/SECURITY.md` §5). No billing/quota seam
//! here by design - this is a self-hosted, bring-your-own-database-and-AI-
//! model project; workspaces isolate by database, not by metered plan.

use crate::auth::resolve_workspace;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::ids::now_secs;
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};

fn platform_token_or_501(state: &AppState) -> ApiResult<(&str, &str)> {
    match (&state.config.turso_platform_api_token, &state.config.turso_org_slug) {
        (Some(token), Some(org)) => Ok((token.as_str(), org.as_str())),
        _ => Err(ApiError::NotImplemented(
            "Turso platform API not configured - set TURSO_PLATFORM_API_TOKEN and TURSO_ORG_SLUG".into(),
        )),
    }
}

/// Ensures `workspaces.envelope_key_enc` is set (shared logic in
/// `crypto::ensure_envelope_key` since issue #110 uses the same key).
async fn ensure_envelope_key(state: &AppState, workspace_id: &str) -> ApiResult<crypto::EncryptionKey> {
    let master_key = state.config.secret_encryption_key.as_ref().ok_or_else(|| {
        ApiError::NotImplemented("LIFEOS_SECRET_ENCRYPTION_KEY not configured".into())
    })?;
    crypto::ensure_envelope_key(&state.conn, master_key, workspace_id).await
}

#[derive(Deserialize)]
pub struct ProvisionRequest {
    workspace_id: Option<String>,
}

/// `POST /api/workspace/provision-db` - creates a dedicated Turso database
/// for the workspace via the Turso platform API, generates that workspace's
/// envelope key if it doesn't exist yet, and stores the new database's auth
/// token envelope-encrypted under it. The token itself is never returned.
pub async fn provision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvisionRequest>,
) -> ApiResult<Json<Value>> {
    let (api_token, org) = platform_token_or_501(&state)?;
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;

    let db_name = format!("lifeos-{}", workspace_id.replace('_', "-").to_lowercase());
    let client = reqwest::Client::new();
    let create_res = client
        .post(format!("https://api.turso.tech/v1/organizations/{org}/databases"))
        .bearer_auth(api_token)
        .json(&json!({ "name": db_name }))
        .send()
        .await
        .map_err(|e| ApiError::Upstream(format!("Turso platform API unreachable: {e}")))?;
    if !create_res.status().is_success() {
        let status = create_res.status();
        let body = create_res.text().await.unwrap_or_default();
        return Err(ApiError::Upstream(format!("Turso database creation failed ({status}): {body}")));
    }
    let created: Value = create_res
        .json()
        .await
        .map_err(|e| ApiError::Upstream(format!("Turso response was not JSON: {e}")))?;
    let db_hostname = created["database"]["Hostname"]
        .as_str()
        .or_else(|| created["database"]["hostname"].as_str())
        .ok_or_else(|| ApiError::Upstream("Turso response missing database hostname".into()))?;
    let db_url = format!("libsql://{db_hostname}");

    let token_res = client
        .post(format!(
            "https://api.turso.tech/v1/organizations/{org}/databases/{db_name}/auth/tokens"
        ))
        .bearer_auth(api_token)
        .send()
        .await
        .map_err(|e| ApiError::Upstream(format!("Turso token mint unreachable: {e}")))?;
    if !token_res.status().is_success() {
        let status = token_res.status();
        let body = token_res.text().await.unwrap_or_default();
        return Err(ApiError::Upstream(format!("Turso token mint failed ({status}): {body}")));
    }
    let token_body: Value = token_res
        .json()
        .await
        .map_err(|e| ApiError::Upstream(format!("Turso token response was not JSON: {e}")))?;
    let db_token = token_body["jwt"]
        .as_str()
        .ok_or_else(|| ApiError::Upstream("Turso response missing jwt".into()))?;

    let envelope_key = ensure_envelope_key(&state, &workspace_id).await?;
    let token_enc = crypto::encrypt(db_token, &envelope_key)?;

    state
        .conn
        .execute(
            "INSERT INTO workspace_databases (workspace_id, turso_db_name, turso_db_url, turso_token_enc, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (workspace_id) DO UPDATE SET turso_db_name = excluded.turso_db_name, turso_db_url = excluded.turso_db_url, turso_token_enc = excluded.turso_token_enc",
            libsql::params![workspace_id.clone(), db_name.clone(), db_url.clone(), token_enc, now_secs()],
        )
        .await?;

    crate::audit::emit(
        &state.conn,
        &workspace_id,
        "workspace.db_provisioned",
        None,
        "api",
        &json!({ "db_name": db_name }),
    )
    .await?;

    Ok(Json(json!({ "db_name": db_name, "db_url": db_url })))
}

/// `GET /api/workspace/database` - the provisioned database's name/url, if
/// any. Never returns the auth token.
pub async fn get_database(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let mut rows = state
        .conn
        .query(
            "SELECT turso_db_name, turso_db_url, created_at FROM workspace_databases WHERE workspace_id = ?1",
            libsql::params![workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(json!({
            "provisioned": true,
            "db_name": row.get::<String>(0)?,
            "db_url": row.get::<String>(1)?,
            "created_at": row.get::<i64>(2)?,
        }))),
        None => Ok(Json(json!({ "provisioned": false }))),
    }
}
