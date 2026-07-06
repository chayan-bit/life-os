# Life OS - Architecture (master document)

This is the canonical, detailed architecture reference for Life OS.
`README.md` is the narrative superset; `CLAUDE.md` is the short working-rules companion; this `docs/` tree is the deep specification.
Read this file first, then the focused sub-documents it links.

> Status: implemented and tested through issue #140 - 9 Rust crates, a React SPA, a Cloudflare Worker, 19 migrations, roughly 1000 passing tests. Each section below is annotated inline with what is actually built vs. what remains target spec.
> Every claim here has been validated against how the underlying tools (Turso/libSQL, Claude Agent SDK, Nango, Cloudflare Workers, sqlite-vec) actually behave as of mid-2026; corrections to the original plan are called out inline and in the relevant sub-doc.

---

## 0. Document map

| Doc                                              | Covers                                                                                                                       |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- |
| **ARCHITECTURE.md** (this)                       | Vision, tiers, principles, the two-brains/one-DB model, global invariants, cross-cutting index                               |
| [DATA-MODEL.md](./DATA-MODEL.md)                 | Every table, DDL sketches, the data/control plane, sync model (corrected), search & recall, derived-DB split                 |
| [MODULES.md](./MODULES.md)                       | The module/plugin system + every seed and extended module in full (entity types, attrs, edges, events, views, tools, gating) |
| [INTEGRATIONS.md](./INTEGRATIONS.md)             | The **owned-OAuth** model that replaces claude.ai MCP connectors; Nango; the browser actuator; per-provider mechanics        |
| [VERSIONING.md](./VERSIONING.md)                 | `lifeos-vcs` - the universal content-addressed VCS for every file type (CAS, chunking, snapshots, per-type semantic diff)    |
| [MEDIA-INTELLIGENCE.md](./MEDIA-INTELLIGENCE.md) | `lifeos-ingest` - transcription/captioning/parsing → memvec; the honest memvec capability boundary; CLIP                     |
| [SELF-EXTENSION.md](./SELF-EXTENSION.md)         | Tier 0 of the ladder: the "Ask AI to add a module" **manifest** builder on the Claude Agent SDK; tool-locking; two validators; sandbox    |
| [SELF-EXTENSION-V2.md](./SELF-EXTENSION-V2.md)   | The **T0-T5 generation ladder**: manifest → renderer → agent tool → backend route → migration → whole subsystem, each sandboxed/validated/committed; the four never-generable surfaces |
| [AGENT-CORE.md](./AGENT-CORE.md)                 | The general **plan → execute → verify** tool-calling loop (`/api/agent`): Tool-RAG, `lifeos-memory` integration, self-evolution, tracing via `events`, minimal self-healing, the **two-layer LLM cache** (API-key mode), world-model snapshot + guardrails, corrective-RAG/abstention |
| [AGENT-CONTROL.md](./AGENT-CONTROL.md)           | Universal in-app agent actuation: typed action registry, the four protected domains, dry-run previews, action ledger + undo, capability matrix |
| [AI-MEMORY.md](./AI-MEMORY.md)                   | The cognitive memory architecture: event-sourced memory, episodic/semantic/procedural layers, activation-scored retrieval, consolidation, the context compiler, bi-temporal forgetting |
| [STORAGE-BACKENDS.md](./STORAGE-BACKENDS.md)     | Bring-your-own blob storage: pluggable `StorageBackend` (R2/S3/Drive/Dropbox/local) for VCS/repo/file data; fetch + markdown render; integrity + token isolation |
| [HARNESS-LOOP.md](./HARNESS-LOOP.md)             | Event store, Eval+Gate, Observe, Release loop; reuse of existing harness infra                                               |
| [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md)     | Command bar, agent pipelines, dashboards/analytics, module marketplace, PWA                                                  |
| [RUST-COMPONENTS.md](./RUST-COMPONENTS.md)       | Full inventory of what is/should be Rust, with crate choices and rationale                                                   |
| [SECURITY.md](./SECURITY.md)                     | The layered permission boundary, gating, secret isolation, sandbox, trading read-only guarantee                              |
| [BUILD-PLAN.md](./BUILD-PLAN.md)                 | Phased build order (revised for the adopted tools), per-phase verification                                                   |
| [LICENSING-REUSE.md](./LICENSING-REUSE.md)       | License audit (everything free?), the exists/fork/build classification, free fallbacks                                       |

---

## 1. What Life OS is

A **self-extending, agent-driven personal operating system** that unifies learning, tasks, coding/projects, trading, social, marketing, design - and anything added later (email, calendar, files, reading, travel, health, finance, …) - behind:

1. **One generic multi-tenant entity-graph database** (no per-domain tables).
2. **A declarative module/plugin system** - new domains are manifests, not schemas.
3. **A self-extension builder** - an in-app AI that writes its own new modules on request.
4. **A universal version-control engine** (`lifeos-vcs`) that versions _every_ file type (video, image, design, 3D, audio, docs) git-style, not just code.
5. **An owned-credential integration layer** (Nango + a browser actuator) that connects any service - API or not - under credentials you control.
6. **The local Claude Code harness as the heavy brain**, plus an always-on Telegram/Cloudflare lane as the light brain.

It is personal today, **architected to become a multi-tenant SaaS with no rewrite**.

### The unifying frame: "GitHub for your whole life"

GitHub is a versioned object store + a collaboration/issue layer + an automation engine (Actions).
Life OS already has the substrate for all three: `entities` (the tree), `edges` (links), `events` (append-only history = the commit log), `jobs` (the runners).
So Life OS does not bolt on a second system; it **turns the schema you already have into a universal VCS + Actions platform that operates on any file type and any domain.**

---

## 2. Design principles (global invariants - do not violate)

1. **One generic schema, specialized by declarative manifests.** Storage is generic; per-domain behavior is metadata. The opposite of Notion's per-database modeling tax. A task / trade / topic / post / campaign / asset / email / trip are all rows in `entities`.
2. **Multi-tenant from the first commit.** Every data row carries `workspace_id`. Personal use is one workspace. No single-user assumptions, no hardcoded ids.
3. **Local-first, no lock-in.** SQLite-compatible store (libSQL); offline on the Mac via embedded replica; data is yours and portable.
4. **Codegen only on the trusted Mac.** The always-on cloud surface can only _enqueue_; all file-writing and reasoning-heavy work happens locally, behind validators, as revertable git commits.
5. **Auditability over speed; gate the irreversible.** Append-only `events`; outward/irreversible actions (social posts, trades, sends, browser actions) are human-gated; every self-built module is one `git revert` away.
6. **Owned credentials only.** No integration depends on a third party's account (notably: **not** the claude.ai MCP connectors). All OAuth flows use developer apps you own, tokens encrypted at rest, injected at call time, never in agent context. See [INTEGRATIONS.md](./INTEGRATIONS.md).
7. **Minimum always-on context, token-disciplined.** API-first thin tools over heavy MCPs; on-demand loading only; bounded context injection.
8. **Reuse before build; fork when 80% fits.** Generalize the existing `knowledge-atlas` app and harness infra; adopt battle-tested OSS (Nango, grammY, Drizzle, Refine, Claude Agent SDK, whisper-rs, jj); fork an OSS repo and extend it rather than writing net-new.
9. **Rust where it is security- or throughput-critical.** The local API, the VCS, the ingest pipeline, the job drainer, the pipeline engine, the actions engine, the keyless agent-CLI router, the cognitive memory engine - all Rust. A future, narrowly-scoped broker guard is target spec, not built (§6, [SECURITY.md](./SECURITY.md) §1). The browser SPA, the Worker bot, the module manifests, the scaffolder stay JS/TS; memvec stays Python. See [RUST-COMPONENTS.md](./RUST-COMPONENTS.md).

---

## 3. Three tiers, two brains, one database

```
                         ┌─────────────────────────────┐
   Telegram  ──────────► │  Cloudflare Worker (free)    │   LIGHT/MEDIUM brain
   (laptop off-OK)       │  bot — Claude Haiku (grammY) │   full DB (workspace) +
                         │  full common-DB + memory +   │   memory + audit;
                         │  audit; enqueues heavy → jobs│   gated outward actions;
                         │  OAuth callbacks only        │   holds NO provider tokens
                         └──────────────┬──────────────┘
                                        │  HTTPS (workspace-scoped, authed)
                         ┌──────────────▼──────────────┐
                         │   Turso / libSQL  lifeos.db  │   CANONICAL, always-on
                         │ control-plane + data-plane   │   multi-tenant,
                         │ entities·edges·events·jobs…  │   SQLite-compatible (Rust engine)
                         └──────────────┬──────────────┘
                                        │  embedded replica (offline:true, periodic sync)
   ┌────────────────────────────────────▼────────────────────────────────┐
   │  MAC (when awake)                              HEAVY brain            │
   │  • lifeos local API (Rust, single DB-token owner, 127.0.0.1, authed)│
   │  • lifeos-vcs (Rust) — universal content-addressed versioning       │
   │  • lifeos-ingest (Rust) — transcribe/caption/parse → memvec         │
   │  • lifeos-pipelines (Rust/JS) — user/module agent DAGs via Agent SDK │
   │  • Life OS SPA (generalized knowledge-atlas + Refine views, light)  │
   │  • Claude Code harness: module scaffolder, Eval, Release, deep agents│
   │  • Nango (self-hosted) — OAuth vault + proxy for owned integrations  │
   │  • browser-use actuator — drive any website with no API (gated)     │
   │  • thin tools in ~/.claude/bin (allow-listed); broker-guard planned  │
   │  • lifeos-derived.db (un-synced): FTS5 + sqlite-vec                  │
   └──────────────────────────────────────────────────────────────────────┘
```

### 3.1 Light/medium brain (cloud, always-on)

A Telegram bot on Cloudflare Workers (scale-to-zero compute = free) built with **grammY**, running on **Claude Haiku**.
It has **full common-DB access scoped to its workspace** (entities/edges/events/annotations RW), **harness memory recall** (shared-DB FTS5/vector), and **audit logging** (`events`).
It handles capture, queries, and medium actions, **gates outward actions** for approval, and **enqueues** anything heavy/codegen for the Mac.
It uses `@libsql/client/web` (HTTP transport - Workers have no filesystem or raw sockets).
It **holds no provider tokens**: outward provider calls are deferred to the Mac/Nango after approval; the Worker's only integration duty is hosting OAuth callbacks (or Nango hosts them).
Works with the laptop off. Compute is free; Haiku tokens are minimal.

**Implemented (issue #63):** `worker/` is the Cloudflare Worker project - `src/bot.ts`
defines the grammY `Bot` (currently just `/start` and `/health`, tested offline via
grammY's documented `botInfo` + `api.config.use` interception pattern, no network needed in
CI), `src/index.ts` is the Worker `fetch` handler routing `POST /telegram` through
`webhookCallback(bot, "cloudflare-mod")` and `GET /` as a bare liveness check. No DB access,
capture/query commands, or approve/deny keyboards yet - those are #64-67. Deploying for
real (Cloudflare account + a live Telegram bot token from @BotFather) is a manual step; see
`docs/MANUAL-SETUP.md` #63.

**Implemented (issue #64):** `worker/src/db.ts` binds `@lifeos/db/client/worker`
(`@libsql/client/web`, HTTP-only transport) and resolves the acting workspace from
`env.WORKSPACE_ID`, falling back to the same `"default-personal-workspace"` id
`services/lifeos-api/src/config.rs::DEFAULT_WORKSPACE` uses - never from Telegram input, so
the bot can't be tricked into reading/writing another tenant's rows. `src/entities.ts` is
the workspace-scoped repository (`listEntities`/`createEntity`, every query
`WHERE workspace_id = ?`); `src/llm.ts` wraps `@anthropic-ai/sdk` for Haiku calls. Both are
tested against a real in-memory libSQL DB (`@lifeos/db/client/local`, sharing this
package's own `drizzle-orm` instance via `@lifeos/db/query` - two independently-installed
copies of `drizzle-orm` produce structurally incompatible branded types, so every
`@lifeos/db` consumer must import query builders from `@lifeos/db/query`, not
`"drizzle-orm"` directly) and a stubbed `fetch` respectively - `worker/test/entities.test.ts`
asserts cross-workspace isolation directly. Not yet wired into a bot command (no live
network call happens on a real Telegram message) - that lands in #65 alongside capture/query
commands; the real `TURSO_URL`/`TURSO_TOKEN`/`ANTHROPIC_API_KEY` secrets are a manual step,
`docs/MANUAL-SETUP.md` #64.

### 3.2 Heavy brain (Mac)

The existing Claude Code harness does deep work: study authoring, coding, trade analysis, the self-extension builder, integration-heavy design/marketing work, media ingestion, agent pipelines, and the Eval/Release loop.
It runs the Rust services (`lifeos` API, `lifeos-vcs`, `lifeos-ingest`), self-hosted Nango, and the browser actuator.
It owns the single DB write token and the embedded replica (`offline:true` for local-first writes).

### 3.3 Canonical DB (Turso/libSQL)

SQLite wire-compatible (all existing FTS5 / `memvec.py` code ports unchanged), hosted and always reachable, purpose-built for multi-tenant SaaS (cheap database-per-tenant when we scale).
The engine itself is **written in Rust**.
An **embedded replica** on the Mac preserves local-first/offline; the cloud copy is always awake for Telegram.
See [DATA-MODEL.md](./DATA-MODEL.md) for the corrected sync semantics (last-push-wins, `offline:true`, derived-state-in-a-separate-DB).

### Key invariant

Request/state is **data** in the synced DB (survives the Mac being off); **codegen + heavy reasoning only ever run on the trusted Mac**; the cloud surface stays trivial and token-free.

---

## 4. The adopted external stack (what we did NOT hand-build)

| Concern                     | Adopted                                                         | Doc                              |
| --------------------------- | --------------------------------------------------------------- | -------------------------------- |
| Canonical DB + offline      | Turso/libSQL (Rust engine), embedded replica                    | [DATA-MODEL.md](./DATA-MODEL.md) |
| Always-on compute           | Cloudflare Workers (free tier)                                  | [ARCHITECTURE.md]                |
| Telegram framework          | grammY (MIT, native Workers adapter)                            | [PLATFORM-SYSTEMS.md]            |
| DB access                   | Drizzle ORM + `@libsql/client`                                  | [DATA-MODEL.md]                  |
| Admin shell / generic views | Refine (MIT, backend-agnostic `dataProvider`)                   | [PLATFORM-SYSTEMS.md]            |
| Self-extension              | Claude Agent SDK (tool-lock, hooks, sandbox, structured output) | [SELF-EXTENSION.md]              |
| OAuth vault + proxy         | Nango (self-hosted, Elastic License 2.0)                        | [INTEGRATIONS.md]                |
| No-API automation           | browser-use / browser-harness (fork)                            | [INTEGRATIONS.md]                |
| Transcription               | whisper-rs / candle-whisper                                     | [MEDIA-INTELLIGENCE.md]          |
| Versioning primitives       | BLAKE3, FastCDC, Jujutsu (jj) model                             | [VERSIONING.md]                  |
| Semantic search             | sqlite-vec + MiniLM-384 (`memvec.py`)                           | [MEDIA-INTELLIGENCE.md]          |
| Graph view                  | Cytoscape                                                       | [PLATFORM-SYSTEMS.md]            |

Everything above is free for our use; the only source-available (not pure-OSS) item is Nango (ELv2), which is free to self-host for an internal vault. Full audit in [LICENSING-REUSE.md](./LICENSING-REUSE.md).

---

## 5. Global data flow (one request, end to end)

**Capture from phone, laptop off:**
Telegram message → Worker (grammY) → Haiku interprets → writes `entity`/`event` to Turso (scoped to workspace) → replies.
If heavy ("ingest this video", "add a module") → writes a `jobs` row → replies "queued".

**Implemented (issue #67):** `worker/src/commands.ts` - `/addmodule <prompt>` writes a
`module_requests` row (`status='queued'`, `worker/src/moduleRequests.ts`), matching the
dedicated table this repo already uses for the self-extension queue (`db/schema.ts`'s
`moduleRequests`, mirrored from `migrations/0001_core.sql`), and `/ingest <text>` writes a
generic `jobs(kind='ingest')` row (`worker/src/jobs.ts::enqueueJob`, the same helper #66's
`execute_approval` jobs use). Both reply "queued" immediately and touch no filesystem -
there isn't one on a Cloudflare Worker, so "the bot never writes code/files" holds
structurally, not just by convention. Real dispatch is now built: `lifeos-drain`'s
`dispatch()` (`services/lifeos-drain/src/lib.rs`) routes `ingest`, `pipeline`, and
`memory_sleep` jobs to real handlers, and `claim_next_module_request`/`run_module_build`
poll and drain `module_requests` directly (outside the `jobs` table, by design - see that
function's doc comment). `module_build` jobs enqueued through the `jobs` table itself
remain a `Dispatch::Stub`, the same acknowledged-but-not-yet-wired shape as `eval`/
`reconcile` (see `docs/PLATFORM-SYSTEMS.md` §6). The bot's command surface has since grown
beyond `/addmodule`/`/ingest`: `worker/src/commands.ts` also implements `/recall <query>`
(lexical entity search, "what did I note about X") and `/pending` (lists every
`pending_approval` entity awaiting a Telegram approve/deny tap, `docs/SECURITY.md` §2).

**Heavy drain on the Mac:**
`launchd` poller (or on-wake) → `lifeos-drain` (Rust) atomically claims a `jobs` row (`UPDATE … RETURNING`) → runs the relevant Rust service or a headless Claude Agent SDK job → writes results + `events` → bot notifies.

**Outward action (gated):**
Agent produces a _draft_ (`gated` tool) → Telegram approve/deny buttons → on approve, the Mac/Worker calls the provider via the **Nango proxy** (token injected server-side; agent never saw it) or the **browser actuator** → `events` records the outward action.

**Search/recall:**
Any tier queries the FTS5 + sqlite-vec hybrid (RRF) over `lifeos-derived.db`; the shared canonical DB is the cross-tier memory.

---

## 6. Where everything lives (directory layout)

```
life-os/
  frontend/                   # React + Vite SPA - the implemented UI (supersedes the
    index.html                #   original vanilla `core/` prototype, now removed)
    src/
      main.jsx App.jsx        # entry + React Router routes
      index.css               # Neo-Brutalist design tokens (Tailwind v4 `@theme`)
      components/ pages/       # shell (Layout, AIConsole, CommandBar) + route pages
      core/                   # generic renderers (list/board/table/calendar/gallery/…)
      lib/                    # api client, module registry/manifests, ai, vcs
  modules/                    # one declarative manifest per domain (scaffold layer)
    learning/ tasks/ projects/ trading/ social/ marketing/ design/
    email/ calendar/ files/ notion/ slack/ reading/ travel/
    _template/                # scaffold skeleton for self-extension
  services/                   # Rust services (the heavy brain's native code), 9 crates
    lifeos-agents/ lifeos-api/   lifeos-vcs/     lifeos-ingest/
    lifeos-pipelines/ lifeos-actions/ lifeos-memory/ lifeos-drain/
    lifeos-cli/                # thin allow-listed CLI, binary name `lifeos`
    # broker-guard: planned, not built (docs/SECURITY.md §1, revert f3bd18d)
  server/                     # Node glue where JS is required
    scaffold.js validators/ evals/ build/ agent/ lib/    # scaffold + T0-T5 pipeline
    memvec.py                 # reused harness infra (Python)
  worker/                     # Cloudflare Worker: grammY bot (Haiku) + OAuth callbacks
  migrations/ 0001_core.sql … 0019_memory_communities.sql   # 19 migrations
  infra/                      # self-hosted Nango, etc.
  external/                   # vendored/forked deps (e.g. jj)
  docs/                       # this specification tree
  CLAUDE.md README.md
```

See each sub-doc for the internals of every box above.
