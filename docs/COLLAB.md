# Real-time co-editing - cr-sqlite vs Yjs (spike #152)

Decision doc for the multiplayer co-editing layer that lands **after** sharing is proven (#146 roles, #148 proposals, #150 live feed + presence).
This is a spike deliverable: it evaluates two CRDT approaches and ships an isolated, converging prototype under [`prototypes/collab/`](../prototypes/collab/) - **not** an integration into the live app.

## TL;DR (recommendation)

- **Long-form content bodies -> Yjs. Validated, adopt when co-editing bodies is needed.**
  Yjs is the mature, correct sequence CRDT for prose/rich text, it is libSQL-agnostic (opaque binary blobs), and its op-log model is a *perfect* fit for the append-only `events` invariant (store each Yjs update as an append-only event, materialize the current text in the un-synced derived DB).
- **Structured entity `attrs` -> cr-sqlite's column-LWW *semantics*, but implemented natively in `lifeos-api` first, not the cr-sqlite extension itself.**
  The prototype proves the merge rule converges with zero lost writes, and proves the outcome is *identical* to replaying the `events` log in causal order (the `reconcile.rs` machinery Life OS already has).
  Full cr-sqlite (the native loadable extension) collides with the Turso embedded-replica sync model, wants mutable merge metadata that fights the append-only ethos, and adds a single-vendor native dependency.
  So: adopt the *idea* (per-column LWW, tie-broken by `(col_version, site_id)`) as a thin layer over the event log for v1; reach for the actual cr-sqlite extension only if hand-rolled column-LWW ever becomes the bottleneck.

Net: the issue's hypothesis - *"cr-sqlite for entity attrs + Yjs for long-form content bodies"* - is **directionally correct on both halves**, and **refined on the attrs half**: use cr-sqlite's *model*, not (yet) its *engine*.

---

## 1. What we are deciding

Co-editing means two or more clients mutate the *same* shared entity concurrently, possibly offline, and must converge without lost writes.
Life OS entities have two very different kinds of editable content:

1. **Structured fields** in `entities.attrs` (a trade's `status`/`stop`/`priority`, a task's `due`, an email's `subject`) - whole-value fields where "the latest edit to this field wins" is the desired semantics.
2. **Long-form bodies** - a Learning topic write-up, a trade thesis, a design brief - prose where two people typing in different places must both keep their characters.

These need different CRDTs. A single mechanism forced onto both is wrong in one direction or the other (see §6).

## 2. The two candidates

**cr-sqlite** ([vlcn.io](https://vlcn.io/docs/cr-sqlite/intro)) is a run-time-loadable C extension that turns SQLite tables into CRDTs.
You call `crsql_as_crr('table')`; it installs triggers and metadata tables and exposes a `crsql_changes` virtual table.
Each column becomes a last-write-wins register carrying a per-cell Lamport `col_version`, a per-database `db_version`, and an origin `site_id`; rows are a causal-length set (CLSet) so deletes converge too.
Sync is a changeset exchange: a peer asks for `crsql_changes` rows whose `db_version` exceeds what it has already seen from each site (a version vector held in `crsql_tracked_peers`).

**Yjs** ([yjs/yjs](https://github.com/yjs/yjs)) is a pure-JS/WASM sequence CRDT (YATA) for shared documents.
A `Y.Doc` exposes shared `Y.Text`/`Y.Map`/`Y.Array` types; edits become compact binary *updates*; the doc is the reduction of an append-only stream of those updates, and `Y.encodeStateAsUpdate(doc, remoteStateVector)` yields exactly the ops a peer is missing.
It has no storage opinion at all - a doc's state is an opaque byte blob you persist anywhere.

---

## 3. Axis (a): libSQL / embedded-replica compatibility

**Yjs - fully compatible, because it never touches sync semantics.**
Yjs state is an opaque binary blob.
libSQL just stores and ships bytes; it neither knows nor cares that they are a CRDT.
The one rule: a Yjs blob must **not** be reconciled by libSQL's row-level last-push-wins sync (DATA-MODEL §4.2), because that would clobber one replica's whole blob.
The clean pattern (confirmed by the Yjs community and PowerSync's Postgres+Yjs work) is to store the *updates* append-only - one row/event per update - and reduce them into a `Y.Doc` on read; because updates are commutative, applying them in any order converges.
Append-only rows never conflict, so they sync through Turso perfectly.
Verdict: Yjs is compatible with the embedded-replica story with **zero** changes to the sync model.

**cr-sqlite - file/extension-compatible, but it wants to *own* sync, which collides with Turso embedded replicas.**
libSQL is a SQLite fork that keeps 100% file-format and API compatibility and supports C loadable extensions, so cr-sqlite can *load* in principle (the libSQL Rust client needs extension-loading explicitly enabled, and its stability here is a watch item).
The real problem is architectural: cr-sqlite has its **own** replication mechanism (per-column CRDT merge over `crsql_changes`), and Turso embedded replicas have their **own** (frame/WAL-level shipping from the primary, last-push-wins at row granularity).
You cannot run both on the same tables: Turso would ship raw row frames that overwrite cr-sqlite's carefully-merged clock metadata, and cr-sqlite's triggers assume they own change tracking.
So adopting cr-sqlite means, for its CRR tables, **replacing** the Turso sync transport with cr-sqlite's changeset exchange - running it on the Mac's *local* SQLite path (`Builder::new_local`, which `lifeos-api` already uses when Turso is unconfigured; see DATA-MODEL §4.1) and shipping changesets peer-to-peer over #150, **not** through Turso replication.
That is a genuine fork from today's "one canonical Turso primary" model, not a drop-in.
Also: the Cloudflare Worker bot talks to the primary over the remote HTTP API and cannot load a native extension, so it could never be a cr-sqlite peer - it would just write rows and let a cr-sqlite-aware node reconcile.
Verdict: cr-sqlite is *compatible with SQLite/libSQL the file format*, but *incompatible with the embedded-replica sync model* unless you carve its tables out of Turso sync entirely.

## 4. Axis (b): sync cost / transport

Both are delta-based, and the prototype measured it:

| | cr-sqlite (attrs) | Yjs (body) |
|---|---|---|
| Unit shipped | changed **cells** (`crsql_changes` rows) | missing **ops** (state-vector diff) |
| Prototype delta | 2 cells, ~235 bytes JSON per side | 25-36 bytes vs a 92-byte full doc |
| Pull model | ask peer for changes since a version vector | exchange state vectors, send the diff |
| Metadata cost | `col_version`/`db_version`/`site_id` per cell (tiny in cr-sqlite's binary form; my JSON overstates it) | tombstones retained in-doc; needs periodic GC/snapshot for long histories |

Both ride #150's channel unchanged: SSE (`GET /api/events/stream`) pushes new changesets/updates server->client, a small `POST`/WS carries client->server.
Because both CRDTs are idempotent (proven), #150's `last-event-id` at-least-once resume is safe - a duplicate delivery is a no-op.
Yjs's **Awareness** protocol (ephemeral per-client cursor/presence state) maps directly onto #150's `presence.ping` design, so if Yjs is in the stack, presence is close to free.
Verdict: sync cost is a wash and small for both; neither needs new transport infra beyond #150.

## 5. Axis (c): event-sourcing fit (the Life OS-specific axis)

Life OS invariants: `events` is append-only and is the reconciliation source of truth; derived state lives in a **separate, never-synced** DB; and (per the issue) *CRDT merges must emit events*.

**Yjs - excellent fit; the CRDT and the event log are the same thing.**
A Yjs update stream *is* an append-only, commutative event log by construction.
Store each update as an `events` row (`type: 'body.updated'`, payload = the base64 update) - append-only, conflict-free, syncs cleanly.
The materialized current text is *derived state* - fold the update-events into a `Y.Doc` inside `lifeos-derived.db`, rebuildable at any time, never in the synced `lifeos.db`.
Large update blobs go to the blob store (R2 via `lifeos-vcs`), not through libSQL.
This satisfies both invariants with no tension: append-only events in, derived materialization out.

**cr-sqlite - real tension, but reconcilable, and the prototype shows why.**
cr-sqlite deliberately stores *only current state + merge metadata*, **not** full history, and its clock tables are **mutated in place** - the opposite of append-only.
It does not natively emit domain events; you would add triggers/app-code to also append an `events` row per CRR write (which `lifeos-api` already does at its write sites, so it is additive but now you keep *two* change records: cr-sqlite's clock metadata and the events log).
The good news the prototype demonstrates: cr-sqlite's column-LWW outcome is fully **deterministic**, and it **equals replaying the union of both append-only event logs**, taking the winning snapshot per cell by `(col_version, site_id)`.
That is exactly what `services/lifeos-api/src/reconcile.rs::reconcile_entity` already does per row (replay `entity.updated` events in causal order via ULID+`ts`, take the last attrs snapshot).
So Life OS **already has a coarse CRDT for attrs**: single-writer discipline + events reconciliation, at *row* granularity.
The only gap cr-sqlite's model closes is **column** granularity - so two users editing *different fields* of one entity don't clobber each other the way row-level last-push does.
Verdict: Yjs is a native fit; cr-sqlite's *outcome* is reproducible from the event log (which is why we can adopt its model without its engine), but its *mechanism* (mutable metadata, no native events) fights the append-only ethos.

---

## 6. What the prototype proves

[`prototypes/collab/`](../prototypes/collab/) is a standalone runnable (see its README for exact commands and captured output):

- **`crr_attrs.js`** (zero-dep, pure Node) reproduces cr-sqlite's published merge rule and runs the exact acceptance scenario: a shared base `{status, notes, priority}` synced to two clients ("mac" heavy lane, "bot" light lane), both go offline, mac edits `notes`+`priority`, bot edits `status`+`notes`, then they reconnect and exchange incremental changesets.
  It asserts, and passes: both **converge** to identical attrs; the disjoint edits (`status`, `priority`) **both survive** (no lost writes); the one genuinely-concurrent edit (`notes`) resolves **deterministically and identically** on both sides by `(col_version, site_id)`; merge is **order-independent**; and event-log replay (the `reconcile.rs` analog) yields the **same** state as the CRDT merge.
- **`yjs_body.js`** uses the **real Yjs library** (not a re-implementation): two clients edit a body offline (one prepends, one appends), reconnect, exchange state-vector deltas, and converge with both edits intact at the character level.

This satisfies the spike's acceptance criterion (two clients concurrently edit one entity's attrs offline and converge on reconnect without lost writes) and additionally validates the Yjs half against the actual library.

## 7. Recommendation

**Validate the hypothesis with one refinement.**

1. **Yjs for long-form bodies - adopt (when body co-editing is actually needed).**
   Store Yjs updates as append-only `body.updated` events; materialize `Y.Doc`s in the derived DB; push/pull deltas over #150; use Awareness for cursors/presence.
   No change to libSQL sync; textbook event-sourcing fit.

2. **cr-sqlite's column-LWW *model* for structured attrs - adopt the semantics natively; defer the extension.**
   Implement per-column LWW (`col_version` Lamport clock + `site_id` tiebreak, exactly as the prototype shows) as a thin layer in `lifeos-api`, carried as per-field metadata inside `attrs` (or as `entity.attr.updated` events), reconciled by extending `reconcile.rs` from row-granular to column-granular.
   This is DATA-MODEL §4.2's option (3) "field-merge `conflictResolver`", formalized - it delivers cr-sqlite's convergence for concurrent multi-field editing, emits events natively (append-only, conflict-free through Turso sync), and adds **no** native dependency and **no** fork of the sync model.
   Only if hand-rolled column-LWW ever becomes a throughput bottleneck (many CRR tables, high write rate) should we reach for the actual cr-sqlite extension - and then run it on the Mac's local path with changesets over #150, accepting the carve-out from Turso sync.

**Why not just adopt cr-sqlite now:** native-extension loading in the libSQL Rust client (watch item), a sync mechanism that collides with Turso embedded replicas (§3), mutable metadata that fights the append-only invariant (§5), and a single-vendor native dependency whose roadmap we would have to track - all to buy a column-LWW outcome the event log can already reproduce (proven).
The cost/benefit only flips at scale we do not have yet.

## 8. How a real integration would ride #150 and respect the derived-DB rules

- **Transport = #150.** Co-edit deltas (Yjs updates, or column-LWW change events) flow over `GET /api/events/stream` (SSE) server->client and a small WS/`POST` client->server; `last-event-id` resume + CRDT idempotence give safe at-least-once delivery.
  Presence/cursors reuse #150's presence design (Yjs Awareness maps onto it directly).
- **events stays append-only.** CRDT merges **emit** events, never rewrite them; reconciliation from events remains the source of truth (the prototype proves events-replay == CRDT outcome).
- **Derived-DB rules hold.** Materialized co-edit state (a `Y.Doc`'s current text; any cached merged-attrs view) is derived and rebuildable -> it lives in `lifeos-derived.db`, never the synced `lifeos.db`.
  The synced canonical tables hold only the reduced final values (`entities.attrs`) plus the append-only update events.
  Large CRDT blobs sync out-of-band to R2/S3 via `lifeos-vcs`, never through libSQL.
- **Governance.** Co-editing is an *inward* collaborative edit within a workspace, so it is not human-gated like outward posts/sends; but merges still emit auditable events, and role-gating from #146 applies (a `viewer` cannot co-edit).

## 9. Risks / watch-items

- Turso offline writes / Sync are public beta, not GA (DATA-MODEL §4.1) - the whole co-edit transport story sits on top of that maturity.
- cr-sqlite is a single-vendor native extension with an active but small ecosystem; adopting the extension (not just the model) means tracking its roadmap and libSQL extension-loading stability.
- Yjs docs retain tombstones; long-lived bodies need periodic snapshot/GC to bound size.
- Column-LWW (native or cr-sqlite) is still *lossy per field* on a true same-field conflict - it keeps one edit deterministically, it does not merge two prose paragraphs; that is precisely why prose belongs in Yjs, not attrs.

## 10. Follow-ups (not done here, by design)

- **`docs/ARCHITECTURE.md` index update is deferred** to avoid colliding with concurrent workers: add a link to this doc under the systems/index section when the tree is quiet. (This spike intentionally touched only new files plus this doc.)
- A future implementation issue should cover: the column-granular extension to `reconcile.rs`, the `body.updated` Yjs-update event type + derived-DB materializer, and the #150 WS client->server leg.
