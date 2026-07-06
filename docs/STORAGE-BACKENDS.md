# Storage backends - bring-your-own blob storage

Today `lifeos-vcs` stores content-addressed blobs in `objects/<hh>/<hash>` locally and mirrors them to Cloudflare R2/S3 out-of-band (never through libSQL).
This doc generalizes that single hardcoded target into a **pluggable storage backend** layer: the user chooses where their blob bytes physically live - their own R2/S3 bucket, Google Drive, Dropbox, OneDrive, WebDAV, or local disk - and Life OS fetches the content back on demand and renders it.

This keeps the whole [VERSIONING.md](./VERSIONING.md) model intact (BLAKE3 content-addressing, FastCDC chunking, `version.created` commits, `blob_ref` pointers); only the *location of the bytes* becomes a per-workspace choice.
It applies to all blob-bearing data: VCS versions, file/folder content, repository-type data (`repo`/`file`/`folder` entities), and design/media assets.

> Invariant alignment: the canonical DB still holds only metadata + `blob_ref` pointers; **bytes never go through libSQL or the replica sync** ([DATA-MODEL.md](./DATA-MODEL.md) §4).
> Backend credentials live in **Nango / `connections.secret_enc`**, never in agent context ([SECURITY.md](./SECURITY.md) §1).

---

## 1. The principle - content-addressing decouples identity from location

A `blob_ref` is a **BLAKE3 content hash**, not a path.
That is exactly what makes storage pluggable: the hash identifies the bytes; a backend only has to answer "given this hash, where are the bytes and how do I get them?".

```
entity.blob_ref = b3:<hash>          ← stable identity (never changes)
        │
        ▼
   StorageBackend.locate(b3:<hash>)  ← per-workspace, user-chosen
        │
   ┌────┴──────────────────────────────────────────────┐
   r2/s3   google-drive   dropbox   onedrive   webdav   local-fs
```

Because identity is the hash, the user can **migrate or mirror** between backends without rewriting any entity, edge, event, or snapshot - the `blob_ref` is unchanged; only the backend mapping moves.

---

## 2. The `StorageBackend` abstraction (in `lifeos-vcs`)

A single Rust trait that every backend implements; `lifeos-vcs` depends only on the trait.

Implemented as `services/lifeos-vcs/src/backend.rs::StorageBackend` - all methods are `async`, the hash is a plain `&str` (not a `Blake3` type), and errors are the trait's own `BackendError`:

```rust
trait StorageBackend: Send + Sync {
    async fn put(&self, hash: &str, bytes: &[u8]) -> Result<(), BackendError>;   // idempotent (CAS)
    async fn fetch_unverified(&self, hash: &str) -> Result<Vec<u8>, BackendError>; // REQUIRED - no integrity check
    async fn get(&self, hash: &str) -> Result<Vec<u8>, BackendError> {           // PROVIDED - verifies BLAKE3 over fetch_unverified()
        verify_bytes(hash, self.fetch_unverified(hash).await?)
    }
    async fn has(&self, hash: &str) -> Result<bool, BackendError>;
    async fn delete(&self, hash: &str) -> Result<(), BackendError>;             // GC only; never agent-callable
    fn location(&self, hash: &str) -> String;                                   // backend-native path/id
}
```

- **`get` is a provided method over a required `fetch_unverified`** - the split exists so a wrapper like `EncryptedBackend` can decrypt the fetched bytes back into plaintext *before* the plaintext-hash check runs; every backend still gets the same non-negotiable BLAKE3 verification for free via the default `get` impl.
- **Integrity is non-negotiable:** every `get` re-hashes the fetched bytes with BLAKE3 and rejects a mismatch.
  An untrusted/remote backend cannot silently corrupt or swap content - content-addressing makes tampering detectable.
- **FastCDC chunking is backend-agnostic:** a blob is still a Merkle manifest of chunk hashes; chunks are `put`/`get` individually, so a backend that supports range/partial reads gets dedup + incremental sync for free.
- **`object_store` (Rust crate)** covers R2/S3/GCS/Azure/local in one implementation; Drive/Dropbox/OneDrive are thin per-provider impls over the Nango proxy; WebDAV is its own impl.
- **`local-fs`** remains the default (today's `objects/<hh>/<hash>`), so nothing regresses when no backend is configured.

---

## 3. Backends and how they authenticate

| Backend | Auth path | Notes |
| --- | --- | --- |
| **local-fs** | none | default; `objects/<hh>/<hash>` on the Mac |
| **R2 / S3 / GCS / Azure** | user's own keys in `connections.secret_enc` (envelope-encrypted) | one `object_store` impl |
| **Google Drive** | **Nango** (`google-drive`) - agent holds only a `connectionId` | bytes stored as Drive files under a Life OS app folder; `blob_ref` <-> Drive file id kept in a backend index |
| **Dropbox / OneDrive** | **Nango** | same pattern as Drive |
| **WebDAV / Nextcloud** | `connections.secret_enc` | self-hosted-friendly |

- **No backend ever exposes a token to the agent.** Owned-OAuth backends go through the Nango proxy (server-side injection); non-Nango keys are envelope-encrypted in `connections.secret_enc` and used only by `lifeos-vcs` on the Mac.
- **Optional client-side envelope encryption:** for semi-trusted consumer backends (Drive/Dropbox), the user may enable per-workspace encryption-at-rest so the provider stores ciphertext only.
  The hash is computed over plaintext (identity is stable); the backend stores the encrypted chunk.

---

## 4. Per-workspace configuration

Storage choice is workspace-scoped configuration, not code.

- A `storage_backend` config entity per workspace: `{ kind, connectionId? | secret_enc?, bucket/folder, encryption?, default:true }`.
- Multiple backends allowed: one **primary** (writes) plus optional **mirrors** (redundancy); reads fall back across them by hash.
- **Migration is a job:** "move my blobs from R2 to Drive" enqueues a `jobs` row that re-`put`s every live `blob_ref` onto the new backend and flips the primary pointer - no entity touched.
- Changing the backend is a **gated** action (it moves the user's data) and is part of the connections surface, which the in-app agent may **not** reconfigure (§6).

---

## 5. Fetch + render on the website (markdown-only by default)

Life OS pulls content back from whatever backend holds it and renders it in the SPA.

- **Fetch:** the frontend asks `lifeos-vcs` (via the API) for a `blob_ref`; the active `StorageBackend.get` returns verified bytes; a local content cache avoids repeat fetches.
- **Default rendering is Markdown only.**
  Markdown/text blobs render with the existing `frontend/src/components/MarkdownRenderer.jsx` (`marked` GFM + KaTeX).
  This is the deliberate, safe default: text/markdown content from any backend - including repository-type data such as `README.md`, notes, and docs - renders inline.
- **General (arbitrary) rendering is intentionally out of the default scope.**
  The platform does not try to natively render every MIME type.
  A user who wants richer rendering (a PDF viewer, syntax-highlighted code, a 3D preview) builds it through the **Agent Control Plane**: the agent can scaffold a custom view/module that fetches the same `blob_ref` and renders it however they like ([AGENT-CONTROL.md](./AGENT-CONTROL.md)).
  This keeps the core renderer minimal and trustworthy while leaving the ceiling unbounded for the user.
- **Non-markdown blobs** show a typed placeholder card (name, mime, size, version, download / open-in-backend link) until a custom view is added.

---

## 6. Security & boundaries

- **Bytes never traverse libSQL.** The DB holds metadata + `blob_ref` only; the replica never carries blob content.
- **Credentials never reach the agent.** Nango `connectionId` or `secret_enc`, injected server-side at call time.
- **The in-app agent cannot reconfigure storage or connections.** Storage-backend selection is part of the OAuth/connections protected domain ([AGENT-CONTROL.md](./AGENT-CONTROL.md) §1) - the agent can *read* content, *render* it, and *build custom views*, but cannot add/switch/credential a backend.
- **Integrity on every read** (BLAKE3 re-hash) defends against a compromised or buggy remote backend.
- **`delete` is GC/maintenance only** - never an agent tool, consistent with the VCS protected domain (agent may commit forward, never rewrite/GC).

---

## 7. Build surface & verification - fully implemented

This system is built, not future work. The pieces:

- **Migration:** `migrations/0014_blob_backends.sql` - the per-workspace `blob_backends` config table (§4).
- **`lifeos-vcs` (Rust):**
  - `src/backend.rs` - the `StorageBackend` trait (§2), `LocalFsBackend` (the default `objects/<hh>/<hash>` layout), and `ExternalObjectStoreBackend` covering R2/S3/GCS/Azure via the `object_store` crate, with a BLAKE3 re-hash on every `get`.
  - `src/backend_proxy.rs` - the Drive/Dropbox/OneDrive/WebDAV backends, riding the Nango proxy for the owned-OAuth providers.
  - `src/backend_index.rs` - the id<->hash mapping for backends whose native locator isn't the hash itself (a Drive file id, for example).
  - `src/migrate.rs` - the backend-migration job: `has`-before-`put` resumability (a resumed migration skips content already on the target), with the primary pointer flip happening only after a clean, complete run.
  - `src/encrypted.rs` - `EncryptedBackend`, the optional client-side envelope (AES-256-GCM) wrapping any other backend: content is hashed over plaintext (identity stable) and encrypted before `put`, decrypted in `fetch_unverified` before the trait's provided `get` runs its BLAKE3 check over the plaintext.
- **`lifeos-api`:** `src/storage.rs` is the `STORAGE_KINDS` factory that resolves a workspace's configured backend kind into the concrete `StorageBackend` impl; `routes/storage.rs` exposes the `storage_migrate` job trigger and enforces the gating from §4/§6 - backend `create` only ever writes a `pending_approval` config, and `migrate` requires an already-active target backend.
- **Frontend:** `frontend/src/components/MarkdownRenderer.jsx` (`marked` GFM + KaTeX) is the real markdown-only renderer described in §5; non-markdown blobs fall back to the typed placeholder.
- **Must-pass checks:**
  - Put a blob on backend X, switch primary to backend Y, read it back by the *same* `blob_ref` - bytes verify.
  - A corrupted/tampered remote blob fails the BLAKE3 check on `get`.
  - A markdown blob from a remote backend renders inline; a non-markdown blob shows the placeholder and is reachable by a user-built custom view.
  - The agent can render content but cannot add/switch a storage backend (refused, protected domain).
  - No blob bytes ever appear in the libSQL replica; no backend token ever in agent context or logs.
