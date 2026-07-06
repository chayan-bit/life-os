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
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Wall-clock ceiling on the Node package-validator subprocess. Structural
/// validation is fast (no browser/LLM); this is only a runaway backstop.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(60);

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

/// `POST /api/marketplace/install` - the trusted install gate. In order, and
/// failing closed at each step (issue #147 acceptance - validation on install
/// is non-negotiable):
///   1. re-verify the stored signature over the stored manifest - a tampered
///      `manifest_json` or forged signature 400s, never "installs";
///   2. re-run the Tier-0 validator chain on the manifest
///      (`server/validators/validatePackage.js`) - a structurally-invalid
///      manifest 400s even with a perfectly valid signature (a signature
///      proves provenance, not that the manifest is installable);
///   3. only then persist the `module_manifest` entity (so the module renders
///      live, issue #121) and record the install event.
///
/// The package is looked up globally by `package_id` (you install packages
/// others published), but the install RECORD - the `module_manifest` entity and
/// the `marketplace.installed` event - lands in the CALLER's workspace, never
/// the publisher's, so an install is tenant-correct.
pub async fn install(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<InstallRequest>,
) -> ApiResult<Json<Value>> {
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

    // Step 1 - signature: provenance + tamper-evidence.
    let manifest_bytes = serde_json::to_vec(&package["manifest"]).unwrap_or_default();
    let signature = package["signature"].as_str().unwrap_or_default();
    let pubkey = package["publisher_pubkey"].as_str().unwrap_or_default();
    if !marketplace_sign::verify(pubkey, &manifest_bytes, signature) {
        return Err(ApiError::BadRequest(
            "signature verification failed - manifest or signature has been tampered with".into(),
        ));
    }

    // Step 2 - validator re-run: the manifest must still pass the T0 gates.
    validate_manifest_via_node(&state, &package["manifest"]).await?;

    // Step 3 - activate into the CALLER's workspace (not the publisher's).
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let module_id = package["module_id"].as_str().unwrap_or_default();
    upsert_manifest_entity(&state, &workspace_id, module_id, &package["manifest"]).await?;
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

/// Parsed shape of `validatePackage.js`'s last stdout line.
#[derive(Debug, serde::Deserialize)]
struct ValidationResult {
    #[serde(default)]
    valid: bool,
    #[serde(default)]
    errors: Vec<String>,
}

/// Parses the LAST non-empty stdout line as the validator's JSON result. Pure +
/// `#[cfg(test)]`-covered so the line contract is verified without spawning
/// Node (same discipline as `agent.rs::parse_agent_output`).
fn parse_validation_output(stdout: &str, stderr: &str) -> Result<ValidationResult, String> {
    let last_line = stdout.lines().rev().find(|l| !l.trim().is_empty());
    let Some(last_line) = last_line else {
        return Err(format!("package validator produced no output (stderr: {stderr})"));
    };
    serde_json::from_str(last_line)
        .map_err(|e| format!("package validator output was not valid JSON: {e} (line: {last_line})"))
}

/// Re-runs the Tier-0 validator chain on a package manifest by shelling the
/// thin Node entry (`server/validators/validatePackage.js`) - the same
/// process + last-line-JSON contract `lifeos-drain` uses for build entries.
/// The manifest is staged to a temp file (never passed as an argv, which
/// would break on size/quoting). Fails CLOSED: a spawn failure, timeout,
/// unparseable output, or a `valid:false` result all reject the install.
async fn validate_manifest_via_node(state: &AppState, manifest: &Value) -> ApiResult<()> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|_| ApiError::BadRequest("manifest is not serializable".into()))?;
    let tmp = std::env::temp_dir().join(format!("{}.json", new_id("pkgval")));
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| ApiError::Internal(format!("could not stage manifest for validation: {e}")))?;

    let run = tokio::process::Command::new("node")
        .arg("validators/validatePackage.js")
        .arg(&tmp)
        .current_dir(&state.config.server_dir)
        .output();
    let output = match tokio::time::timeout(VALIDATE_TIMEOUT, run).await {
        Err(_) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError::Upstream("package validation timed out".into()));
        }
        Ok(Err(e)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApiError::Upstream(format!("failed to spawn package validator: {e}")));
        }
        Ok(Ok(o)) => o,
    };
    let _ = tokio::fs::remove_file(&tmp).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let result = parse_validation_output(&stdout, &stderr).map_err(ApiError::Upstream)?;
    if result.valid {
        Ok(())
    } else {
        let detail = if result.errors.is_empty() {
            "invalid manifest".to_string()
        } else {
            result.errors.join("; ")
        };
        Err(ApiError::BadRequest(format!("package failed validation: {detail}")))
    }
}

/// Persists (upserts) the installed manifest as the generic `module='system'` /
/// `type='module_manifest'` entity the live app reads (issue #121,
/// `server/lib/manifestEntity.js`) so a hot-installed module renders through
/// the real multi-view `ModuleManifestPage`. Keyed by `title =
/// module_manifest_<id>` (the write side's logical id), matching the Node
/// upsert exactly, so re-installing a newer version replaces in place.
async fn upsert_manifest_entity(
    state: &AppState,
    workspace_id: &str,
    module_id: &str,
    manifest: &Value,
) -> ApiResult<()> {
    let title = format!("module_manifest_{module_id}");
    let attrs_str = serde_json::to_string(manifest).unwrap_or_else(|_| "{}".into());
    let now = now_secs();

    let mut rows = state
        .conn
        .query(
            "SELECT id FROM entities WHERE workspace_id = ?1 AND module = 'system' \
             AND type = 'module_manifest' AND title = ?2 LIMIT 1",
            libsql::params![workspace_id, title.clone()],
        )
        .await?;
    let entity_id = if let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        state
            .conn
            .execute(
                "UPDATE entities SET attrs = ?1, updated_at = ?2 WHERE id = ?3 AND workspace_id = ?4",
                libsql::params![attrs_str, now, id.clone(), workspace_id],
            )
            .await?;
        id
    } else {
        let id = new_id("ent");
        state
            .conn
            .execute(
                "INSERT INTO entities \
                 (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
                 VALUES (?1, ?2, 'system', 'module_manifest', NULL, ?3, NULL, NULL, ?4, 'marketplace', NULL, ?5, ?5)",
                libsql::params![id.clone(), workspace_id, title, attrs_str, now],
            )
            .await?;
        id
    };
    // Keep the lexical search index live (best-effort; boot rebuild reconciles).
    if let Err(e) = crate::db::index_entity(&state.conn, &entity_id).await {
        tracing::warn!("derived index upsert failed for {entity_id}: {e}");
    }
    Ok(())
}

/// `GET /api/marketplace/package/:module_id/versions` - every published version
/// of a module in this workspace, newest first. The detail view renders this as
/// the version history; installing an older row's `package_id` is the rollback
/// path (it flows through the same signature + validator gate as any install).
pub async fn versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(module_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let workspace_id = resolve_workspace(&headers, &state.config, None)?;
    let mut rows = state
        .conn
        .query(
            "SELECT id, workspace_id, module_id, version, manifest_json, signature, publisher_pubkey, created_at \
             FROM module_packages WHERE workspace_id = ?1 AND module_id = ?2 ORDER BY created_at DESC",
            libsql::params![workspace_id, module_id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(package_row_to_json(&row)?);
    }
    Ok(Json(json!({ "versions": out })))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_result_from_the_last_stdout_line() {
        let stdout = "npm noise\n\n{\"valid\":true,\"errors\":[],\"tier\":\"T0\"}\n";
        let result = parse_validation_output(stdout, "").unwrap();
        assert!(result.valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn parses_an_invalid_result_with_its_errors() {
        let stdout = "{\"valid\":false,\"errors\":[\"(root) must have required property 'name'\"],\"tier\":\"T0\"}\n";
        let result = parse_validation_output(stdout, "").unwrap();
        assert!(!result.valid);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn errors_when_the_validator_produced_no_output() {
        let err = parse_validation_output("  \n\n", "boom on stderr").unwrap_err();
        assert!(err.contains("no output"));
        assert!(err.contains("boom on stderr"));
    }

    #[test]
    fn errors_when_the_last_line_is_not_json() {
        let err = parse_validation_output("not json at all\n", "").unwrap_err();
        assert!(err.contains("not valid JSON"));
    }
}
