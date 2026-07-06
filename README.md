# Life OS

A self-extending, agent-driven personal operating system - one place to manage **learning, tasks, coding/projects, trading, social, marketing, and design** (and anything else you add later: health, finance, …), backed by a single common database, controllable from Telegram even when the laptop is off, and able to **build its own new features on request**.

It is personal today but **architected to become a multi-tenant SaaS** with no rewrite.

This README is the canonical architecture document - a superset of the original plan. `CLAUDE.md` is the short working-rules companion.

---

## 0. Quickstart

```sh
git clone <this-repo> && cd life-os
./setup.sh    # checks Rust/Node, builds services, installs frontend deps
./run.sh      # API on :8080 + web app on :5173 (add --with-drain for jobs)
```

Zero configuration needed: the API auto-creates and migrates a local
`lifeos.db` on first boot, and AI runs **keyless** through whatever coding
agent CLI is already on your PATH (Claude Code, Gemini CLI, Codex, OpenCode,
Hermes, Antigravity, ...) - switch agent/model on the go from the AI Console.
Every optional knob (Turso sync, Telegram bot, Nango OAuth, whisper, ...) is
documented in `.env.example` and `docs/MANUAL-SETUP.md`.
The web app is an installable PWA (Add to Home Screen / Install app).

---

## 1. Why this exists

Two problems drove it:

1. **Notion's free AI is too weak and forces manual database-building.** Every tracker had to be hand-modeled, and the AI couldn't populate or evolve it. Life OS inverts this: a generic graph schema means you *never* hand-build a database per domain, and the agent reads/writes it directly.
2. **Tools and data were siloed** - Postgres apps, broker APIs, JSON task files, a SQLite memory DB, markdown vaults, social accounts, Figma files - none talking to each other. Life OS unifies them under one database and one agent loop.

Four requirements reshaped the design:

- **Mobile-first control with the laptop off** - manage everything from a Telegram bot 24/7.
- **Self-extension** - an AI inside the app that, when asked, edits the codebase to add a brand-new module itself.
- **Real-world integrations from day 1** - social (multi-account OAuth), marketing, and design are first-class, not "addable later."
- **SaaS-ready** - multi-tenant, secret-isolated, and billable if it ever leaves personal use.

The result is a **generic multi-tenant entity-graph + a module/plugin system + a self-extension builder + the local Claude Code harness as the brain**, with seven seed domains and infinite room to grow.

---

## 2. Design principles

- **One generic schema, specialized by declarative manifests.** Storage is generic; per-domain behavior is metadata. The opposite of Notion's per-database modeling tax.
- **Multi-tenant from the first commit.** Every data row carries `workspace_id`; personal use is just one workspace. No single-user assumptions anywhere.
- **Local-first, no lock-in.** SQLite-compatible store; offline on the Mac; data is yours and portable.
- **Codegen only on the trusted Mac.** The always-on cloud surface can only enqueue; all file-writing and reasoning-heavy work happens locally.
- **Auditability over speed; gate the irreversible.** Append-only event log; outward/irreversible actions (social posts, trades) are human-gated; every self-built module is a revertable git commit.
- **Minimum always-on context, token-disciplined.** API-first thin tools over heavy MCPs; on-demand loading only.
- **Reuse before build.** Generalize the existing `knowledge-atlas` app, the existing harness, and existing skills (copywriting, figma-*, mcp-*).

---

## 3. Architecture overview — three tiers, two brains, one DB

```
                         ┌─────────────────────────────┐
   Telegram  ──────────► │  Cloudflare Worker (free)    │   LIGHT/MEDIUM brain
   (laptop off-OK)       │  bot — Claude Haiku          │   full DB (workspace) +
                         │  full common-DB + memory +   │   memory + audit;
                         │  audit; enqueues heavy → jobs│   gated outward actions
                         └──────────────┬──────────────┘
                                        │  HTTPS (workspace-scoped, authed)
                         ┌──────────────▼──────────────┐
                         │   Turso / libSQL  lifeos.db  │   CANONICAL, always-on
                         │ control-plane + data-plane   │   multi-tenant,
                         │ entities·edges·events·jobs…  │   SQLite-compatible
                         └──────────────┬──────────────┘
                                        │  embedded replica (periodic sync)
   ┌────────────────────────────────────▼────────────────────────────────┐
   │  MAC (when awake)                                                     │
   │  • lifeos local API  (single DB-token owner, 127.0.0.1, authed)     │   HEAVY brain
   │  • Life OS SPA (generalized knowledge-atlas, light theme)           │   deep work
   │  • Claude Code harness: module scaffolder, Eval, Release, deep agents│
   │  • thin curl tools in ~/.claude/bin (allow-listed) + broker-guard    │
   │  • Nango (self-hosted): OAuth vault + proxy for social/Figma/Notion  │
   │  • integration callers: Figma, Higgsfield, social APIs (on-demand)   │
   └──────────────────────────────────────────────────────────────────────┘
```

**Two brains, one database:**

- **Light/medium brain (cloud, always-on).** A Telegram bot on Cloudflare Workers (scale-to-zero compute = free) running on **Claude Haiku**. It has **full common-DB access scoped to its workspace** (entities/edges/events/annotations RW), **harness memory recall** (shared-DB FTS5/vector), and **audit logging** (`events`). It handles capture, queries, and medium actions, **gates outward actions** (social posts, trades) for approval, and **enqueues** anything heavy/codegen for the Mac. Works with the laptop off. (Compute is free; Haiku tokens are minimal.)
- **Heavy brain (Mac).** The existing Claude Code harness (Claude) does deep work: study authoring, coding, trade analysis, the self-extension builder, integration-heavy design/marketing work, and the Eval/Release loop. Syncs to the same DB.

**Why Turso/libSQL:** SQLite wire-compatible (all existing FTS5 / `memvec.py` code ports unchanged), hosted and always reachable, and **purpose-built for multi-tenant SaaS** (cheap database-per-tenant when we scale). An **embedded replica** on the Mac preserves local-first/offline; the cloud copy is always awake for Telegram.

**Key invariant:** request/state is **data** in the synced DB (survives the Mac being off); **codegen + heavy reasoning only ever run on the trusted Mac**; the cloud surface stays trivial.

---

## 4. The common database

### 4.1 Data plane (one generic schema, per workspace)
A task / trade / topic / post / campaign / asset are all rows in `entities`, distinguished by `workspace_id` + `module` + `type` + a flexible `attrs` JSON. **New domains and fields need zero migration by default.**

| Table | Purpose |
|---|---|
| `entities` | Every typed node: `workspace_id`, `module`, `type` (topic/task/project/trade/social_account/post/campaign/content/lead/design_file/asset/component/note/gap/…), `parent_id` hierarchy, `status`, `tier`, `ts`, **`attrs` JSON** (escape hatch), `source`. FTS5-backed. |
| `edges` | Typed cross-domain links (generalizes the atlas's `connections`): `connection`/`depends_on`/`blocks`/`derived_from`/`owns`/`publishes_to`/`uses_asset`/…; `state` (pending/accepted), `created_by`; nullable `dst_id` + `dst_ref`. |
| `events` | **Append-only** log: domain events (study.review, task.completed, trade.closed, post.published, campaign.sent) **and** harness run-rows (run_id, tier, model, tokens, cost, latency, error, outcome, eval_score, gated). Powers Observe/Eval/Release. |
| `annotations` | Reader notes (generalizes the atlas's localStorage comment/link/question layer). |
| `jobs` | Heavy-work queue: bot enqueues, Mac drains. |
| `module_requests` | Self-extension queue: survives the Mac being off. |

### 4.2 Control plane (SaaS-ready, single-row for personal use)

| Table | Purpose |
|---|---|
| `workspaces` | Tenant. Personal = one seeded row. Carries plan/limits. |
| `users` | Identity. Personal = one row (you). |
| `memberships` | `user_id ↔ workspace_id` + role (owner/admin/member). |
| `connections` | Per-workspace, per-account integration credentials: `provider` (instagram/x/whatsapp/slack/reddit/figma/notion/kite/…), `account_handle`, `scopes`, **`access_token_enc` / `refresh_token_enc` (encrypted at rest)**, `expires_at`, `status`. Supports **multiple accounts per provider**. |

**Billing removed by design (migration `0013_remove_billing.sql`, issue #104):** the `subscriptions`/`plans` billing seam from the original control-plane migration was dropped - `DROP TABLE IF EXISTS` on both, never read by any route. This is a self-hosted, bring-your-own-database-and-AI-model project; there is no product to meter or bill. `workspaces.plan` stays as a free-text label only, never checked by any route.

**Tenancy strategy:** schema is `workspace_id`-scoped everywhere; personal deployment uses **one shared DB**, and the API enforces workspace filtering (RLS-style). SaaS scales via **Turso database-per-workspace** - the local API abstracts "which DB" so this is a deployment swap, not a code change. Secrets are **never** synced into the agent context or the replica's reach of the bot; the API injects them at call time using a per-workspace envelope key.

### 4.3 Search & recall
- **Lexical:** `entities_fts` (FTS5; triggers flatten `attrs` → `attrs_text`).
- **Semantic:** `entity_vec` reusing `~/.claude/bin/memvec.py` (MiniLM-384, sqlite-vec `vec0`); fuse with FTS5 via RRF like `~/.claude/bin/memory-recall`. The shared DB *is* the cross-tier memory the Haiku bot recalls from.
- **Derived state lives in a SEPARATE, never-synced database file.** Turso/libSQL has **no table-level "don't sync" flag** - a synced embedded replica converges to the primary's schema, so a local-only table in the synced file would be clobbered. Derived state (FTS5 + sqlite-vec) therefore lives in `lifeos-derived.db` (plain SQLite, **no `syncUrl`**), `ATTACH`-ed at runtime. This is already how `memvec.py` stores vectors, so it is reuse, not new work; physical separation enforces "never synced" instead of a flag that does not exist.
- **Keep sqlite-vec/memvec; do not switch to libSQL native vectors.** Native vectors are beta with the weakest *local* path and would only help if vectors lived in the synced DB - which violates the rule above.

### 4.4 No-migration growth & sync
Hot query paths get **additive** `GENERATED … VIRTUAL` columns over `attrs` (e.g. `due`) - confirmed indexable (expression index) and addable via `ALTER TABLE ADD COLUMN` **without a table rewrite**; ~90% of new modules ship with no SQL.

**Sync model (corrected against how libSQL actually behaves):**
- `events`/`jobs`/`module_requests` are append-only → conflict-free.
- **Embedded-replica writes go to the remote primary by default; the Mac sets `offline: true`** (Turso Sync, public beta) so writes land locally first and sync bidirectionally - this is required for the local-first/offline premise, not optional.
- **Turso's default conflict resolution is *last-push-wins* (not LWW-on-`updated_at`), at row granularity over the whole `attrs` JSON blob.** Mitigation, defense-in-depth: (1) **single-writer-per-row discipline** - the bot writes its lane (light/medium), the Mac writes its lane (heavy); make it an explicit invariant; (2) treat the **append-only `events` log as the reconciliation source of truth** - repair entity state from the event log on a detected row conflict rather than trusting the blind last-push outcome; (3) optionally a field-merge `conflictResolver` (per-field `updated_at` inside `attrs`) when both tiers start mutating the same rows.
- Derived state (FTS5/vectors) is rebuilt locally in `lifeos-derived.db` and never participates in sync.

---

## 5. Modules — the plugin system

Each module ships `modules/<id>/module.js` calling `osRegisterModule({...})` - the generalization of the knowledge-atlas's `atlasAdd` merge-by-id contract. A manifest is **declarative**: data + how to render generic entities, never DOM/router/DB code.

```js
osRegisterModule({
  id, name, icon, color /* light palette */, num, version,
  entityTypes: { <typeId>: { label, plural, icon,
    attrs: { <field>: { type:'text|number|date|enum|ref|bool|secret', enum?, ref?, required? } },
    display: { title, subtitle?, badge? }, lifecycle: [/* statuses */] } },
  views: [ { id, label, kind:'list|board|table|calendar|detail|graph|gallery', type, groupBy?, sortBy?, filter?, columns? } ],
  events:      [ /* emitted event types */ ],
  botCommands: [ { cmd, help, handler } ],        // Telegram surface
  agentTools:  [ { name, schema, impl, gated? } ],// harness surface; gated=true → draft/approve
  integrations:[ { provider, scopes, onConnect } ],// OAuth/API providers this module uses
  syncTargets: [ { kind:'notion', db, map } ],     // optional outbound sync, no lock-in
  seed:        [ /* optional starter entities/edges */ ],
});
```

### Day-1 seed modules

- **Learning / Study** - the knowledge-atlas generalized to *any subject*: domains → topics → subtopics → resources, cross-domain edges, gaps inbox, spaced repetition, examiner/teach-back. Atlas data files wrapped via an `atlasAdd → osRegisterModule` shim. Telegram: `add topic`, `quiz me`, `what's due`.
- **Tasks / Productivity** - tasks/projects/schedule; Kanban board + "today" list. Telegram: `/task`, `/done`, `/today`.
- **Coding / Projects** - seeded from a git scan of the ~27 repos in `04_Projects`; status, gaps, CI/review state, harness links. Telegram: project status, what's blocked.
- **Trading** - **from scratch (not `ai-trade`)**: trade journal (entry/exit/stop/target/R-multiple/PnL/emotion), reusable setups/playbooks, equity-curve from `events`. `thesis` edges to learning topics → journal and atlas are one graph. **Broker read-only; logging/analysis only**; order placement is human-gated and out-of-agent (see §7). Telegram: `/buy` (logs a *planned* trade), `/close`, `/pnl`.
- **Social** - **multi-account OAuth** for Instagram, X, WhatsApp, Slack, Reddit, via **self-hosted Nango** (it *is* the encrypted token vault + OAuth dance + auto-refresh + ~800 providers). Nango's **Proxy** matches the security model exactly: the local API calls `nango.proxy({connectionId, endpoint})`, Nango injects the live token server-side, and the **agent only ever holds a `connectionId` handle, never a secret**. Each connected account is an `entity` (`type=social_account`) referencing a Nango `connectionId`; you hold **many accounts per platform** natively. **Kite (daily request-token) and WhatsApp Business Cloud are custom connectors** Nango doesn't model cleanly. The AI can **read** (feeds, mentions, DMs, threads) freely and **draft** posts/replies/DMs, but **publishing is human-gated** (draft → Telegram approve → publish via the same proxied connection), because it is outward and irreversible. Posts/mentions/threads are entities; engagement is `events`. Telegram: `/inbox`, `/draft`, approve/deny buttons.
- **Marketing** - campaigns, a content calendar, audiences/segments, leads, channels, and analytics. Content is drafted with the `copywriting` + `marketingskills-ai-agents` skills, **publishes via Social** accounts (`publishes_to` edge), and **uses Design assets** (`uses_asset` edge); funnel/UTM metrics arrive as `events`. Outward sends (broadcast email, ad launch, scheduled posts) are human-gated. Telegram: campaign status, draft content, approve sends.
- **Design** - **Figma (read+write)** via `mcp-figma`, **image/video generation** via `mcp-higgsfield`, plus a design-system/asset library. Entities: `design_file` (Figma ref), `component`, `token`, `asset` (exported media), `brief`. The AI can read/inspect Figma, generate assets, build/maintain a design system (`figma-generate-library`), and implement designs to code (`figma-implement-design`). Assets feed Marketing/Social via edges. Integration MCPs are loaded **on-demand via mcp-multiplexer** and unloaded after. Views include an asset **gallery** and a component library.

Future domains (health, finance, CRM, …) are added the same way - via the self-extension builder. **You never enumerate domains up front.**

---

## 6. Self-extension — the headline feature

An in-app "**+ Ask AI to add a module**" affordance grows the system by talking to it.

**Mac online (synchronous):**
1. `POST /api/module-request { prompt, workspace_id }` → `module_requests` row + `events('module.requested')`.
2. `server/scaffold.js` drives the **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`, `query({prompt, options})`) - not a raw `claude -p` subprocess - so tool-locking, structured output, and hooks are first-class. Restriction is **defense-in-depth, 3 layers**: (A) `allowedTools`/`disallowedTools` + `permissionMode: "dontAsk"` (hard-deny, headless-safe); (B) a **PreToolUse hook** that fails closed when a `Write`/`Edit` `file_path` escapes `modules/<id>/` (the dir scope `allowedTools` can't express; a hook deny holds even under bypass); (C) the built-in **macOS Seatbelt sandbox** (`failIfUnavailable: true`) confining any Bash child - in a git worktree it auto-allows shared `.git` writes so `git commit` works.
3. The agent copies `modules/_template` → `modules/<id>/`, fills the manifest, and emits the manifest summary as **schema-validated structured output** (`outputFormat: {type:"json_schema", schema}` from a **Zod** source of truth; the SDK re-prompts on mismatch - no hand-rolled ajv retry loop).
4. **Two validators:** structural (pure Node: manifest matches schema, no duplicate type ids, no dangling `view.type`) + render (headless Playwright against a **scratch derived/replica DB**, 0 JS errors, asserts an app-emitted `module-mounted:<id>` event, one bounded retry).
5. Insert `modules` row + `events('module.installed')`; **SSE hot-reload** - new tile appears, no restart.
6. On failure → `status='failed'` surfaced in-app, one retry.

**Mac offline (queued):** Telegram `/addmodule …` → the bot writes `module_requests(status='queued')` to cloud Turso, replies "queued". A LaunchAgent poller on the Mac (modeled on `~/.claude/bin/route-daemon`) drains on wake, runs the identical local build, commits to git, bot notifies "✅ live".

**Safety:** the cloud bot only enqueues; codegen only ever runs on the Mac; the agent writes only under `modules/` (never `core/`); every install is a git commit → one `git revert` away.

---

## 7. Harness integration, permissions & the agent loop

### Single writer + thin tools
One local `lifeos` API (extending the atlas's zero-dep Node server; FastAPI acceptable) owns the DB write token at `127.0.0.1` and enforces **workspace scoping + auth**. Agent tools are **thin curl wrappers** in `~/.claude/bin/` (`lifeos entity|edge|event|job|config`, `tg send`, `social draft`, `design gen`), allow-listed in `settings.json`. **CRUD is never an MCP.** mcp-multiplexer hot-loads heavy MCPs (Figma, Higgsfield, social) only when needed; a bounded `UserPromptSubmit` context-injection hook replaces always-mounted MCPs.

### Permission boundary (hard, layered)
- **Common DB:** harness = full RW; Haiku bot = full RW **within its workspace**.
- **Trading:** **read-only** for any agent/bot. No order tool registered anywhere; fail-closed `broker-guard` PreToolUse hook denies place/modify/cancel/GTT even if the Kite MCP is mis-loaded; broker keys read-scoped. Orders flow agent → `proposed_order` entity → Telegram approve → separate interactive `trade-exec` (never agent/hook/cron-callable, typed confirmation). **No autonomous trading.**
- **Outward actions (social/marketing publish):** `gated` agent tools produce **drafts only**; publishing requires Telegram approval, then a worker/Mac executor calls the provider API.
- **Secrets:** OAuth tokens live encrypted in `connections`, **never** in agent context; the API injects them at call time.
- **`events` append-only:** no UPDATE/DELETE route, so even the RW token cannot rewrite history.

### The four diagram gaps (reuse existing infra, near-zero new always-on cost)
- **Event store** - the `events` table doubles as the harness run-log; a `lifeos-sync-events` bridge in the existing **Stop hook** joins `~/.claude/logs/route.jsonl` + `~/.claude/metrics/costs.jsonl` + `session-capture` into one append-only row per run. Cloud rows written by the Worker.
- **Eval + Gate** - LLM-as-judge on the Mac via the existing `claude -p --model haiku` pattern (reusing archived `eval-harness`/`agent-eval` rubrics), **sampled + ship-class only + content-cached** → cents/day. `eval-gate` wraps the commit/sync/job-complete boundary; below threshold → `gated=1`, ship blocked, rationale to Telegram. Trade analysis judged on **data-grounding only, never PnL, never auto-acts**.
- **Observe** - a `harness observe` case beside `route-stats`; reads `events` + quotas, breaks down tokens/cost/latency/error/gated per tier/module/phase, with a cloud free-tier + Haiku-spend meter.
- **Release loop** - a `lifeos-release` learner turns logged outcomes into **candidate** versioned `configs` (incl. the "learned reranking prior" as a JSON bias on `route_core.py`); shadow-replayed → Telegram-approved → `config promote` (human-gated) flips active atomically; rollback = one pointer flip; every flip is an event.

### Cloud ↔ Mac queue
`jobs` table (kept in Turso - **not** Cloudflare Queues, whose free tier drops messages after 24h and splits the audit trail from `events`). No queue library (graphile-worker/pg-boss are Postgres-only; no mature libSQL queue exists). The atomic claim is a one-liner - `UPDATE jobs SET status='running', claimed_by=:w WHERE id=(SELECT id FROM jobs WHERE status='queued' ORDER BY priority DESC, created_at ASC LIMIT 1) RETURNING …` - plus a reaper that returns timed-out `running` rows to `queued` (handles a Mac crash mid-job). `module_requests` is the same pattern with its own lifecycle. Triggered by a `launchd` LaunchAgent poller (`StartInterval` + `RunAtLoad`, re-fires on wake). Turso is the only always-on piece.

---

## 8. Interface

A generalization of the `knowledge-atlas` SPA into a multi-module shell. **Implemented as a React + Vite SPA under `frontend/`** (the original plan called for a vanilla-JS no-build `core/`; that prototype was superseded and removed - see `frontend/FRONTEND.md` for the authoritative status).

- **Reuse (concepts carried over):** the atlas router, SVG cross-module graph, markdown engine, search, `annotations`, `intelligence`.
- **Implemented in `frontend/src/`:** `lib/moduleRegistry.js` (`osRegisterModule` contract) + `lib/moduleManifests.js`, `lib/db`/`lib/api.js` (libSQL + workspace context), soft auth, `core/renderers/{list,board,table,calendar,detail,graph,gallery,timeline,map}.jsx`, `index.css` design tokens, the seven manifests; plus `server/{scaffold,…}.js` for self-extension.

Each module renders generically from its manifest's `views` + `entityTypes.display`: a trade → journal table + equity calendar; a task → Kanban board; a topic → atlas article + connection chips; a design asset → gallery; a campaign → calendar/funnel - all from the same `entities`/`edges` rows. You get cross-domain edges, one search index, one event log, and one graph across every module for free.

---

## 9. Directory layout

```
life-os/
  frontend/                  # React + Vite SPA - the implemented UI (supersedes the
    index.html               #   original vanilla `core/` prototype, now removed)
    src/ main.jsx App.jsx    #   entry + React Router routes
      components/ pages/      #   shell (Layout, AIConsole, CommandBar) + route pages
      core/renderers/         #   generic list/board/table/calendar/gallery/… views
      lib/                    #   api client, module registry/manifests, ai, vcs
      index.css               #   Neo-Brutalist design tokens (Tailwind v4 @theme)
  modules/
    learning/  module.js data/01_dsa.js … 13_gpu.js   # migrated atlas data
    tasks/     module.js
    projects/  module.js                              # git-seeded
    trading/   module.js                              # from scratch
    social/    module.js                              # multi-account OAuth
    marketing/ module.js
    design/    module.js                              # Figma + Higgsfield
    _template/ module.js views.md README.md           # scaffold skeleton
  services/                  # Rust services (the heavy brain's native code), 9 crates
    lifeos-agents/ lifeos-api/ lifeos-vcs/ lifeos-ingest/ lifeos-pipelines/
    lifeos-actions/ lifeos-memory/ lifeos-drain/ lifeos-cli/   # binary name `lifeos`
  server/                    # Node glue where JS is required
    scaffold.js memvec.py agent/ build/ validators/ lib/ evals/ test/
  worker/                    # Cloudflare Worker: Telegram bot (Haiku) + OAuth callbacks
  migrations/ 0001_core.sql … 0019_memory_communities.sql   # 19 migrations
  infra/                     # self-hosted Nango, etc.
  external/                  # vendored/forked deps (e.g. jj)
  CLAUDE.md README.md
```

---

## 10. Build order (phased; each phase independently usable)

1. **Foundations** - Turso project + `0001_core.sql` + `0002_control_plane.sql` (workspace/users/memberships/connections); local `lifeos` API (single DB-token owner, workspace-scoped); embedded-replica sync; thin `~/.claude/bin/lifeos` tool + allow-list; seed the personal workspace/user. → *agent can CRUD the common DB, tenant-aware.*
2. **App shell + seed modules** - generalize the atlas into `core/` (light palette), `osRegisterModule`, generic `views/`; ship Learning, Tasks, Projects, Trading, Social, Marketing, Design. → *usable multi-module SPA over the DB.*
3. **Integrations** - OAuth flows + encrypted `connections` for Instagram/X/WhatsApp/Slack/Reddit; Figma + Higgsfield via mcp-multiplexer; read paths live, write paths drafted. → *real accounts/files connected, AI can read & draft.*
4. **Telegram lane (Haiku)** - Cloudflare Worker bot on Claude Haiku with full workspace DB + memory recall + audit; capture/query/medium actions; gated approve/deny for outward actions; enqueue heavy → `jobs`. → *control + capture from phone, laptop off.*
5. **Self-extension builder** - `/api/module-request`, `scaffold.js`, `_template/`, two validators, SSE hot-reload, offline queued variant + drain poller. → *"ask AI to add a module" works.*
6. **Harness loop** - event-store bridge, Eval+Gate, `harness observe`, Release loop, `broker-guard` + read-only broker tools + gated executors. → *everything logged, scored, auto-improved, boundaries enforced.*
7. **SaaS hardening (when needed)** - real auth/sessions, database-per-workspace swap, plan/quota gating, billing. → *flip from personal to multi-tenant product.*

Each phase ships with tests (≥80%, TDD) and a conventional-commit history.

---

## 11. Verification (per phase)

- **DB/tools:** `lifeos entity create … && lifeos event add …`; assert rows + FTS5 hit + memvec recall; **workspace isolation** (a second workspace can't see the first's rows); kill cloud, write offline, reconnect, assert replica converges.
- **SPA:** load offline then with the local API; each module's views render; cross-module graph draws from `edges`; search works; manual light/minimalist UI check.
- **Integrations:** connect a throwaway account per provider; AI reads feed/mentions; a draft post is created but **not published** until approved; tokens stored encrypted, absent from agent context/logs.
- **Telegram (Haiku):** with the Mac **off**, capture/query/`/pnl`/`/inbox`/`/addmodule` respond; outward action shows approve/deny and only acts on approve; heavy `/addmodule` shows `queued`; wake Mac → drain builds + notifies.
- **Self-extension:** "add a health tracker" → new tile after validators; `git log` shows the commit; break the template → validator fails cleanly, no partial register.
- **Permissions (must-pass):** order attempt via agent and via mis-loaded Kite MCP → `broker-guard` denies both; no order tool registered; `events` UPDATE/DELETE → 404; social publish blocked without approval; broker/social reads succeed.
- **Harness loop:** `harness observe` shows tokens/cost/latency/gated per tier/module; a low-quality run is Eval-gated + Telegram rationale; `lifeos-release propose` → shadow-replayed candidate that only activates after human `config promote`.

---

## 12. Tech stack

| Layer | Choice | Why |
|---|---|---|
| Canonical DB | Turso / libSQL | SQLite-compatible (reuse FTS5/memvec), always-on, embedded replica = local-first, built for multi-tenant SaaS |
| Always-on compute | Cloudflare Workers (free) | Scale-to-zero, webhook-shaped; hosts bot + OAuth callbacks |
| Bot LLM | **Claude Haiku** | Capable + cheap; full DB + memory + audit on the always-on lane |
| Heavy-lane LLM | Claude (existing Claude Code harness) | Deep reasoning, codegen, Eval/Release |
| Bot transport | Telegram | Free, ubiquitous, works laptop-off |
| Telegram framework | **grammY** (MIT) | Native Cloudflare Workers adapter (`webhookCallback(bot,"cloudflare-mod")`), inline approve/deny keyboards |
| DB access | **Drizzle ORM** + `@libsql/client` (MIT) | Typed access to `entities/edges/events`; `/web` import on the Worker (no FS/TCP), embedded-replica `syncUrl` on the Mac |
| Frontend | React + Vite SPA under `frontend/` (superseded the original no-build vanilla-JS plan); **Refine** (MIT) adopted, `dataProvider` proven | The live generic views (list/board/table/calendar/gallery/…) are hand-built components in `frontend/src/core/renderers/`, not yet routed through Refine; `frontend/src/lib/refineDataProvider.js` is wired end-to-end against the real API and proven on one proof-of-concept page (`frontend/src/pages/RefineDemo.jsx`), not yet the app shell; bring-your-own graph (Cytoscape) |
| Local API | Node (extend atlas server) / FastAPI | Single DB-token owner, workspace-scoped |
| Semantic search | sqlite-vec + MiniLM-384 (`memvec.py`) in a separate un-synced `lifeos-derived.db` | Reuse existing harness infra; native libSQL vectors rejected (beta, would couple derived state to the synced DB) |
| Self-extension | **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`) | First-class tool-locking, PreToolUse path hook, Seatbelt sandbox, worktree isolation, schema-validated structured output (Zod) |
| Integrations / OAuth vault | **Self-hosted Nango** (Elastic License 2.0) + Figma/Higgsfield via mcp-multiplexer | ~800 providers, encrypted tokens, auto-refresh, **Proxy** keeps secrets out of agent context; Kite + WhatsApp as custom connectors |
| Secrets | Nango-managed encrypted tokens + per-workspace envelope key for non-OAuth | SaaS-safe, never in agent context (agent holds only a `connectionId`) |

---

---

## 13. Owned-integration model (replaces claude.ai MCP connectors)

**Hard rule: every third-party integration uses credentials WE own, never a claude.ai connector.**
The Gmail/Calendar/Drive/Notion/Slack MCPs live inside a claude.ai *lead* account that is not ours; depending on them is fragile and not SaaS-ready.
Instead, **self-hosted Nango** is the OAuth vault for all API providers (Google, Notion, Slack, Instagram, X, Reddit, Figma, GitHub): it runs the OAuth dance, stores tokens encrypted, auto-refreshes, and its **Proxy** injects the live token server-side so the **agent only ever holds a `connectionId`, never a secret**.
Non-OAuth providers (Zerodha Kite daily-token, WhatsApp Business Cloud) are native custom connectors.
Anything with no API at all is driven by a **browser actuator** (fork of `browser-use/browser-harness`), Mac-only and fully gated.
MCPs are reserved for heavy on-demand *capability* tools (Figma, Higgsfield, game engines), never credentialed connectors.
Full detail: [`docs/INTEGRATIONS.md`](./docs/INTEGRATIONS.md).

## 14. Extended capabilities

- **Universal version control (`lifeos-vcs`)** - the headline differentiator: git-style history, branches, tags, semantic diffs, and time-travel over **every file type** (video, image, design, 3D, audio, docs), not just code. Content-addressed (BLAKE3 + FastCDC chunking), modeled on Jujutsu, with per-type semantic diff plugins. Version history *is* the append-only `events` log. A **Rust** service. See [`docs/VERSIONING.md`](./docs/VERSIONING.md).
- **Media intelligence (`lifeos-ingest`)** - "find the clip where I said X" across your library. memvec is text-only, so a Rust pipeline transcribes (whisper-rs), captions, and parses media into timestamped `segment` entities that memvec embeds. Optional CLIP for true visual search. See [`docs/MEDIA-INTELLIGENCE.md`](./docs/MEDIA-INTELLIGENCE.md).
- **GitHub connector + "Life OS Actions"** - manage all repos (issues/PRs/CI/releases) as entities; event-triggered AI automation on *any* entity (`on asset.version_created → draft post`), GitHub-Actions-style but for everything.
- **New modules** - Email, Calendar, Files, Notion, Slack, Reading, Travel - all manifests over the existing schema, owned-OAuth via Nango. See [`docs/MODULES.md`](./docs/MODULES.md).
- **Platform systems** - universal command bar, user/module agent pipelines (DAGs via the Agent SDK), per-module dashboards derived from `events` (zero new storage), a signed module marketplace (the SaaS seam), and a PWA. See [`docs/PLATFORM-SYSTEMS.md`](./docs/PLATFORM-SYSTEMS.md).
- **Game-dev connectors** - Godot/Unity/Unreal via mcp-game-engines; Godot `.tscn` scenes are text → first-class semantic diffs in `lifeos-vcs` for free.

## 15. Detailed specification

This README is the narrative superset. The deep, authoritative specification lives in [`docs/`](./docs/ARCHITECTURE.md):
data model, every module, the owned-integration model, `lifeos-vcs`, media intelligence, self-extension, the harness loop, platform systems, the Rust component inventory, the security model, the phased build plan, and the licensing/reuse audit.

---

*Status: implemented and tested through issue #140 - 9 Rust crates, a React SPA, a Cloudflare Worker, 19 migrations, roughly 1000 passing tests. See `CLAUDE.md` for working rules, [`docs/`](./docs/ARCHITECTURE.md) for the full spec (each section annotated with what's actually built), and §10 / `docs/BUILD-PLAN.md` for the phased build.*
