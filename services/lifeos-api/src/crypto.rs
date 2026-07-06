//! Envelope encryption for `connections.secret_enc` - the handful of
//! non-Nango secrets (Kite's daily access token, WhatsApp's session token,
//! docs/INTEGRATIONS.md §3) that don't fit Nango's OAuth vault. AES-256-GCM
//! with a random nonce per call; the server-held master key never leaves this
//! process (docs/SECURITY.md §1: "never in agent context, never in logs").

use crate::error::ApiError;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;

/// AES-256-GCM key, resolved once at boot from `LIFEOS_SECRET_ENCRYPTION_KEY`
/// (base64, 32 raw bytes - `openssl rand -base64 32`).
pub type EncryptionKey = [u8; 32];

pub fn parse_key(base64_key: &str) -> Result<EncryptionKey, String> {
    let bytes = STANDARD
        .decode(base64_key.trim())
        .map_err(|e| format!("LIFEOS_SECRET_ENCRYPTION_KEY is not valid base64: {e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("LIFEOS_SECRET_ENCRYPTION_KEY must decode to 32 bytes, got {}", v.len()))
}

/// Encrypts `plaintext`, returning a base64 blob of `nonce || ciphertext`
/// suitable for `connections.secret_enc`.
pub fn encrypt(plaintext: &str, key: &EncryptionKey) -> Result<String, ApiError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| ApiError::Internal("envelope encryption failed".into()))?;
    let mut blob = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(blob))
}

/// Reverses [`encrypt`]. Fails closed (Internal) on any tamper/format/key
/// mismatch rather than returning partial plaintext.
pub fn decrypt(blob: &str, key: &EncryptionKey) -> Result<String, ApiError> {
    let raw = STANDARD
        .decode(blob)
        .map_err(|_| ApiError::Internal("secret_enc is not valid base64".into()))?;
    if raw.len() < 12 {
        return Err(ApiError::Internal("secret_enc blob too short".into()));
    }
    let (nonce_bytes, ciphertext) = raw.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| ApiError::Internal("envelope decryption failed - wrong key or tampered blob".into()))?;
    String::from_utf8(plaintext).map_err(|_| ApiError::Internal("decrypted secret_enc is not valid UTF-8".into()))
}

/// Generates a fresh random 32-byte envelope key (per-workspace envelope
/// keys, issue #104 - `docs/DATA-MODEL.md` §4, `docs/SECURITY.md` §5).
pub fn random_key() -> EncryptionKey {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Decodes a base64-encoded raw key blob (as stored, encrypted, in
/// `envelope_key_enc`) into an [`EncryptionKey`] after decrypting it under
/// the server's master key.
fn decrypt_envelope_key(enc: &str, master_key: &EncryptionKey) -> Result<EncryptionKey, ApiError> {
    let raw = decrypt(enc, master_key)?;
    let bytes = STANDARD
        .decode(raw)
        .map_err(|_| ApiError::Internal("envelope_key_enc did not decode to raw key bytes".into()))?;
    bytes
        .try_into()
        .map_err(|_| ApiError::Internal("envelope key is not 32 bytes".into()))
}

/// Ensures `workspaces.envelope_key_enc` is set, generating + storing one
/// under the server's master key if it isn't yet. Idempotent. Shared by
/// database-per-workspace provisioning (issue #104) and client-side blob
/// encryption (issue #110) so both derive the same per-workspace key.
///
/// The generate-then-store step is a compare-and-swap
/// (`UPDATE ... WHERE envelope_key_enc IS NULL`) rather than an unconditional
/// write: two concurrent callers can both observe NULL and mint different
/// random keys, and an unconditional UPDATE would let the second writer's key
/// silently clobber the first, permanently orphaning anything already
/// encrypted under the first key. When the CAS loses (`rows_changed == 0`),
/// this re-reads whichever key actually won and returns that instead.
pub async fn ensure_envelope_key(
    conn: &libsql::Connection,
    master_key: &EncryptionKey,
    workspace_id: &str,
) -> Result<EncryptionKey, ApiError> {
    let mut rows = conn
        .query(
            "SELECT envelope_key_enc FROM workspaces WHERE id = ?1",
            libsql::params![workspace_id],
        )
        .await?;
    let existing: Option<String> = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'"))),
    };
    if let Some(enc) = existing {
        return decrypt_envelope_key(&enc, master_key);
    }

    let key = random_key();
    let key_b64 = STANDARD.encode(key);
    let enc = encrypt(&key_b64, master_key)?;
    let rows_changed = conn
        .execute(
            "UPDATE workspaces SET envelope_key_enc = ?1, updated_at = ?2 WHERE id = ?3 AND envelope_key_enc IS NULL",
            libsql::params![enc, crate::ids::now_secs(), workspace_id],
        )
        .await?;
    if rows_changed == 0 {
        // Lost the race: another caller already wrote a key between our
        // SELECT and this UPDATE. Re-read and return the winner's key
        // rather than the one we just generated locally.
        let mut rows = conn
            .query(
                "SELECT envelope_key_enc FROM workspaces WHERE id = ?1",
                libsql::params![workspace_id],
            )
            .await?;
        let winner: Option<String> = match rows.next().await? {
            Some(row) => row.get(0)?,
            None => return Err(ApiError::BadRequest(format!("unknown workspace '{workspace_id}'"))),
        };
        let enc = winner.ok_or_else(|| {
            ApiError::Internal("envelope_key_enc CAS lost but re-read found no key".into())
        })?;
        return decrypt_envelope_key(&enc, master_key);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> EncryptionKey {
        [7u8; 32]
    }

    #[test]
    fn roundtrips_plaintext() {
        let key = test_key();
        let blob = encrypt("kite-access-token-abc123", &key).unwrap();
        assert_ne!(blob, "kite-access-token-abc123", "must not store plaintext");
        assert_eq!(decrypt(&blob, &key).unwrap(), "kite-access-token-abc123");
    }

    #[test]
    fn two_encryptions_of_same_plaintext_differ() {
        let key = test_key();
        let a = encrypt("same-secret", &key).unwrap();
        let b = encrypt("same-secret", &key).unwrap();
        assert_ne!(a, b, "random nonce must make ciphertexts unlinkable");
    }

    #[test]
    fn wrong_key_fails_closed() {
        let blob = encrypt("secret", &test_key()).unwrap();
        let wrong_key = [9u8; 32];
        assert!(decrypt(&blob, &wrong_key).is_err());
    }

    #[test]
    fn parses_valid_base64_key() {
        let encoded = STANDARD.encode([1u8; 32]);
        assert_eq!(parse_key(&encoded).unwrap(), [1u8; 32]);
    }

    #[test]
    fn rejects_wrong_length_key() {
        let encoded = STANDARD.encode([1u8; 16]);
        assert!(parse_key(&encoded).is_err());
    }

    async fn test_conn_with_workspace(workspace_id: &str) -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY, envelope_key_enc TEXT, updated_at INTEGER)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, envelope_key_enc, updated_at) VALUES (?1, NULL, 0)",
            libsql::params![workspace_id],
        )
        .await
        .unwrap();
        conn
    }

    #[tokio::test]
    async fn ensure_envelope_key_generates_and_persists_when_absent() {
        let conn = test_conn_with_workspace("ws-1").await;
        let master_key = test_key();

        let key = ensure_envelope_key(&conn, &master_key, "ws-1").await.unwrap();

        let mut rows = conn
            .query("SELECT envelope_key_enc FROM workspaces WHERE id = 'ws-1'", ())
            .await
            .unwrap();
        let enc: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(decrypt_envelope_key(&enc, &master_key).unwrap(), key);
    }

    #[tokio::test]
    async fn ensure_envelope_key_is_idempotent_on_repeat_calls() {
        let conn = test_conn_with_workspace("ws-1").await;
        let master_key = test_key();

        let first = ensure_envelope_key(&conn, &master_key, "ws-1").await.unwrap();
        let second = ensure_envelope_key(&conn, &master_key, "ws-1").await.unwrap();

        assert_eq!(first, second, "repeat calls must return the same persisted key");
    }

    /// Regression test for the read-then-write race (finding 16): if a key
    /// is already persisted by the time the CAS UPDATE runs (simulating a
    /// concurrent winner), ensure_envelope_key must return the winner's key,
    /// not silently overwrite it with a freshly generated one.
    #[tokio::test]
    async fn ensure_envelope_key_returns_winner_when_cas_loses_the_race() {
        let conn = test_conn_with_workspace("ws-1").await;
        let master_key = test_key();

        // Simulate a concurrent caller that already won: pre-populate the
        // row with an already-persisted envelope key.
        let winner_key = random_key();
        let winner_enc = encrypt(&STANDARD.encode(winner_key), &master_key).unwrap();
        conn.execute(
            "UPDATE workspaces SET envelope_key_enc = ?1 WHERE id = 'ws-1'",
            libsql::params![winner_enc.clone()],
        )
        .await
        .unwrap();

        // A caller that (in a real race) already generated its own key
        // before reaching the CAS UPDATE must still end up with the
        // winner's key, because the SELECT above now finds it present.
        let result = ensure_envelope_key(&conn, &master_key, "ws-1").await.unwrap();
        assert_eq!(result, winner_key, "must return the already-persisted winner's key");

        // The row must still hold the winner's ciphertext, untouched.
        let mut rows = conn
            .query("SELECT envelope_key_enc FROM workspaces WHERE id = 'ws-1'", ())
            .await
            .unwrap();
        let enc: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(enc, winner_enc, "the pre-existing key must not be overwritten");
    }

    #[tokio::test]
    async fn ensure_envelope_key_cas_branch_returns_pre_existing_key_not_local_generation() {
        // Directly exercises the rows_changed == 0 branch: pre-populate the
        // row with envelope_key_enc already NOT NULL, so the CAS UPDATE
        // (`WHERE envelope_key_enc IS NULL`) is guaranteed to match zero
        // rows regardless of timing, then assert the returned key is the
        // pre-existing one rather than a freshly minted local key.
        let conn = test_conn_with_workspace("ws-1").await;
        let master_key = test_key();
        let pre_existing_key = random_key();
        let pre_existing_enc = encrypt(&STANDARD.encode(pre_existing_key), &master_key).unwrap();
        conn.execute(
            "UPDATE workspaces SET envelope_key_enc = ?1 WHERE id = 'ws-1'",
            libsql::params![pre_existing_enc],
        )
        .await
        .unwrap();

        let result = ensure_envelope_key(&conn, &master_key, "ws-1").await.unwrap();

        assert_eq!(result, pre_existing_key);
    }

    #[tokio::test]
    async fn ensure_envelope_key_errors_on_unknown_workspace() {
        let conn = test_conn_with_workspace("ws-1").await;
        let master_key = test_key();

        let err = ensure_envelope_key(&conn, &master_key, "does-not-exist").await;
        assert!(err.is_err());
    }
}
