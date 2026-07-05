//! Module marketplace: publish/sign/install with local re-validation
//! (issues #101/#102, `docs/PLATFORM-SYSTEMS.md`, `docs/SECURITY.md` §3).
//!
//! Publish signs the manifest's canonical JSON bytes with the platform's
//! ed25519 key and stores the package + signature. Install (or any third
//! party) re-verifies the signature locally before trusting the manifest -
//! a single tampered byte changes the signed bytes and fails verification.
//! Turning a verified manifest into an actual on-disk module install (git
//! commit under `modules/<id>/`) is the Node scaffold layer's job
//! (`server/scaffold.js`, docs/SELF-EXTENSION.md) - this route's "install"
//! only covers the marketplace half: signature re-verification + an
//! `events` record, honestly scoped rather than faking the commit step.

use crate::auth::resolve_workspace;
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::marketplace_sign;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::{json, Value};

fn signing_key_or_501(state: &AppState) -> ApiResult<&SigningKey> {
    state.config.marketplace_signing_key.as_ref().ok_or_else(|| {
        ApiError::NotImplemented(
            "marketplace signing key not configured - set LIFEOS_MARKETPLACE_SIGNING_SEED".into(),
        )
    })
}

/// `GET /api/marketplace/pubkey` - the platform's public signing key,
/// base64. Safe to publish; anyone can verify with it, no one can sign.
pub async fn pubkey(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let key = signing_key_or_501(&state)?;
    Ok(Json(json!({ "pubkey": marketplace_sign::public_key_b64(key) })))
}

#[derive(Deserialize)]
pub struct PublishRequest {
    module_id: String,
    version: String,
    manifest: Value,
    workspace_id: Option<String>,
}

fn structural_check(manifest: &Value, module_id: &str, version: &str) -> ApiResult<()> {
    let obj = manifest
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("manifest must be a JSON object".into()))?;
    let manifest_id = obj.get("id").and_then(Value::as_str);
    if manifest_id != Some(module_id) {
        return Err(ApiError::BadRequest(format!(
            "manifest.id ('{}') must match module_id ('{module_id}')",
            manifest_id.unwrap_or("<missing>")
        )));
    }
    let manifest_version = obj.get("version").and_then(Value::as_str);
    if manifest_version != Some(version) {
        return Err(ApiError::BadRequest(format!(
            "manifest.version ('{}') must match version ('{version}')",
            manifest_version.unwrap_or("<missing>")
        )));
    }
    Ok(())
}

/// `POST /api/marketplace/publish` - structural check, ed25519-sign the
/// manifest's canonical JSON bytes, store the package. The render validator
/// (headless-Chromium boot, `server/validators/render.js`) stays in the Node
/// scaffold layer - out of scope for this HTTP route.
pub async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PublishRequest>,
) -> ApiResult<Json<Value>> {
    if req.module_id.trim().is_empty() || req.version.trim().is_empty() {
        return Err(ApiError::BadRequest("module_id and version are required".into()));
    }
    structural_check(&req.manifest, &req.module_id, &req.version)?;
    let key = signing_key_or_501(&state)?;

    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let manifest_bytes = serde_json::to_vec(&req.manifest)
        .map_err(|_| ApiError::BadRequest("manifest is not serializable".into()))?;
    let signature = marketplace_sign::sign(key, &manifest_bytes);
    let pubkey = marketplace_sign::public_key_b64(key);

    let id = new_id("pkg");
    let now = now_secs();
    state
        .conn
        .execute(
            "INSERT INTO module_packages (id, workspace_id, module_id, version, manifest_json, signature, publisher_pubkey, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                id.clone(),
                workspace_id.clone(),
                req.module_id.clone(),
                req.version.clone(),
                String::from_utf8(manifest_bytes).unwrap(),
                signature.clone(),
                pubkey.clone(),
                now
            ],
        )
        .await?;

    crate::audit::emit(
        &state.conn,
        &workspace_id,
        "marketplace.published",
        Some(&id),
        "api",
        &json!({ "module_id": req.module_id, "version": req.version }),
    )
    .await?;

    Ok(Json(json!({ "package_id": id, "signature": signature, "pubkey": pubkey })))
}

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
    module_id: Option<String>,
}

/// `GET /api/marketplace/packages` - browse published packages.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let mut sql = "SELECT id, workspace_id, module_id, version, manifest_json, signature, publisher_pubkey, created_at \
                    FROM module_packages WHERE workspace_id = ?1"
        .to_string();
    let mut binds: Vec<String> = vec![workspace_id];
    if let Some(module_id) = &params.module_id {
        sql.push_str(" AND module_id = ?2");
        binds.push(module_id.clone());
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut rows = state.conn.query(&sql, libsql::params_from_iter(binds)).await?;
    let mut packages = Vec::new();
    while let Some(row) = rows.next().await? {
        packages.push(package_row_to_json(&row)?);
    }
    Ok(Json(json!({ "packages": packages })))
}

#[derive(Deserialize)]
pub struct VerifyRequest {
    manifest: Value,
    signature: String,
    pubkey: String,
}

/// `POST /api/marketplace/verify` - a tampered manifest fails verification
/// (issue #101 acceptance). Generic: works against any manifest/signature/
/// pubkey triple, not just ones this server published.
pub async fn verify(Json(req): Json<VerifyRequest>) -> ApiResult<Json<Value>> {
    let manifest_bytes = serde_json::to_vec(&req.manifest)
        .map_err(|_| ApiError::BadRequest("manifest is not serializable".into()))?;
    let valid = marketplace_sign::verify(&req.pubkey, &manifest_bytes, &req.signature);
    Ok(Json(json!({ "valid": valid })))
}

#[derive(Deserialize)]
pub struct InstallRequest {
    package_id: String,
}

/// `POST /api/marketplace/install` - re-verifies the stored signature
/// against the stored manifest before recording an install event. A
/// tampered `manifest_json` (or a package whose signature was forged)
/// fails closed with 400, never silently "installs".
pub async fn install(State(state): State<AppState>, Json(req): Json<InstallRequest>) -> ApiResult<Json<Value>> {
    let mut rows = state
        .conn
        .query(
            "SELECT id, workspace_id, module_id, version, manifest_json, signature, publisher_pubkey, created_at \
             FROM module_packages WHERE id = ?1",
            libsql::params![req.package_id.clone()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("package '{}' not found", req.package_id)))?;
    let package = package_row_to_json(&row)?;

    let manifest_bytes = serde_json::to_vec(&package["manifest"]).unwrap_or_default();
    let signature = package["signature"].as_str().unwrap_or_default();
    let pubkey = package["publisher_pubkey"].as_str().unwrap_or_default();
    if !marketplace_sign::verify(pubkey, &manifest_bytes, signature) {
        return Err(ApiError::BadRequest(
            "signature verification failed - manifest or signature has been tampered with".into(),
        ));
    }

    let workspace_id = package["workspace_id"].as_str().unwrap_or_default().to_string();
    crate::audit::emit(
        &state.conn,
        &workspace_id,
        "marketplace.installed",
        Some(&req.package_id),
        "api",
        &json!({ "module_id": package["module_id"], "version": package["version"] }),
    )
    .await?;

    Ok(Json(json!({ "installed": true, "manifest": package["manifest"] })))
}

fn package_row_to_json(row: &libsql::Row) -> ApiResult<Value> {
    let id: String = row.get(0)?;
    let workspace_id: String = row.get(1)?;
    let module_id: String = row.get(2)?;
    let version: String = row.get(3)?;
    let manifest_json: String = row.get(4)?;
    let signature: String = row.get(5)?;
    let publisher_pubkey: String = row.get(6)?;
    let created_at: i64 = row.get(7)?;
    let manifest: Value = serde_json::from_str(&manifest_json).unwrap_or(json!({}));
    Ok(json!({
        "id": id,
        "workspace_id": workspace_id,
        "module_id": module_id,
        "version": version,
        "manifest": manifest,
        "signature": signature,
        "publisher_pubkey": publisher_pubkey,
        "created_at": created_at,
    }))
}
