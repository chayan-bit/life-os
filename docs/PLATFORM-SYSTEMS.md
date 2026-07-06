# Platform systems

Cross-cutting systems that are not modules but make Life OS feel like one app you never leave.
All sit on the existing schema (`entities`/`edges`/`events`/`jobs`) - **zero new tables.**

---

## 1. Command bar + agent pipelines

### 1.1 Command bar (`Cmd-K`)
A single natural-language bar that routes to entities, actions, or pipelines.
- **Search:** queries the one hybrid index (FTS5 + memvec RRF) over **every module** → entities across domains.
- **Verbs:** "draft a post about X", "what's blocked", "diff this asset", "find the clip where I said Y".
- **Build:** `frontend/src/components/CommandBar.jsx` (JS, browser) over the existing search + tool registry. No new storage.

### 1.2 Agent pipelines (user/module-defined DAGs)
Multi-agent Workflows declared in the manifest:
```js
pipelines: [{ id:'post-from-topic', stages:[
  { agent:'research', tool:'memvec.recall' },
  { agent:'draft',    skill:'copywriting' },
  { agent:'verify',   gate:'eval' },
  { agent:'publish',  tool:'social.draft', gated:true } ]}]
```
- **Engine `lifeos-pipelines`:** runs on the Mac heavy lane, enqueued via `jobs`, dispatching **Claude Agent SDK** calls per stage.
- Each run = a `pipeline_run` entity; each stage writes `events` (run_id, stage, tokens, outcome) → reuses the Eval/Gate/Observe loop ([HARNESS-LOOP.md](./HARNESS-LOOP.md)).
- Outward final stages are **gated**.
- **Build:** 🦀 Rust orchestrator (or JS if kept close to `scaffold.js`); the per-stage worker is the Agent SDK.

**Implemented (issue #92):** `services/lifeos-pipelines` is the real orchestrator.
`lifeos-drain` claims `pipeline` jobs (payload `{pipeline, input}`, same shape
`routes/planned.rs::pipeline_run` already enqueues) and calls
`lifeos_pipelines::process_pipeline_job` directly as a library, same pattern as
`lifeos-ingest`. No Rust port of the Claude Agent SDK exists anywhere in this
workspace; instead **`lifeos-agents`** (added later) now fills that role via
CLI subprocesses - it detects an installed coding-agent CLI on PATH and runs
completions through it keyless. The per-stage runner wiring in
`services/lifeos-drain/src/main.rs` is **keyless-CLI-first**: `AgentCliStageRunner`
(via `lifeos_agents::detect()`) is used whenever any CLI is on PATH; only when
none is detected does it fall back to the older pattern established by
`lifeos-ingest/src/vision.rs::HaikuCaptioner` - a direct `reqwest` call to the
Anthropic Messages API, DI-trait-wrapped (`PipelineStageRunner`,
`NoopStageRunner`/`HaikuStageRunner`, env-gated on `ANTHROPIC_API_KEY`) - and
finally to `NoopStageRunner` when neither a CLI nor a key is available.

`pipeline_registry()` is a hardcoded Rust table today, seeded with the one
pipeline this doc actually specifies (`post-from-topic` above, field names
verbatim). Manifest-driven registration - reading a module's own `pipelines:
[...]` array - is a deferred gap: no module manifest declares one yet, so
building the JS→Rust bridge for a single documented example would be
speculative.

Each stage writes one `events` row with the full harness run-log columns
(`run_id` = the job id, `tier='mac'`, `model`, `tokens_in/out`, `latency_ms`,
`outcome`). A `gate:'eval'` stage (`verify` above) runs the real
LLM-as-judge system from [HARNESS-LOOP.md](./HARNESS-LOOP.md) §2 (issue
#96: `eval_gate::HaikuJudge`, content-cached + sampled, falling back to
the original length heuristic `eval_gate::HeuristicJudge` when no judge
call is made) and halts the run (`gated=1` on that event) if the score is
below threshold. Any stage marked `gated: true`
(`publish` above) is **unconditional**: the runner is never called for it;
instead a `pending_approval` entity is drafted (same "only ever drafts"
shape as `integrations.rs::draft_action`, `whatsapp.rs`, `slack.rs`,
`drive.rs`, `travel.rs`) and the run halts at `awaiting_approval` - never
auto-publishing.

---

## 2. Dashboards / analytics (derived from `events`)

**No new storage** - `events` is already the fact table. A manifest declares metrics; a generic agg layer renders them:
```js
metrics: [
  { id:'pnl_curve',      source:'events', where:"type='trade.closed'",   agg:'cumsum(attrs.pnl)', viz:'line' },
  { id:'posts_per_week', source:'events', where:"type='post.published'", agg:'count', bucket:'week', viz:'bar' } ]
```
- **Build:** a frontend render layer over `GET /api/metrics` (`services/lifeos-api/src/routes/metrics.rs`, pure SQL agg over `events` - fast).
- Cross-module dashboard = union over all manifests' metrics.
- **Time-travel "as of"** works because `events` is append-only.

---

## 3. Notifications & digest

- **Daily Telegram (and Slack) digest:** what's due, what's blocked, PnL, drafts awaiting approval, what the Life OS Actions did overnight.
- Built from a scheduled `jobs` row → query over `events`/`entities` → send via the bot.
- PWA push (§5) mirrors the same digest.

**Implemented (issue #65):** the pieces this digest will eventually assemble already exist
as standalone Telegram commands - `worker/src/commands.ts`: `/today` (due today), `/pnl`
(realized PnL), `/inbox` (uncategorized captures - status IS NULL, closest analog to
"what's blocked" until a `task.blocked` event exists), `/draft` (creates a
`pending_approval` entity, "drafts awaiting approval"). The same module also implements
`/recall <query>` (lexical entity search, "what did I note about X") and `/pending` (lists
every `pending_approval` entity awaiting a Telegram approve/deny tap, `docs/SECURITY.md`
§2) - useful digest-adjacent lookups today, even though neither feeds the scheduled digest
itself. No scheduled digest job yet - that's
a `jobs`-row + cron trigger, deferred until the heavy-job enqueue path (#67) exists to model
it on. `/task`/`/topic`/`/done`/`/quiz` are capture/complete/quiz commands, not digest
inputs - see `docs/MODULES.md` §2.1/§2.2/§2.4 for their module-specific notes. Every command
is workspace-scoped (`worker/src/db.ts::resolveWorkspaceId`, #64) and reads/writes only via
`entities.ts`/`events.ts`, never a direct query - tested against a real in-memory libSQL DB,
`worker/test/{entities,events,commands,bot}.test.ts`.

**Implemented (issue #71):** `worker/src/digest.ts::buildDigest` composes the pieces above
into one message. It's a Cloudflare **Cron Trigger** (`wrangler.toml`'s `[triggers] crons`),
not a `jobs` row - the digest is a pure DB read + a single Telegram send, no heavy/codegen
work a Mac drain would need to do, so routing it through the `jobs`/drain machinery #66/#67
built for outward-provider-call and Mac-only work would add a hop for no reason. `index.ts`
exports a `scheduled(event, env)` handler that builds and sends it; unset `DIGEST_CHAT_ID`
means no digest (manual-setup-gated the same way `BOT_TOKEN`/`TURSO_URL`/etc. are,
`docs/MANUAL-SETUP.md`). `buildDigest` is fully unit-tested (`worker/test/digest.test.ts`);
the actual Telegram send is verified live post-deployment, same as `/telegram` itself. PWA
push mirroring this digest is Phase 7, not yet built.

---

## 4. Module marketplace (the SaaS seam)

A module is already a validated, self-contained manifest ([SELF-EXTENSION.md](./SELF-EXTENSION.md)). The marketplace distributes and installs them.
- **Entity types:** `module_package` (`{id, version, author, manifest_ref(blob), signature, installs}`), `module_review`.
- **Publish:** `lifeos module publish` → structural + render validators → **sign** the manifest (ed25519, tamper-evident) → push to a registry (a Turso table + R2 blob, or a GitHub repo of manifests).
- **Install:** the **same two validators run locally on the trusted Mac** before register → git commit. Untrusted manifests get the same `dontAsk` + PreToolUse + Seatbelt sandbox treatment as self-built ones.
- **Security:** never auto-install outside `modules/<id>/`; manifests are declarative (no arbitrary code), drastically limiting blast radius; signature + local re-validation + sandbox.
- **Build:** 🦀 Rust for sign/verify (ed25519) inside the `lifeos` API; validators reused from Phase 5.
- This is the multi-tenant distribution channel.

**Implemented (issues #101/#102):** `services/lifeos-api/src/marketplace_sign.rs`
does the ed25519 sign/verify (a tampered manifest's changed bytes fail
`verify()` - no separate tamper-detection logic needed); `routes/marketplace.rs`
exposes `GET /api/marketplace/pubkey`, `POST /api/marketplace/publish`
(structural check - `manifest.id`/`manifest.version` must match the request -
then sign and store), `GET /api/marketplace/packages`, `POST
/api/marketplace/verify`, and `POST /api/marketplace/install` (re-verifies
the stored signature before recording a `marketplace.installed` event).
`module_packages` (migration `0009_marketplace.sql`) is the registry table
this base uses in place of a separate Turso table + R2 blob. The signing key
comes from `LIFEOS_MARKETPLACE_SIGNING_SEED`; publish/verify honestly 501
until it's set, same posture as Nango/Kite/GOWA. The **render validator**
(headless-Chromium boot) and the **install-as-git-commit** step stay in the
Node scaffold layer (`server/scaffold.js`, `server/validators/render.js`,
docs/SELF-EXTENSION.md §4) - this route covers the marketplace half only.
Frontend: `frontend/src/pages/Marketplace.jsx` (publish form + browse/install
list), routed at `/marketplace`.

---

## 5. PWA (rich mobile beyond Telegram)

- The SPA + `manifest.webmanifest` + a **service worker** (offline cache of the frontend build output + the embedded-replica read model) + **Web Push**.
- Telegram stays for quick capture/approve; the PWA is the full-fidelity surface (galleries, dashboards, version diffs, maps, timelines).
- **Auth:** `frontend/src/lib` auth path (session + workspace) - no-op locally, real for SaaS.
- **Offline:** reads from a local cache / IndexedDB mirror; writes spool to `store/` and reconcile via the single-writer + events-as-truth model ([DATA-MODEL.md](./DATA-MODEL.md) §4).
- **Build:** JS/PWA; the Worker serves push.

**Implemented (issue #103):** `frontend/public/manifest.webmanifest` +
`frontend/public/sw.js` (installable app shell: caches the shell + does
stale-while-revalidate on `GET /api/*` so the embedded-replica read model
stays browsable offline - writes are never cached or served from cache) +
`index.html`'s `<link rel="manifest">` + `main.jsx` registering the service
worker. `POST /api/push/subscribe` / `POST /api/push/unsubscribe`
(`services/lifeos-api/src/routes/push.rs`, `push_subscriptions` table,
migration `0010_push_subscriptions.sql`) store Web Push subscriptions -
`Profile.jsx`'s "Enable Push" button drives the real browser Push API
subscribe flow into this endpoint. **Deferred:** actually sending a push
(VAPID-signed, mirroring the Telegram digest) needs a VAPID keypair + sender
this base doesn't wire up yet - `sw.js`'s `push` event handler is ready to
receive one once that lands. Real app icons (`icon-192.png`/`icon-512.png`)
are also a follow-up; the manifest references them but no binary assets
ship in this base.

---

## 6. Life OS Actions (event-triggered automation - "GitHub Actions for everything")

Declared per module as `actions: [{ on, if?, run }]`; a rules engine over `events` → `jobs`.
Examples:
- `on asset.version_created` → generate thumbnail + caption + draft social post.
- `on design_file.updated` → regenerate code component (figma-implement-design) + open a PR.
- `on trade.closed` → update equity curve + draft journal reflection.
- `on topic.due` → quiz in Telegram.
- **Engine:** 🦀 Rust hot event loop (part of `lifeos-pipelines` or standalone). Outward steps gated; internal steps free.
- New automations need **zero new code** - just manifest entries.

**Implemented (issue #93):** `services/lifeos-actions` is the real hot event
loop, called once per `lifeos-drain` poll tick
(`lifeos_actions::run_action_engine_tick`) - same standalone-crate
convention as `lifeos-pipelines`/`lifeos-ingest`, no dependency on
`lifeos-api`. `action_registry()` is a hardcoded Rust table today, seeded
with the 3 examples this doc lists above verbatim (`asset.version_created`,
`trade.closed`, `topic.due`); manifest-driven `actions: [...]` declarations
are a deferred gap, same as `lifeos-pipelines::pipeline_registry()`'s.

Per-workspace incremental scanning needs no new table: `events.id` is a
time-ordered ULID, so `WHERE id > cursor ORDER BY id ASC` is a correct scan,
and the cursor itself is one `entities` row per workspace
(`module='actions', type='cursor'`) - "zero new tables" holds. `if`
conditions are a real but intentionally minimal single-field-equality check
over the triggering event's `attrs`, not a general expression language -
same minimalism precedent as #92's `eval_stage_output`.

A fired rule enqueues a real `jobs` row (`kind='action'`) plus an
`action.fired` audit event - that is #93's whole acceptance bar ("a
declared action fires on its event and enqueues the right job"). What the
`action` job actually does downstream (thumbnail+caption+draft, equity
curve + journal, Telegram quiz) is deferred: `lifeos-drain` dispatches it
as an honest `Dispatch::Stub`, the same acknowledged-but-not-yet-wired
shape already accepted for `module_build`/`eval`/`reconcile`.

**Frontend (issue #94):** `GET /api/pipeline/registry`
(`services/lifeos-api/src/routes/pipeline.rs`) exposes
`pipeline_registry()` as JSON so the UI no longer hardcodes stage names.
`frontend/src/pages/Harness.jsx` gained a fourth "Pipelines" tab
(`PipelineBuilder.jsx`): it lists every registered pipeline's DAG, triggers
a run, and lists run history from `GET /api/entity?type=pipeline_run`,
expanding a row to `GET /api/event?run_id=<attrs.run_id>` for that run's
stage-by-stage outcome. This required one small addition to
`process_pipeline_job` (`services/lifeos-pipelines/src/lib.rs`): the
`pipeline_run` entity now records its own triggering `run_id` in `attrs`,
so history rows can join to their events - without it, a listed run had no
way to find its own stage log. The trigger + poll logic that used to live
only in `Dashboard.jsx` is now a shared hook
(`frontend/src/lib/usePipelineRun.js`) used by both the Dashboard demo
widget and the new Pipelines tab.
