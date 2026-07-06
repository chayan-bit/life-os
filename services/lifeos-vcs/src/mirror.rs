//! Out-of-band blob mirroring (issue #83, generalized by issue #105): blobs
//! sync via a [`StorageBackend`], never through the libSQL replica
//! (docs/VERSIONING.md §2.1/§5, CLAUDE.md hard rules).
//!
//! **Not yet wired into any caller.** [`BlobMirror`], [`pull_on_demand`], and
//! [`BlobMirror::from_r2_env`] are re-exported from the crate root but no
//! service in this workspace constructs or calls them yet - this is a real
//! planned feature (R2 mirror/pull-on-demand for the CAS), not dead code to
//! delete. `from_r2_env` is gated behind the `R2_BUCKET`, `R2_ENDPOINT`,
//! `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` env vars (unset anywhere today
//! - see docs/MANUAL-SETUP.md), so don't assume production blobs are
//! actually mirrored to R2 by reading this file alone.
//!
//! Historically this module talked to R2/S3 directly; it is now a thin
//! compatibility wrapper over [`ExternalObjectStoreBackend`]
//! (docs/STORAGE-BACKENDS.md §2) so the mirror is just "one more backend" -
//! the put/get/integrity logic lives in `backend.rs` and is shared with every
//! other user-chosen backend. `AmazonS3Builder` pointed at an R2 endpoint
//! remains the intended production path once wired up; tests stand a
//! `LocalFileSystem` remote in so the mirror/pull/integrity logic is
//! exercised without live R2 credentials in CI (the same boundary used for
//! the Nango/Kite connectors, docs/MANUAL-SETUP.md).

use std::fmt;
use std::sync::Arc;

use object_store::aws::AmazonS3Builder;
use object_store::ObjectStore as ExternalObjectStore;

use crate::backend::{BackendError, ExternalObjectStoreBackend, StorageBackend};
use crate::store::ObjectStore;

#[derive(Debug)]
pub enum MirrorError {
    Store(object_store::Error),
    Io(std::io::Error),
    /// The bytes pulled from the remote don't hash to the address we asked
    /// for - never trust the remote's bytes blindly.
    IntegrityMismatch { expected: String, actual: String },
}

impl fmt::Display for MirrorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirrorError::Store(e) => write!(f, "object store error: {e}"),
            MirrorError::Io(e) => write!(f, "local io error: {e}"),
            MirrorError::IntegrityMismatch { expected, actual } => {
                write!(f, "integrity mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for MirrorError {}

impl From<BackendError> for MirrorError {
    fn from(e: BackendError) -> Self {
        match e {
            BackendError::IntegrityMismatch { expected, actual } => {
                MirrorError::IntegrityMismatch { expected, actual }
            }
            BackendError::Store(e) => MirrorError::Store(e),
            BackendError::Io(e) => MirrorError::Io(e),
            BackendError::NotFound { hash } => MirrorError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("object {hash} not found on mirror"),
            )),
            BackendError::Other(msg) => {
                MirrorError::Io(std::io::Error::other(msg))
            }
        }
    }
}

/// Out-of-band mirror of the local CAS onto an S3-compatible remote (R2 in
/// production). Holds no local filesystem state of its own.
pub struct BlobMirror {
    backend: ExternalObjectStoreBackend,
}

impl BlobMirror {
    pub fn new(inner: Arc<dyn ExternalObjectStore>) -> Self {
        Self { backend: ExternalObjectStoreBackend::new(inner) }
    }

    /// Builds a mirror against Cloudflare R2 from env vars: `R2_BUCKET`,
    /// `R2_ENDPOINT`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`. See
    /// docs/MANUAL-SETUP.md for the real bucket setup.
    pub fn from_r2_env() -> Result<Self, object_store::Error> {
        let bucket = std::env::var("R2_BUCKET").unwrap_or_default();
        let endpoint = std::env::var("R2_ENDPOINT").unwrap_or_default();
        let access_key = std::env::var("R2_ACCESS_KEY_ID").unwrap_or_default();
        let secret_key = std::env::var("R2_SECRET_ACCESS_KEY").unwrap_or_default();

        let s3 = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_endpoint(endpoint)
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key)
            .with_region("auto")
            .with_virtual_hosted_style_request(false)
            .build()?;

        Ok(Self::new(Arc::new(s3)))
    }

    /// Mirrors one object to the remote, keyed by its content hash.
    pub async fn mirror_object(&self, hash: &str, data: &[u8]) -> Result<(), MirrorError> {
        Ok(self.backend.put(hash, data).await?)
    }

    /// Pulls an object from the remote and verifies its BLAKE3 hash matches
    /// `hash` before returning it.
    pub async fn pull_object(&self, hash: &str) -> Result<Vec<u8>, MirrorError> {
        Ok(self.backend.get(hash).await?)
    }
}

/// Pull-on-demand: serves from the local CAS if present, otherwise fetches
/// from the remote mirror (with integrity verification), writes it into the
/// local CAS, and returns it.
pub async fn pull_on_demand(
    local: &ObjectStore,
    mirror: &BlobMirror,
    hash: &str,
) -> Result<Vec<u8>, MirrorError> {
    if let Ok(data) = local.read_object(hash) {
        return Ok(data);
    }
    let data = mirror.pull_object(hash).await?;
    local.write_object(hash, &data).map_err(MirrorError::Io)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;
    use object_store::local::LocalFileSystem;

    fn mirror_backed_by(dir: &std::path::Path) -> BlobMirror {
        let backend = LocalFileSystem::new_with_prefix(dir).unwrap();
        BlobMirror::new(Arc::new(backend))
    }

    #[tokio::test]
    async fn blobs_round_trip_via_the_mirror() {
        let remote_dir = tempfile::tempdir().unwrap();
        let mirror = mirror_backed_by(remote_dir.path());
        let data = b"mirrored content".to_vec();
        let hash = hash_bytes(&data);

        mirror.mirror_object(&hash, &data).await.unwrap();
        let pulled = mirror.pull_object(&hash).await.unwrap();

        assert_eq!(pulled, data);
    }

    #[tokio::test]
    async fn pull_object_rejects_content_that_does_not_match_the_requested_hash() {
        let remote_dir = tempfile::tempdir().unwrap();
        let mirror = mirror_backed_by(remote_dir.path());
        // Mirror tampered/corrupt bytes under a hash they don't match.
        mirror.mirror_object("not-the-real-hash", b"tampered bytes").await.unwrap();

        let result = mirror.pull_object("not-the-real-hash").await;

        assert!(matches!(result, Err(MirrorError::IntegrityMismatch { .. })));
    }

    #[tokio::test]
    async fn pull_on_demand_serves_from_local_without_touching_remote() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let local = ObjectStore::new(local_dir.path());
        let mirror = mirror_backed_by(remote_dir.path());

        let data = b"already have this locally".to_vec();
        let hash = hash_bytes(&data);
        local.write_object(&hash, &data).unwrap();
        // Deliberately never mirrored - if pull_on_demand reached the
        // remote, this would fail with a not-found error instead.

        let result = pull_on_demand(&local, &mirror, &hash).await.unwrap();

        assert_eq!(result, data);
    }

    #[tokio::test]
    async fn pull_on_demand_falls_back_to_remote_and_populates_local_cas() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let local = ObjectStore::new(local_dir.path());
        let mirror = mirror_backed_by(remote_dir.path());

        let data = b"only exists on the remote".to_vec();
        let hash = hash_bytes(&data);
        mirror.mirror_object(&hash, &data).await.unwrap();
        assert!(!local.has_object(&hash));

        let result = pull_on_demand(&local, &mirror, &hash).await.unwrap();

        assert_eq!(result, data);
        assert!(local.has_object(&hash));
    }
}
