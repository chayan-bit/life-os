# lifeos-vcs - universal version control for every file type

> The headline differentiator: git-style history, branches, tags, diffs, and time-travel over **videos, images, design files, 3D scenes, audio, PSDs, docs - anything** - not just code.
> Nobody else has this. It is a Rust service, and it composes natively with the append-only `events` log (a `version.created` event *is* a commit).

---

## 1. Why it exists

- Notion/Drive/Figma each have weak, siloed, type-specific version history you cannot diff, branch, or reason about across domains.
- Git + Git-LFS handle code and "large files" but give no *semantic* diff for binary/media and no cross-domain snapshot.
- Life OS already has the perfect substrate: `entities` (the tree of files), `edges` (links), `events` (append-only history). `lifeos-vcs` turns these into a real VCS for everything.

---

## 2. Core model

### 2.1 Content-addressed store (CAS)
- Every file blob is hashed with **BLAKE3** (Rust, fast, parallel) → `blob_ref` = the hash.
- Identical content is stored once (dedup). The bytes live in `objects/<hh>/<hash>` locally, mirrored to **Cloudflare R2/S3** out-of-band (NOT through libSQL - blobs never go in the DB or the replica sync).
- File-bearing entities store only the `blob_ref` (see [DATA-MODEL.md](./DATA-MODEL.md) §2.1) - LFS-style pointer model.

### 2.2 Big-file dedup via content-defined chunking
- Large/re-exported media (a 2GB video re-rendered) would naively re-store the whole file.
- **FastCDC** (Rust crate) splits blobs into content-defined chunks; only changed chunks are stored. A re-export with a trimmed intro re-stores ~one chunk, not 2GB.
- Each blob = a manifest of chunk hashes (a Merkle list); the blob hash is the hash of that manifest.

### 2.3 History = the events log
- Each change to a file-bearing entity emits `version.created` with `{entity_id, blob_ref, parent_blob_ref, author, message, ts}`.
- The version history of any file is therefore a query over `events` - **no separate history table.**
- Because `events` is append-only and conflict-free, version history survives sync without merge hazards.

### 2.4 Snapshots, branches, tags (git-style over EVERYTHING)
- A **snapshot** = a content-addressed Merkle manifest of `{entity_id → blob_ref}` across the whole workspace (or a filtered subset) at a point in time.
- A **branch** = a named, moving pointer to a snapshot; a **tag** = a fixed pointer.
- This gives **time-travel over your entire life**, not one repo: "show me everything as it was 3 weeks ago", "branch my design system, try a variant, merge or discard."
- Snapshot/branch/merge logic is **modeled on Jujutsu (jj)**'s concepts (Rust, git-compatible, superior large-file & conflict model) but **implemented from scratch** in `lifeos-vcs` - a `BTreeMap`-backed `SnapshotManifest` (`src/snapshot.rs`) and the crate's own `vcs_refs` table (migration `0005_vcs_refs.sql`), not a fork of `jj-lib` and not a dependency on it. `external/jj` is checked out as a submodule for reference only (reading jj's model while implementing), and no `Cargo.toml` in this repo depends on `jj-lib`.

---

## 3. Per-type semantic diff

A raw byte diff is useless for media. Each module declares a `diff(a, b)` function per entity type in its manifest; `lifeos-vcs` orchestrates it and asks Haiku to write a plain-English "what changed" summary.

| Type | Diff strategy |
|---|---|
| text / code / markdown | line diff (existing) |
| Godot `.tscn` / `.tres` | **text diff** (they are text!) - first-class scene history for free |
| image (png/jpg) | perceptual hash + pixel/region diff + thumbnail-overlay |
| video | scene/keyframe diff (changed segments + timestamps) |
| audio | waveform / transcript diff (via lifeos-ingest) |
| Figma | node-tree diff (added/removed/changed nodes) via mcp-figma |
| 3D (glb/fbx/Unity/Unreal) | metadata + chunk-level diff; node graph where text-exportable |
| PDF / docx | extracted-text diff + page-image diff |

The `diff` plugin contract:
```js
// in modules/<id>/module.js
diff: {
  asset: async (aBlob, bBlob, ctx) => ({
    kind: 'video',
    changedSegments: [{ t_start, t_end, note }],
    summary: '<one-line, AI-written>',
    preview: '<blob_ref to a generated diff thumbnail/overlay>'
  })
}
```

**Implemented (issue #85):** `strategy_for`/`diff_blobs` (`services/lifeos-vcs/src/diff.rs`) is the dispatch registry that routes an entity type to a diff strategy. Only **text / code / markdown / Godot `.tscn` / `.tres`** are real, working diff pipelines - `diff_text` uses the battle-tested `similar` crate (Myers diff) for a genuine line-level diff, exactly the "first-class scene history for free" the table above promises for Godot scenes. Every other row - image, video, audio, Figma, 3D, PDF/docx - routes to `DiffStrategy::Unsupported` and `diff_blobs` returns `DiffError::UnsupportedKind` naming the specific blocking issue (`lifeos-ingest` transcription #89, image captioning / text extraction #90, or `mcp-figma` wiring) rather than a silent no-op or a fake diff. PDF/docx are the one row expected to fall out for free once #90 lands: extracted text is just text, so it reuses this same `Text` strategy rather than needing its own pipeline. The Haiku-written plain-English summary stays an orchestration concern in `server/` (same Agent SDK call pattern as `scaffold.js`, issue #72); `TextDiffResult::summary()` here is the deterministic line-count summary that call would be built from, not a replacement for it - **not yet wired up**, since the JS orchestration side and the module `diff` plugin contract itself are unbuilt.

Acceptance criterion "each file type produces a meaningful diff + summary" is therefore only met for the text-backed row today; the rest is correctly, honestly blocked rather than faked. Issue left open pending #88-90/mcp-figma.

---

## 4. The `lifeos-vcs` service (Rust)

Responsibilities:
1. **Hash & store** blobs (BLAKE3 + FastCDC chunking), dedup, mirror to R2/S3.
2. **Commit** - emit `version.created` events; update entity `blob_ref`.
3. **Diff** - dispatch to the per-type plugin; cache results.
4. **Snapshot/branch/tag/checkout** - Merkle manifests; restore any entity or the whole workspace to a past state.
5. **GC** - drop unreferenced chunks (mark-and-sweep against live snapshots).

Why Rust: tight hashing/IO loops, large-file throughput, memory safety on a security-relevant store, and native composition with libSQL (also Rust). Crates actually used: `blake3`, `fastcdc`, `object_store` (R2/S3), `libsql`. **Not** `jj-lib` - see §2.4: the snapshot/branch/tag model is modeled on Jujutsu's concepts but implemented from scratch, with no dependency on the `jj-lib` crate.

CLI surface (thin, allow-listed): `lifeos file commit|diff|log|checkout|snapshot|branch|tag`.

**Implemented (issue #87):** the rest of the HTTP surface `TimeTravel.jsx` needs - `GET /api/vcs/diff`, `GET /api/vcs/refs`, `POST /api/vcs/branch`, `POST /api/vcs/tag`, `GET /api/vcs/snapshot` (`services/lifeos-api/src/routes/vcs.rs`). `diff` maps a committed file's name to the coarse type `strategy_for` (#85) dispatches on and calls `diff_blobs` directly - real line-level diff for text-backed types, an honest `{"supported": false, "kind", "blocked_by"}` for everything else, never a fake diff. `refs`/`branch`/`tag`/`snapshot` expose #84's library primitives read/forward-only: `branch`/`tag` always snapshot the workspace's *current* state and point the ref at it (tags 400 if they'd have to move); there is still no route anywhere that force-moves a branch backward, rewrites a tag, or deletes a ref - matching docs/AGENT-CONTROL.md §1 by construction. On the frontend, `TimeTravel.jsx` now has a "Files (lifeos-vcs)" section wired to this real surface (list committed file entities, commit a new version, view timeline + real diffs) alongside a "Snapshots" section (create/list branches+tags, inspect a snapshot's full `entity_id -> blob_ref` manifest - "show me everything as it was 3 weeks ago"). The pre-existing localStorage app-settings checkpoint UI is kept as a separate, clearly-labeled section - it versions browser-only app state, a different concern from file content versioning.

**Implemented (issue #86):** `commit`/`history`/`checkout` end-to-end - `services/lifeos-api/src/routes/vcs.rs` (`POST /api/vcs/commit`, `GET /api/vcs/history`, `GET /api/vcs/checkout`) is the first caller to actually persist bytes through the real CAS + commit model (#81/#82) rather than the pre-existing `/api/files/commit` (#58) stopgap, which only ever hashed content without storing it. `lifeos file commit --path <file> [--entity-id] [--message]` reads the local file and uploads its bytes (base64) rather than asking the API to trust a client-supplied filesystem path; `lifeos file history --entity-id <id>` and `lifeos file checkout --entity-id <id> [--blob-ref <hash>] --out <path>` round out the loop - checkout with no `--blob-ref` retrieves the entity's current version, a specific one otherwise. `diff`/`snapshot`/`branch`/`tag` CLI subcommands aren't wired yet (the underlying `lifeos-vcs` primitives from #84/#85 exist; only their CLI/HTTP exposure is still open). No update/delete/rewrite route exists anywhere on this surface - matches docs/AGENT-CONTROL.md §1 by construction, not by convention.

**Implemented (issue #81):** the CAS object store itself — `services/lifeos-vcs`. `ObjectStore` (`src/store.rs`) lays blobs out under `objects/<hh>/<hash>` local to a configurable root (mirroring to R2/S3 is a later issue), and `write_object` is a no-op when the hash already exists — dedup falls directly out of content addressing rather than an explicit check. `store_blob`/`read_blob` (`src/blob.rs`) implement §2.2's manifest model exactly: `chunk_reader` (`src/chunk.rs`) splits the input via FastCDC, each chunk is written as its own object (so chunk-level dedup works across different blobs that happen to share content-defined chunks), the ordered list of chunk hashes is serialized as a `BlobManifest`, and the blob's `blob_ref` is the hash of that manifest JSON — not the hash of the raw bytes.

**Implemented (issue #82):** the commit model, per §2.3 exactly — `commit_version` (`src/commit.rs`) is the "a commit is a `version.created` event" primitive: it updates the entity's `blob_ref` and appends a `version.created` event (`entity_id, blob_ref, parent_blob_ref, author, message`) in one call, mirroring `lifeos-drain`'s standalone-connection + self-contained `emit_event` convention (this crate holds no `lifeos-api` dependency, same local-embedded-replica DB file). `history` is a plain oldest-first query over `events` for that entity — no separate history table, so it survives sync the same way the rest of the append-only log does. "Checkout an old version" composes directly: pull an old entry's `blob_ref` out of `history`, hand it to `read_blob` (issue #81) to get the bytes back — no separate checkout primitive needed since retrieval-by-hash already is checkout. No diff/snapshot/branch/GC yet; those are separate issues (#83+) layered on top.

---

## 5. Interaction with sync & gating

- **Blobs sync out-of-band** to R2/S3, keyed by hash - never through the libSQL replica (keeps the DB small and conflict-free).
- **Version history (`events`) syncs** as normal append-only rows.
- **Reads/diffs/time-travel are free** (internal, reversible).
- **Outward sharing** (publishing a versioned file, `drive.share`) is **gated**.
- Pairs with [MEDIA-INTELLIGENCE.md](./MEDIA-INTELLIGENCE.md): on `version.created`, `lifeos-ingest` (re)derives transcripts/captions so search stays current per version.

**Implemented (issue #84):** snapshot/branch/tag + GC (`services/lifeos-vcs/src/snapshot.rs`, `src/gc.rs`). `create_snapshot` captures every entity's current `blob_ref` in a workspace into a `SnapshotManifest` (`entity_id -> blob_ref`, `BTreeMap` for deterministic ordering), stores it as just another CAS object, and returns its content address - identical workspace state always re-hashes to the same `snapshot_ref`. `vcs_refs` (`migrations/0005_vcs_refs.sql`) holds branch/tag pointers: `set_branch` moves freely; `set_tag` refuses to move once set to a different snapshot (fixed pointer). `mark_and_sweep` computes the live set as every entity's *current* `blob_ref` (not just snapshotted state - committing a blob and GC'ing before ever snapshotting must not destroy it) plus everything reachable from every branch/tag's snapshot manifest, then deletes any on-disk object outside that set. Per docs/AGENT-CONTROL.md §1, VCS internals (branch-force/rewrite/GC) are a hard-denied domain for the agent - these are library primitives for a human/CLI caller (#86), never wrapped as an agent tool or run automatically on any commit path.

**Implemented (issue #83):** `BlobMirror` (`services/lifeos-vcs/src/mirror.rs`) - out-of-band mirroring onto an S3-compatible remote via the `object_store` crate (R2 is S3-compatible, so `AmazonS3Builder` pointed at an R2 endpoint is the real path; `BlobMirror::from_r2_env()` reads `R2_BUCKET`/`R2_ENDPOINT`/`R2_ACCESS_KEY_ID`/`R2_SECRET_ACCESS_KEY`, see docs/MANUAL-SETUP.md). `pull_object` verifies the BLAKE3 hash of whatever bytes come back before accepting them - the remote is never trusted blindly. `pull_on_demand` serves from the local CAS if present, otherwise pulls from the mirror, verifies, and populates the local CAS. Tests stand a `LocalFileSystem` `object_store` backend in for the remote so the mirror/pull/integrity logic runs with no network or real credentials in CI; the real R2 round-trip is a manual check (docs/MANUAL-SETUP.md #83), same boundary as the other owned-credential connectors.
