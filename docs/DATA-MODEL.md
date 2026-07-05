# Data Model

The entire system is one generic, multi-tenant entity-graph.
**New domains and fields need zero migration by default.**
This document specifies every table, the sync semantics (corrected against real libSQL behavior), and the search/recall layer.

---

## 1. Two planes

- **Data plane** - the generic graph every module writes into (`entities`, `edges`, `events`, `annotations`, `jobs`, `module_requests`).
- **Control plane** - SaaS-ready tenancy/identity/credentials (`workspaces`, `users`, `memberships`, `connections`, `sessions`, `workspace_databases`). No billing/quota tables by design (issue #104) - self-hosted, bring-your-own-database-and-AI-model, nothing to meter.

Both are in the canonical `lifeos.db` (Turso/libSQL).
Derived state (FTS5, vectors) is **not** here - it lives in a separate, never-synced `lifeos-derived.db` (see §5).

---

## 2. Data plane

### 2.1 `entities` - every typed node
```sql
CREATE TABLE entities (
  id          TEXT PRIMARY KEY,           -- ulid
  workspace_id TEXT NOT NULL,             -- tenant scope (always present)
  module      TEXT NOT NULL,              -- 'learning' | 'tasks' | 'email' | …
  type        TEXT NOT NULL,              -- 'topic' | 'task' | 'trade' | 'email' | 'asset' | …
  parent_id   TEXT,                       -- hierarchy (nullable)
  title       TEXT,                       -- denormalized display title
  status      TEXT,                       -- lifecycle state per module manifest
  tier        TEXT,                       -- optional ranking/priority bucket
  attrs       TEXT NOT NULL DEFAULT '{}', -- JSON escape hatch (the per-domain fields)
  source      TEXT,                       -- provenance ('telegram'|'gmail'|'agent'|…)
  blob_ref    TEXT,                       -- content hash into lifeos-vcs (for file-bearing entities)
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id)
);
CREATE INDEX ix_entities_ws_module_type ON entities(workspace_id, module, type);
CREATE INDEX ix_entities_parent ON entities(parent_id);
```
- `attrs` is the flexible JSON store: a trade's `entry/exit/stop/R`, an email's `from/subject`, a trip's `budget`. ~90% of new modules add fields here with **no DDL**.
- `blob_ref` ties any entity to a versioned file in `lifeos-vcs` (see [VERSIONING.md](./VERSIONING.md)).

### 2.2 `edges` - typed cross-domain links
```sql
CREATE TABLE edges (
  id          TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  src_id      TEXT NOT NULL,              -- entity
  dst_id      TEXT,                       -- entity (nullable for external refs)
  dst_ref     TEXT,                       -- external target (URL, notion_page_id, …)
  rel         TEXT NOT NULL,              -- 'connection'|'depends_on'|'blocks'|'derived_from'
                                          -- 'owns'|'publishes_to'|'uses_asset'|'thesis'|'same_as'|…
  state       TEXT DEFAULT 'accepted',    -- 'pending'|'accepted'
  created_by  TEXT,                       -- 'agent'|'user'|module id
  created_at  INTEGER NOT NULL
);
CREATE INDEX ix_edges_src ON edges(workspace_id, src_id);
CREATE INDEX ix_edges_dst ON edges(workspace_id, dst_id);
```
Generalizes the knowledge-atlas's `connections`.
One asset can `uses_asset`-link into a Godot scene, a marketing campaign, and a Figma frame simultaneously - one graph across every module.

### 2.3 `events` - append-only log (the commit history)
```sql
CREATE TABLE events (
  id          TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  ts          INTEGER NOT NULL,
  type        TEXT NOT NULL,              -- domain: 'study.review'|'task.completed'|'trade.closed'
                                          -- |'post.published'|'version.created'|'module.installed'…
  entity_id   TEXT,                       -- subject (nullable)
  actor       TEXT,                       -- 'user'|'bot'|'harness'|module
  attrs       TEXT DEFAULT '{}',          -- payload
  -- harness run-log columns (events doubles as the run log):
  run_id      TEXT, tier TEXT, model TEXT,
  tokens_in   INTEGER, tokens_out INTEGER, cost REAL, latency_ms INTEGER,
  error       TEXT, outcome TEXT, eval_score REAL, gated INTEGER DEFAULT 0
);
CREATE INDEX ix_events_ws_ts ON events(workspace_id, ts);
CREATE INDEX ix_events_type ON events(workspace_id, type);
```
- **Append-only.** No UPDATE/DELETE route exists in the API - even the RW token cannot rewrite history.
- Doubles as: the **domain log**, the **harness run-log** (Observe/Eval/Release read it), the **version history** (a `version.created` event IS a commit), and the **reconciliation source of truth** for sync conflicts (§4).
- Powers all dashboards/analytics with **no separate storage** (see [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md)).

### 2.4 `annotations` - reader notes
Generalizes the atlas's localStorage comment/link/question layer; per-entity notes/highlights/questions, workspace-scoped.

### 2.5 `jobs` - heavy-work queue (cloud→Mac)
```sql
CREATE TABLE jobs (
  id          TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  kind        TEXT NOT NULL,              -- 'ingest'|'pipeline'|'module_build'|'eval'|…
  payload     TEXT NOT NULL DEFAULT '{}',
  status      TEXT NOT NULL DEFAULT 'queued', -- queued|running|done|failed
  priority    INTEGER DEFAULT 0,
  run_after   INTEGER,                    -- delayed jobs
  claimed_by  TEXT, claimed_at INTEGER,   -- lease (for crash recovery)
  attempts    INTEGER DEFAULT 0,
  created_at  INTEGER NOT NULL
);
CREATE INDEX ix_jobs_claim ON jobs(status, priority DESC, created_at);
```
**Atomic claim** (single statement, safe across pollers; no queue library needed - graphile-worker/pg-boss are Postgres-only):
```sql
UPDATE jobs SET status='running', claimed_by=:worker, claimed_at=unixepoch()
WHERE id = (SELECT id FROM jobs
            WHERE status='queued' AND (run_after IS NULL OR run_after<=unixepoch())
            ORDER BY priority DESC, created_at ASC LIMIT 1)
RETURNING id, kind, payload;
```
A **reaper** returns `running` rows whose `claimed_at` is older than a timeout back to `queued` (handles a Mac crash mid-job).
Implemented in `lifeos-drain` (Rust). **Kept in Turso, not Cloudflare Queues** (whose free tier drops messages after 24h and would split the audit trail from `events`).

### 2.6 `module_requests` - self-extension queue
Same claim pattern, separate lifecycle (`requested → building → installed | failed`). Survives the Mac being off. Each install is a git commit. See [SELF-EXTENSION.md](./SELF-EXTENSION.md).

**Implemented (issue #76):** the lifecycle is `queued → building → installed | failed` in the
actual `status` column (the table's own `CREATE TABLE` comment in `migrations/0001_core.sql`
already documented this - `requested` above is the conceptual name for the initial state, the
row's `status` value is literally `'queued'`, matching `jobs`). Unlike `jobs`, `module_requests`
carries no `claimed_by`/`attempts` of its own - the atomicity comes from each transition being a
single `UPDATE ... WHERE id=?1 AND status='<expected>'` (same CAS discipline `complete_job`/
`fail_job` already use), guarding against a stale write clobbering a transition that already
happened, rather than duplicating a full lease. `services/lifeos-drain::{claim_module_request,
complete_module_request, fail_module_request}` implement the three transitions, each appending
the matching `events('module.building'|'module.installed'|'module.failed')` row only when the
transition actually applied. `GET /api/module-request/:id` exposes the current `status`/`error`
to the requester. See [SELF-EXTENSION.md](./SELF-EXTENSION.md)'s #76 note for what's real today
versus deferred to #78 (the live drain loop that actually calls these against a real build).

---

## 3. Control plane (SaaS-ready, single-row for personal use)

| Table | Purpose |
|---|---|
| `workspaces` | Tenant. Personal = one seeded row. `plan` is a free-text label only - never gated on (issue #104: no billing/quota system). `envelope_key_enc` (issue #104) is a per-workspace AES-256-GCM key, itself encrypted under the server's master key. |
| `users` | Identity. Personal = one row. |
| `memberships` | `user_id ↔ workspace_id` + role (owner/admin/member). |
| `connections` | Per-workspace, per-account integration credential **handles** - now primarily a **Nango `connectionId` + provider + account_handle + scopes + status**, plus an encrypted envelope for the few non-Nango secrets (Kite daily token, WhatsApp). OAuth tokens themselves live in Nango, never here in plaintext, never in agent context. Supports many accounts per provider. |
| `sessions` | Refresh-token rotation for real login (issue #100) - see `docs/SECURITY.md` §5. |
| `workspace_databases` | Database-per-workspace provisioning record (issue #104) - Turso db name/url + an auth token encrypted under that workspace's own `envelope_key_enc`, never the shared master key alone. |

```sql
CREATE TABLE connections (
  id            TEXT PRIMARY KEY,
  workspace_id  TEXT NOT NULL,
  provider      TEXT NOT NULL,            -- 'google'|'notion'|'slack'|'x'|'instagram'|'reddit'|'figma'|'kite'|…
  account_handle TEXT,                    -- which account (multi-account)
  nango_connection_id TEXT,               -- handle into Nango's vault (preferred path)
  secret_enc    TEXT,                     -- envelope-encrypted blob for non-Nango providers only
  scopes        TEXT, expires_at INTEGER,
  status        TEXT DEFAULT 'active',    -- active|expired|revoked
  created_at    INTEGER NOT NULL
);
```

**Tenancy strategy:** `workspace_id`-scoped everywhere; personal deployment = one shared DB; the API enforces workspace filtering (RLS-style).
SaaS scales via **Turso database-per-workspace** - the local API abstracts "which DB", so this is a deployment swap, not a code change.

---

## 4. Sync model (CORRECTED against real libSQL behavior)

> The original plan assumed two things libSQL does not provide. Both are fixed below. This is the most important correctness section in the spec.

### 4.1 Embedded replica writes go to the remote primary by default
You **must** set `offline: true` on the Mac client, or "offline" gets you reads only and writes need the network - breaking the local-first premise.
```js
import { createClient } from "@libsql/client";
const db = createClient({
  url: "file:lifeos.db",            // embedded replica on the Mac
  syncUrl: process.env.TURSO_URL,   // canonical cloud primary
  authToken: process.env.TURSO_TOKEN,
  syncInterval: 60,                 // periodic pull; or call db.sync()
  offline: true,                    // REQUIRED: local-first writes (Turso Sync, public beta)
});
```
Read-your-writes is guaranteed on the replica that issued the write, even before `sync()`.
**Maturity flag:** offline writes / Turso Sync are public beta, not GA - treat as a watch item.

### 4.2 Conflict resolution is last-push-wins, NOT last-writer-wins-on-`updated_at`
Turso's default is **whichever replica pushes last wins**, regardless of `updated_at`, at **row granularity over the whole `attrs` JSON blob**.
So if the Haiku bot updates `attrs.status` while the Mac edits `attrs.notes` on the same row, one silently overwrites the other.
**Mitigation (defense in depth):**
1. **Single-writer-per-row discipline** - the bot writes its lane (light/medium), the Mac writes its lane (heavy). This is an explicit invariant: a given `entities.attrs` blob should only be mutated by one tier in a given window. The tiering already pushes this way (bot = light/medium-cost mutations like status/notes capture, Mac = heavy-cost mutations like ingest/pipeline output) - it is a discipline enforced by where call sites live, not by a runtime lock, so a violation degrades to (2) rather than corrupting data.
2. **`events` (append-only, conflict-free) is the reconciliation source of truth** - on a detected row conflict, repair entity state from the event log rather than trusting the blind last-push outcome. Implemented in `services/lifeos-api/src/reconcile.rs::reconcile_entity` (#12): every `entity.created`/`entity.updated` event now snapshots the `attrs` blob it applied, so reconciling an entity means replaying its events in causal order (ULID-prefixed `id`, time-sortable, with `ts` as tiebreak) and taking the last attrs-bearing snapshot as the intended final state - this is correct even when the last-push-wins sync outcome landed events out of causal order. Reachable via `POST /api/entity/:id/reconcile`; also acknowledged as the `reconcile` `jobs` kind in `lifeos-drain` for queued/batch repair (stub today, same as the other job kinds pending later-phase wiring).
3. **Optional field-merge `conflictResolver`** - carry per-field `updated_at` inside `attrs` and merge keys - add only when both tiers genuinely mutate the same rows.

Adopt (1)+(2) for v1 (correct for a single active user); add (3) later.

### 4.3 What syncs vs what does not
- `events`/`jobs`/`module_requests` - append-only → conflict-free.
- `entities`/`edges` - single-writer discipline + events reconciliation.
- **Derived state (FTS5, vectors) - never syncs** (§5).
- **Blobs (`lifeos-vcs` objects) - synced out-of-band to R2/S3**, not through libSQL (see [VERSIONING.md](./VERSIONING.md)).

---

## 5. Derived state lives in a SEPARATE, never-synced DB (CORRECTED)

> The original plan said derived tables are "local-only and don't sync." **libSQL has no table-level sync-exclusion flag** - a synced replica converges to the primary's schema, so a local-only table in the synced file would be clobbered. FTS5 over the embedded client also has an open panic bug (libsql#1811).

**Fix (cleaner, and already how `memvec.py` works):**
```sql
ATTACH DATABASE 'file:lifeos-derived.db' AS d;  -- plain SQLite; FTS5 + sqlite-vec live here
```
- `lifeos.db` (synced replica) holds **only** canonical tables.
- `lifeos-derived.db` (no `syncUrl`) holds `entities_fts` (FTS5) + `entity_vec` (sqlite-vec) and is **rebuilt locally**.
- Physical separation enforces "never synced" - derived state cannot conflict or be clobbered because it lives outside the synced file.

---

## 6. Search & recall

- **Lexical:** `entities_fts` (FTS5; triggers flatten `attrs` → `attrs_text`).
- **Semantic:** `entity_vec` reusing `~/.claude/bin/memvec.py` (MiniLM-384, sqlite-vec `vec0`). **Text-only** - see [MEDIA-INTELLIGENCE.md](./MEDIA-INTELLIGENCE.md) for how media becomes searchable.
- **Fusion:** FTS5 + vectors via **RRF**, reusing `~/.claude/bin/memory-recall`. No first-party Turso RRF helper exists; reusing memory-recall is the right call.
- **Keep sqlite-vec/memvec; do NOT switch to libSQL native vectors** - native vectors are beta with the weakest local path and would only help if vectors lived in the synced DB (which violates §5).
- The shared canonical DB **is** the cross-tier memory the Haiku bot recalls from.

**Implemented (issue #69):** the Mac-side hybrid described above is `services/lifeos-api/
src/routes/search.rs` (`GET /api/search`), already built - FTS5 lexical + memvec.py
semantic, fused with RRF, workspace-scoped. The Worker bot's `/recall <query>`
(`worker/src/recall.ts`) is **not** that endpoint: `lifeos-derived.db` is un-synced/Mac-local
and the Mac API only binds 127.0.0.1, so a Cloudflare Worker can reach neither while the
laptop is off. `/recall` is instead a lexical-only fallback - a case-insensitive `LIKE` over
`title`/`attrs` in the canonical Turso DB - trading recall quality for the "works with the
laptop off" guarantee every other bot command has. Citing the matched entity (short id +
module/type/title) is the one property both paths share.

---

## 7. No-migration growth

Hot query paths get **additive `GENERATED … VIRTUAL` columns** over `attrs`:
```sql
ALTER TABLE entities ADD COLUMN due INTEGER
  GENERATED ALWAYS AS (json_extract(attrs,'$.due')) VIRTUAL;
CREATE INDEX ix_entities_due ON entities(workspace_id, due);
```
- Confirmed: only VIRTUAL columns can be added via `ALTER TABLE ADD COLUMN` **without a table rewrite**; they are **indexable** (expression index); `json_extract` is deterministic/scalar/same-row → valid generated-column expression.
- The generated column becomes part of the canonical schema and **does** sync to the primary - correct and harmless.
- ~90% of new modules ship with no SQL at all; the remaining ~10% add one such column.

**Implemented (issue #136) - Self-Extension T4 generates exactly this shape.** `server/build/node.js`'s `TIER_PROMPTS.T4` instructs the agent to author ONE additive migration file (`migrations/<NNNN>_<name>.sql`) limited to the shapes above plus `CREATE INDEX IF NOT EXISTS` and `CREATE VIRTUAL TABLE IF NOT EXISTS ... fts5(...)`; `server/validators/t4Migration.js` proves it before commit: a statement classifier rejects anything outside those shapes (explicit destructive denylist wins first), a file-discipline check requires exactly one new file numbered current-max + 1, and a scratch-apply proof (real `sqlite3` CLI, applying every existing migration in numeric order first) snapshots `sqlite_master` + `PRAGMA table_info` before/after to assert no pre-existing column or non-table schema object changed - re-applying the migration a second time when every statement is idempotent to prove that claim too. **Honestly scoped:** `services/lifeos-derived` does not exist as a crate/DB in this repo - the derived DB's own DDL (`migrations/0003_derived.sql`, `0018_derived_memory.sql`) is baked into `lifeos-api/src/db.rs` as `include_str!` constants applied to a physically separate file, not read generically from `migrations/`, so T4's write-scope (`server/lib/tierScopes.js`) is narrowed to canonical additive migrations only; the validator rejects a migration that names a memvec-owned (`entity_vec`, `entity_vec_meta`, `llm_cache`) or lexical-index (`entities_idx`, `entities_fts`, `memory_idx`, `memory_fts`) table outright (those are rebuilt, never migrated), a static-text check rather than a live rebuild-convergence run. Also deferred, and equally honest: a landed T4 migration file is not yet wired into `lifeos-api/src/db.rs`'s `run_migrations()` (today's migration list is a hardcoded, ordered sequence of `include_str!` constants) - that wiring is a separate human-lane edit outside T4's generated write-scope, so a shipped T4 migration takes effect at runtime only once a human adds its constant + call, exactly like every prior migration in this repo's history.
