# Life OS — Claude working notes

Self-extending personal (and SaaS-ready) operating system. One generic multi-tenant graph DB, a declarative module/plugin system, an in-app AI that scaffolds new modules on request, and the local Claude Code harness as the brain. Read `README.md` for the narrative spec and **`docs/ARCHITECTURE.md` (+ the `docs/` tree) for the deep, authoritative specification** before non-trivial work.

## Mental model (don't violate these)
- **One generic schema, no per-domain tables.** A task / trade / topic / post / campaign / asset are all rows in `entities`, keyed by `workspace_id` + `module` + `type` + a flexible `attrs` JSON. New domains/fields = **zero migration** by default. Never hand-build a bespoke table per domain (the Notion failure mode we're killing).
- **Multi-tenant from day 1.** Every data row carries `workspace_id`. Personal use = one default workspace, but write all logic tenant-aware - no single-user assumptions, no hardcoded ids. SaaS is a deployment swap, not a rewrite.
- **Modules are declarative.** A module is a manifest (`modules/<id>/module.js` for the scaffold layer; `frontend/src/lib/moduleManifests.js` for the live React app) - data + how to render generic entities, never DOM/router/DB code. Rendering lives in the React SPA under `frontend/` (generic renderers in `frontend/src/core/`). The old vanilla `core/` prototype has been removed; see `frontend/FRONTEND.md`.
- **Codegen runs only on the trusted Mac.** The cloud Telegram bot may *enqueue* (`jobs`, `module_requests`) but never writes code/files. Self-extension builds happen locally, behind two validators; each install is a git commit (revertable).
- **Three tiers, one DB.** Cloud Worker bot (Claude Haiku, light/medium lane) + Turso/libSQL `lifeos.db` (always-on canonical, SQLite-compatible, Rust engine) + Mac harness (heavy lane). Embedded replica keeps the Mac local-first/offline (**must set `offline:true`** or writes go to the remote primary).
- **Sync is last-push-wins, NOT LWW-on-`updated_at`** (libSQL default), row-level over the whole `attrs` blob. Mitigate with single-writer-per-row discipline + `events` (append-only) as the reconciliation source of truth. See `docs/DATA-MODEL.md` §4.
- **Owned credentials only - never the claude.ai MCP connectors.** All integrations use developer apps WE own via self-hosted **Nango** (OAuth vault + proxy); the agent holds only a `connectionId`, never a token. No-API services use a gated **browser actuator** (browser-use fork). See `docs/INTEGRATIONS.md`.

## Adopted stack (don't hand-build these)
grammY (Telegram/Workers) · Drizzle + `@libsql/client` (`/web` on Worker, embedded-replica on Mac) · Refine (`dataProvider` proven on one proof-of-concept page, `frontend/src/pages/RefineDemo.jsx`; not yet the app shell - live generic views are hand-built in `frontend/src/core/renderers/`) · Claude Agent SDK (self-extension) · Nango (OAuth vault) · whisper-rs (transcription) · BLAKE3/FastCDC/Jujutsu (versioning) · sqlite-vec/memvec (search). Full audit: `docs/LICENSING-REUSE.md` (all free; only Nango is source-available ELv2, free for us).

## Rust where it counts
Build in Rust: `lifeos-api` (single DB-token owner), `lifeos-vcs` (universal versioning), `lifeos-ingest` (media→text), `lifeos-pipelines`, `lifeos-actions` (Life OS Actions), `lifeos-memory` (cognitive memory), `lifeos-agents` (keyless local agent-CLI router), `lifeos-drain` (job claim), `lifeos-cli` (binary `lifeos`), marketplace signing. `broker-guard` is planned, not built (see Hard rules below). Stays JS: the React+Vite SPA in `frontend/`, module manifests, Worker bot, `scaffold.js`. Stays Python: memvec. See `docs/RUST-COMPONENTS.md`.

## Day-1 modules
Learning, Tasks, Coding/Projects, Trading, **Social, Marketing, Design**. Extended: Email, Calendar, Files, Notion, Slack, Reading, Travel (owned-OAuth). All others (health, finance, …) added later via the self-extension builder. See `docs/MODULES.md`.

## Headline systems
- **`lifeos-vcs`** - git-style versioning for EVERY file type (video/image/design/3D/audio/docs), content-addressed, per-type semantic diff; version history *is* the `events` log. `docs/VERSIONING.md`.
- **Media intelligence** - memvec is text-only; `lifeos-ingest` transcribes/captions/parses → timestamped `segment` entities → "find the clip where I said X". `docs/MEDIA-INTELLIGENCE.md`.

## Hard rules (security / safety)
- **Outward or irreversible actions are human-gated.** Social posts/DMs, email sends, calendar writes, drive shares, browser actions, any trade action go draft → Telegram approve → execute. **Reads are free.** Never let the agent or bot act outwardly autonomously.
- **Trading is read-only for any agent/bot.** No order tool registered anywhere: `KiteClient` exposes no place/modify/cancel/GTT method, no order route is registered in `routes/mod.rs`, and `server/lib/routeAllowlist.js` fail-closed-denies any `broker`/`order` path by omission; broker keys read-scoped. A `broker-guard` PreToolUse hook denying place/modify/cancel/GTT is planned but not built - a first attempt (matcher `*`) fail-closed on ordinary tool calls too and was reverted (issue #99); it remains the target spec for closing the one residual gap, an independently loaded order-capable MCP. Real orders only via a separate human-typed-confirmation executor.
- **Tokens live in Nango (encrypted), never in agent context;** the API injects them at call time via the proxy. Non-Nango secrets (Kite daily token, WhatsApp) are envelope-encrypted in `connections.secret_enc`.
- **`events` is append-only.** No UPDATE/DELETE route. It is the domain log, the harness run-log (Observe/Eval/Release), the version history, and the sync-reconciliation source of truth.
- **Derived state lives in a SEPARATE un-synced DB** (`lifeos-derived.db`; FTS5 + sqlite-vec) - libSQL has no table-level "don't sync" flag, so physical separation enforces it. Rebuilt locally. Blobs sync out-of-band to R2/S3, never through libSQL.

## Telegram bot scope (Claude Haiku)
Full common-DB RW **within its workspace** (entities/edges/events/annotations) + harness memory recall (shared-DB FTS5/vector) + audit logging (`events`). **Cannot:** write code/files, place orders, publish without approval, promote configs. Heavy/deep work → enqueue to the Mac.

## Token discipline
API-first thin-HTTP tools over heavy MCPs; CRUD is never an MCP. mcp-multiplexer hot-loads heavy MCPs (Figma, Higgsfield, social APIs) only on demand, unloaded at cleanup. Bounded context injection replaces always-mounted MCPs.

## Reuse before build
Generalize `../../01_Inbox/knowledge-atlas/` (`app.js`, `annotations.js`, `intelligence.js`, `tools/server.js`, `tools/memory.js`). Reuse harness infra: `~/.claude/bin/memvec.py`, `memory-recall`, `session-capture`, `~/.claude/logs/route.jsonl`, `~/.claude/metrics/costs.jsonl`. Reuse skills: `copywriting`, `marketingskills-ai-agents`, `figma-*`, `mcp-figma`, `mcp-higgsfield`.

## Conventions
- Conventional commits; no co-author trailers. Many small files (200-400 lines, 800 max), functions <50 lines, immutable patterns. Tests ≥80%, TDD. Light, minimalist UI palette.
- Build is phased (README §"Build Order"); each phase independently usable, ships with tests + a commit.
- **One commit per resolved GitHub issue.** After fully resolving an issue (implementation + tests passing), make a single conventional commit referencing it (e.g. `feat: full-spec 0001_core.sql data-plane migration (#1)`) before starting the next issue.
- **Run `cargo clean` in `services/` after every Rust build session.** `services/target` is a debug-build cache with incremental compilation artifacts that regrows into tens of GB across a multi-crate workspace if never pruned - it's gitignored so it never hits git, but it still eats local disk. Clean it once you're done building/testing for the session (not between every single `cargo build`, since that defeats incremental compilation mid-session).
