//! `/api/storage/backends` - per-workspace storage-backend configuration
//! (issue #107, docs/STORAGE-BACKENDS.md §4).
//!
//! Adding/switching a backend MOVES the user's data, so `create` is gated:
//! it only ever writes a `pending_approval` config entity (the same
//! draft -> approve -> active flow as every outward action,
//! docs/SECURITY.md §2). Keys never enter `attrs` - they are
//! envelope-encrypted into `connections.secret_enc` and the entity carries
//! only the `connection_id` handle. Storage selection is part of the
//! connections protected domain: the in-app agent can read/render content
//! but cannot call this route to reconfigure storage
//! (docs/AGENT-CONTROL.md §1, docs/STORAGE-BACKENDS.md §6).

use crate::audit::emit;
use crate::auth::resolve_workspace;
use crate::db::{index_entity, workspace_exists};
use crate::error::{ApiError, ApiResult};
use crate::ids::{new_id, now_secs};
use crate::models::{read_entity, Entity, COLS_ENTITY};
use crate::state::AppState;
use crate::storage::STORAGE_KINDS;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ListParams {
    workspace_id: Option<String>,
}

/// `GET /api/storage/backends` - free read: every backend config in the
/// workspace, any status, so the UI can show pending drafts too.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Entity>>> {
    let workspace_id = resolve_workspace(&headers, &state.config, params.workspace_id.as_deref())?;
    let mut rows = state
        .conn
        .query(
            &format!(
                "SELECT {COLS_ENTITY} FROM entities \
                 WHERE workspace_id = ?1 AND module = 'storage' AND type = 'storage_backend' \
                 ORDER BY created_at DESC"
            ),
            libsql::params![workspace_id],
        )
        .await?;
    let mut entities = Vec::new();
    while let Some(row) = rows.next().await? {
        entities.push(read_entity(&row)?);
    }
    Ok(Json(entities))
}

#[derive(Deserialize)]
pub struct CreateBackend {
    kind: String,
    /// Life OS app folder / prefix on the provider.
    folder: Option<String>,
    /// Existing connection to authenticate with (e.g. a Nango OAuth
    /// connection completed via /api/connections). Mutually exclusive with
    /// `keys`.
    connection_id: Option<String>,
    /// Non-Nango credentials (S3/R2/GCS/Azure/WebDAV). Envelope-encrypted
    /// into `connections.secret_enc` - never stored in entity attrs.
    keys: Option<Value>,
    /// Client-side envelope encryption at rest (issue #110).
    #[serde(default)]
    encryption: bool,
    /// Make this the primary (write) backend once approved.
    #[serde(default)]
    default: bool,
    workspace_id: Option<String>,
}

/// `POST /api/storage/backends` - GATED (docs/STORAGE-BACKENDS.md §4): only
/// drafts a `pending_approval` config entity; nothing changes where bytes
/// live until a human approves it.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateBackend>,
) -> ApiResult<Json<Entity>> {
    if !STORAGE_KINDS.contains(&req.kind.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "unknown backend kind '{}' (expected one of {})",
            req.kind,
            STORAGE_KINDS.join(", ")
        )));
    }
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    if !workspace_exists(&state.conn, &workspace_id).await? {
        return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'")));
    }
    if req.connection_id.is_some() && req.keys.is_some() {
        return Err(ApiError::BadRequest("pass either connection_id or keys, not both".into()));
    }

    let connection_id = match (&req.connection_id, &req.keys) {
        (Some(existing), None) => Some(existing.clone()),
        (None, Some(keys)) => Some(store_keys(&state, &workspace_id, &req.kind, keys).await?),
        (None, None) if req.kind == "local-fs" => None,
        (None, None) => {
            return Err(ApiError::BadRequest(format!("'{}' backend needs connection_id or keys", req.kind)))
        }
        (Some(_), Some(_)) => unreachable!("guarded above"),
    };

    // Only the handle and non-secret settings - key material never lands in
    // attrs, events, or logs.
    let attrs = json!({
        "kind": req.kind,
        "folder": req.folder,
        "connection_id": connection_id,
        "encryption": req.encryption,
        "default": req.default,
    });

    let id = new_id("ent");
    let now = now_secs();
    let attrs_str = serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into());
    let title = format!("{} backend", req.kind);
    state
        .conn
        .execute(
            "INSERT INTO entities \
             (id, workspace_id, module, type, parent_id, title, status, tier, attrs, source, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'storage', 'storage_backend', NULL, ?3, 'pending_approval', NULL, ?4, 'api', NULL, ?5, ?5)",
            libsql::params![id.clone(), workspace_id.clone(), title, attrs_str, now],
        )
        .await?;
    emit(&state.conn, &workspace_id, "storage.backend.drafted", Some(&id), "api", &attrs).await?;
    if let Err(e) = index_entity(&state.conn, &id).await {
        tracing::warn!("derived index upsert failed for {id}: {e}");
    }

    let mut rows = state
        .conn
        .query(&format!("SELECT {COLS_ENTITY} FROM entities WHERE id = ?1"), libsql::params![id])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Json(read_entity(&row)?)),
        None => Err(ApiError::Internal("backend draft vanished after insert".into())),
    }
}

#[derive(Deserialize)]
pub struct MigrateRequest {
    /// The `storage_backend` config entity to become the new primary. Must
    /// already be `active` - i.e. its creation was already human-approved
    /// (docs/STORAGE-BACKENDS.md §4), so the move itself is authorized.
    target_backend_id: String,
    workspace_id: Option<String>,
}

/// `POST /api/storage/migrate` - re-puts every live object onto the target
/// backend in-process, then flips the primary pointer (issue #108). Identity
/// is the hash, so no entity/edge/event/snapshot is rewritten. Returns 202 +
/// `migration_id`; progress lands in the target backend entity's
/// `attrs.migration`; `has`-before-put makes a re-run resume where it stopped.
///
/// This intentionally does NOT enqueue a `jobs` row (finding: double-claim).
/// The migration runs entirely inside this API process - it is the only
/// place holding the Nango/secret_enc clients storage backends need - but
/// `lifeos-drain`'s `claim_job` claims the highest-priority *queued* row
/// with no kind filter, and its `dispatch()` stub-completes `storage_migrate`
/// the instant it is claimed (`Dispatch::Stub`, `services/lifeos-drain/src/lib.rs`).
/// A drain node polling concurrently would therefore mark the job 'done'
/// while objects are still copying, and its own status write would race this
/// function's. Since drain claims any kind, there is no claimable-but-ignored
/// kind to pick either - so progress is tracked on the entity instead of in
/// `jobs`, which nothing but this handler ever writes to for this backend id.
pub async fn migrate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MigrateRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<Value>)> {
    let workspace_id = resolve_workspace(&headers, &state.config, req.workspace_id.as_deref())?;
    let target = fetch_backend_config(&state, &workspace_id, &req.target_backend_id).await?;
    if target.status.as_deref() != Some("active") {
        return Err(ApiError::BadRequest(
            "target backend is not active - approve it first (gated action)".into(),
        ));
    }

    let migration_id = new_id("mig");
    mark_migration_running(&state, &req.target_backend_id, &migration_id).await?;

    let task_state = state.clone();
    let task_ws = workspace_id.clone();
    let task_migration = migration_id.clone();
    let task_target = req.target_backend_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_migration(&task_state, &task_ws, &task_migration, &task_target).await {
            tracing::error!("storage migration {task_migration} failed: {e:?}");
            let _ = mark_migration_failed(&task_state, &task_target, &task_migration, &format!("{e:?}")).await;
        }
    });
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(json!({ "status": "queued", "migration_id": migration_id, "target_backend_id": req.target_backend_id })),
    ))
}

async fn fetch_backend_config(state: &AppState, workspace_id: &str, id: &str) -> ApiResult<Entity> {
    let mut rows = state
        .conn
        .query(
            &format!(
                "SELECT {COLS_ENTITY} FROM entities \
                 WHERE id = ?1 AND workspace_id = ?2 AND module = 'storage' AND type = 'storage_backend'"
            ),
            libsql::params![id, workspace_id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(read_entity(&row)?),
        None => Err(ApiError::NotFound(format!("no storage backend '{id}'"))),
    }
}

/// The migration itself: every live object hash (entity blob_refs +
/// snapshots + their chunks) re-put onto the target with fallback reads
/// across the local CAS and existing backends, progress persisted onto the
/// target entity's `attrs.migration`, and - only on a fully clean run - the
/// primary pointer flip.
async fn run_migration(state: &AppState, workspace_id: &str, migration_id: &str, target_id: &str) -> ApiResult<()> {
    let target_entity = fetch_backend_config(state, workspace_id, target_id).await?;
    let target = crate::storage::backend_from_config(state, workspace_id, target_id, &target_entity.attrs).await?;
    let sources = crate::storage::read_backends(state, workspace_id).await?;

    let live = lifeos_vcs::live_object_hashes(&state.conn, &state.vcs_store)
        .await
        .map_err(|e| ApiError::Internal(format!("live-set scan failed: {e}")))?;
    let mut hashes: Vec<String> = live.into_iter().collect();
    hashes.sort();

    let report = lifeos_vcs::migrate_objects(&sources, target.as_ref(), &hashes, |_, _| {})
        .await
        .map_err(|e| ApiError::Internal(format!("migration failed: {e}")))?;

    let progress = json!({
        "migration_id": migration_id,
        "target_backend_id": target_id,
        "migrated": report.migrated,
        "skipped": report.skipped,
        "failed": report.failed,
        "total": hashes.len(),
    });
    if report.failed > 0 {
        let error = format!("{} object(s) could not be migrated", report.failed);
        mark_migration_finished(state, target_id, migration_id, "failed", &progress, Some(&error)).await?;
        emit(&state.conn, workspace_id, "storage.migration.failed", Some(target_id), "api", &progress).await?;
        return Ok(()); // primary pointer NOT flipped on a partial copy
    }

    // Flip the primary pointer: target becomes default, everything else not.
    let now = now_secs();
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = json_set(attrs, '$.default', json('false')), updated_at = ?1 \
             WHERE workspace_id = ?2 AND module = 'storage' AND type = 'storage_backend' AND id != ?3",
            libsql::params![now, workspace_id, target_id],
        )
        .await?;
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = json_set(attrs, '$.default', json('true')), updated_at = ?1 WHERE id = ?2",
            libsql::params![now, target_id],
        )
        .await?;
    mark_migration_finished(state, target_id, migration_id, "done", &progress, None).await?;
    emit(&state.conn, workspace_id, "storage.migrated", Some(target_id), "api", &progress).await?;
    Ok(())
}

/// Stamps `attrs.migration = { id, status: 'running', started_at }` onto the
/// target backend entity. Called synchronously before the migration task is
/// spawned, so a client that lists backends immediately after the 202
/// response already sees the in-flight state.
async fn mark_migration_running(state: &AppState, target_id: &str, migration_id: &str) -> ApiResult<()> {
    let now = now_secs();
    let migration = json!({ "id": migration_id, "status": "running", "started_at": now });
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = json_set(attrs, '$.migration', json(?1)), updated_at = ?2 WHERE id = ?3",
            libsql::params![migration.to_string(), now, target_id],
        )
        .await?;
    Ok(())
}

/// Stamps the terminal `attrs.migration` state (`done` or `failed`) with the
/// object-copy report, merged from `run_migration`'s own success/failure
/// path (not the panic/error fallback - see `mark_migration_failed`).
async fn mark_migration_finished(
    state: &AppState,
    target_id: &str,
    migration_id: &str,
    status: &str,
    progress: &Value,
    error: Option<&str>,
) -> ApiResult<()> {
    let mut migration = progress.clone();
    migration["id"] = json!(migration_id);
    migration["status"] = json!(status);
    migration["finished_at"] = json!(now_secs());
    if let Some(e) = error {
        migration["error"] = json!(e);
    }
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = json_set(attrs, '$.migration', json(?1)), updated_at = ?2 WHERE id = ?3",
            libsql::params![migration.to_string(), now_secs(), target_id],
        )
        .await?;
    Ok(())
}

/// Terminal failure path for an error `run_migration` returned before it
/// could compute a progress report (e.g. the backend factory or live-set
/// scan itself failed) - `migrate()`'s spawned task calls this directly.
async fn mark_migration_failed(state: &AppState, target_id: &str, migration_id: &str, error: &str) -> ApiResult<()> {
    let now = now_secs();
    let migration = json!({ "id": migration_id, "status": "failed", "finished_at": now, "error": error });
    state
        .conn
        .execute(
            "UPDATE entities SET attrs = json_set(attrs, '$.migration', json(?1)), updated_at = ?2 WHERE id = ?3",
            libsql::params![migration.to_string(), now, target_id],
        )
        .await?;
    Ok(())
}

/// Envelope-encrypts non-Nango backend keys into a `connections` row and
/// returns its id - the only artifact the config entity references.
async fn store_keys(state: &AppState, workspace_id: &str, kind: &str, keys: &Value) -> ApiResult<String> {
    let enc_key = state
        .config
        .secret_encryption_key
        .as_ref()
        .ok_or_else(|| ApiError::NotImplemented("LIFEOS_SECRET_ENCRYPTION_KEY is not configured".into()))?;
    let plaintext = serde_json::to_string(keys).map_err(|_| ApiError::BadRequest("keys must be a JSON object".into()))?;
    let secret_enc = crate::crypto::encrypt(&plaintext, enc_key)?;

    let connection_id = new_id("conn");
    state
        .conn
        .execute(
            "INSERT INTO connections (id, workspace_id, provider, account_handle, nango_connection_id, secret_enc, scopes, expires_at, status, created_at) \
             VALUES (?1, ?2, ?3, NULL, NULL, ?4, NULL, NULL, 'active', ?5)",
            libsql::params![connection_id.clone(), workspace_id, format!("storage-{kind}"), secret_enc, now_secs()],
        )
        .await?;
    Ok(connection_id)
}
