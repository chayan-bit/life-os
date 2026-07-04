# Self-extension - "Ask AI to add a module"

> **This is Tier 0 (the manifest builder) of the generation ladder specified in [SELF-EXTENSION-V2.md](./SELF-EXTENSION-V2.md).** This doc remains the authoritative spec for the built-and-tested manifest path (issues #72-#80); v2 places it as rung one and adds T1-T5 (renderer → agent tool → backend route → migration → whole subsystem). Read v2 for the ladder; read here for how Tier 0 actually works today.

> The system grows itself: an in-app affordance that, on request, writes a brand-new declarative module, validates it, and hot-loads it - as a revertable git commit.
> Built on the **Claude Agent SDK** (not a raw `claude -p` subprocess), so tool-locking, structured output, and hooks are first-class.

---

## 1. Flow (Mac online, synchronous)

1. `POST /api/module-request { prompt, workspace_id }` → `module_requests` row + `events('module.requested')`.
2. `server/scaffold.js` drives the **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`, `query({prompt, options})`), tool-restricted (see §2), given `modules/_template/` and existing manifests as examples.
3. The agent copies `modules/_template` → `modules/<id>/`, fills the manifest (entity types, attrs, views, integrations, bot commands, light color), and emits a **schema-validated manifest summary** as structured output (§3).
4. **Two validators** run (§4). Both must pass.
5. Insert `modules` row + `events('module.installed')`; **SSE hot-reload** - the new tile appears, no restart.
6. On failure → `status='failed'` surfaced in-app, one retry.

## 1b. Flow (Mac offline, queued)
Telegram `/addmodule …` → the bot writes `module_requests(status='queued')` to cloud Turso, replies "queued".
A `launchd` poller on the Mac drains on wake, runs the **identical** local build, commits to git, bot notifies "✅ live".
**Codegen only ever runs on the trusted Mac; the cloud bot only enqueues.**

**Implemented (issue #76):** the `queued → building → installed | failed` state machine itself,
as atomic, tested primitives - not yet the live drain loop that calls them (that's #78, see
below). `services/lifeos-drain/src/lib.rs` adds `claim_module_request`/`complete_module_request`/
`fail_module_request`, each a single `UPDATE ... WHERE id=?1 AND status='<expected>'` guarded
exactly like `complete_job`/`fail_job`'s lease check (0 rows affected = someone else already
moved this request - never clobber a transition that already happened), and each appending the
matching `events('module.building'|'module.installed'|'module.failed')` row only when the
transition actually applied (a self-contained `emit_event` mirroring `lifeos_api::audit::emit`'s
shape, since this crate has no dependency on `lifeos-api`). `GET /api/module-request/:id`
(`services/lifeos-api/src/routes/module_request.rs`) lets a requester poll the lifecycle,
returning `error` verbatim on `failed` - "surfaces honestly to the requester" per the issue's
acceptance, not a generic message. The SSE filter (`stream.rs`) now includes
`module.building`/`module.failed` alongside the two types it already carried.
**Deliberately not wired into the live `run_job`/`dispatch` path yet**: `module_build` is still
a `Dispatch::Stub` (§2's #72 note - no real `scaffold.js` invocation exists), and calling
`complete_module_request` for a build that never actually ran would manufacture exactly the kind
of false "installed" confidence the #74/#75 validators exist to prevent. #78's real drain loop
(the one that actually shells out to `scaffoldModule()`) is what calls these three functions in
lockstep with `claim_job`/`complete_job`/`fail_job`. Tested with 4 new `services/lifeos-drain`
unit tests (happy path through both events, the failed path with the error surfaced, and two
no-op-outside-expected-state cases) plus 2 new `services/lifeos-api` integration tests for the
GET route (`200` right after `POST`, `404` for an unknown id).

**Implemented (issue #78):** the real offline path - a bot-queued `/addmodule` request now
actually builds and notifies. Two intake paths converge on `module_requests`, but only one
reached the drain before this issue: `POST /api/module-request` links a `jobs` row of kind
`module_build`, while `worker/src/moduleRequests.ts::enqueueModuleRequest` (the Telegram
`/addmodule` path this issue is about) inserts only the `module_requests` row - no `jobs` row
for `claim_job` to ever see. So `lifeos-drain` claims directly off `module_requests`:
`claim_next_module_request` (new, `services/lifeos-drain/src/lib.rs`) is the same atomic
`UPDATE ... WHERE id = (SELECT ...)` shape as `claim_job`, transitioning the oldest queued row
straight to `building`. `run_module_build` then calls a `ModuleBuilder` (real impl
`ScaffoldJsBuilder` spawns `node scaffold.js <prompt> <workspaceId>` - #78 also gave
`scaffold.js` a real CLI entry point whose last stdout line is `scaffoldModule`'s JSON return
value verbatim), applies `complete_module_request`/`fail_module_request` on the result, and -
if the row carries a `chat_id` - calls a `Notifier` (`TelegramNotifier`, a direct
`api.telegram.org/bot<token>/sendMessage` call; no Cloudflare Worker round-trip, since the Mac
is offline-first by design). Both `ModuleBuilder` and `Notifier` are `async-trait` traits with
mock implementations in tests, mirroring the exact DI convention `lifeos-api` already uses for
every external call (`NangoClient`, `WhatsAppClient`).

`module_requests.chat_id` (new `migrations/0004_module_requests_chat_id.sql`, nullable) carries
the Telegram chat to notify back - threaded through from `ctx.chat.id` at the `/addmodule`
call site (`worker/src/bot.ts`) down through `requestModule`/`enqueueModuleRequest`. API-
originated requests have no chat and are simply never notified (`chat_id IS NULL`). Since
`ALTER TABLE ADD COLUMN` isn't idempotent like this project's other `CREATE TABLE IF NOT
EXISTS` migrations, `lifeos-api/src/db.rs::add_column_if_missing` guards it with a
`PRAGMA table_info` check before applying, so `run_migrations` stays safe to call on every boot.

The drain's `module_requests` poll runs sequentially with its `jobs` poll (single-worker
simplicity, same as the rest of this crate) - a long-running build pauses job draining until it
finishes. Accepted tradeoff, not a bug; a future issue can split it into a second poll loop if
this becomes a real problem in practice.

**Not run live in this session** - same reasoning as #72: a real build needs a real
`ANTHROPIC_API_KEY`, costs real tokens, and mutates real git state; here it additionally sends
a real Telegram message to a real chat. All tested with mocks (`services/lifeos-drain`'s
`claim_next_module_request`/`run_module_build` unit tests, plus a `worker` test asserting
`chat_id` round-trips through `enqueueModuleRequest`). `docs/MANUAL-SETUP.md` §78 documents the
one true end-to-end check: queue a request via `/addmodule` on the real bot, run `lifeos-drain`
locally with `TELEGRAM_BOT_TOKEN` set, confirm a real commit lands and a real message arrives.

---

## 2. Tool restriction - defense in depth (3 layers)

Restricting writes to one subdir is not a single switch; layer all three.

**Layer A - locked tool surface (primary gate):**
```ts
options = {
  allowedTools: ["Read","Glob","Grep","Edit","Write","Bash"],
  disallowedTools: ["WebFetch","WebSearch","Bash(rm -rf *)","Bash(git push *)","Bash(curl *)"],
  permissionMode: "dontAsk",   // anything not pre-approved is DENIED, never prompts (headless-safe)
}
```
Do **not** use `bypassPermissions` - `allowedTools` does not constrain it.

**Layer B - PreToolUse hook that fails closed on path (the dir scope):**
`allowedTools` cannot express "Write only under `modules/<id>/`". A hook matching `Write|Edit` (and `Bash`) denies when `tool_input.file_path` resolves outside the target dir:
```ts
hooks: { PreToolUse: [{ matcher: "Write|Edit", hooks: [async (input) => {
  const p = path.resolve(input.tool_input.file_path);
  if (!p.startsWith(targetModuleDir + path.sep))
    return { hookSpecificOutput: { hookEventName:"PreToolUse",
      permissionDecision:"deny", permissionDecisionReason:"writes confined to the new module dir" } };
  return {};
}] }] }
```
Hooks run first; a deny is absolute - it holds even under `bypassPermissions`. This is the code-level guarantee.

**Layer C - OS sandbox (kernel backstop for Bash):**
Enable the built-in macOS Seatbelt sandbox so any shell child is physically confined:
```json
{ "sandbox": { "enabled": true, "failIfUnavailable": true, "allowUnsandboxedCommands": false,
  "filesystem": { "allowWrite": ["./modules"] },
  "credentials": { "files":[{"path":"~/.aws","mode":"deny"},{"path":"~/.ssh","mode":"deny"}],
                   "envVars":[{"name":"GITHUB_TOKEN","mode":"deny"},{"name":"NPM_TOKEN","mode":"deny"}] } } }
```
In a linked git worktree the sandbox auto-allows the shared `.git` so `git commit` works, while denying `.git/hooks` and `.git/config`. `failIfUnavailable:true` makes the build refuse to run if Seatbelt can't init. Note: built-in Read/Edit/Write bypass the sandbox - that is why Layer B's hook is required; the sandbox only confines Bash.

**Implemented (issue #72):** `server/scaffold.js` + `server/lib/{preToolUseHook,sandbox,
worktree,slugify}.js` - all three layers verbatim from this section, wired into a real
`query()` call from `@anthropic-ai/claude-agent-sdk` (confirmed against the installed
package's `sdk.d.ts`, not just this doc). Deliberately deferred to later issues, so this
file doesn't build more than #72's own checklist:
- **Module id selection** - `server/lib/slugify.js` is a naive placeholder (lowercase,
  non-alnum → `_`). Layer B's hook needs a concrete target directory *before* `query()`
  runs, so something outside the LLM call has to pick the id first; the agent's own
  structured-output `id` (§3, issue #73) is now asserted to match this slug, but does not
  replace it as the pre-agent directory choice - that chicken-and-egg constraint doesn't
  go away with structured output, it just gets a consistency check.
- **The two validators (§4, #74/#75)** are not called. `server/validators/structural.js`
  and `render.js` predate this issue and are fakes (a `content.includes('id:')`-style
  check and an unconditional `return true`, respectively, left over from an earlier
  prototype commit) - `scaffold.js` does not import them, since calling a fake validator
  would give false confidence rather than none. Gating the merge on real validators is
  #74/#75's job.
- **No live end-to-end run.** A real Agent SDK call needs a live `ANTHROPIC_API_KEY`, costs
  tokens, and (on success) merges a git commit into `main` - too high a blast radius to run
  unprompted in an assistant session. `server/test/scaffold.test.js` exercises the full
  pipeline (worktree create → agent → commit-and-merge → cleanup, and the escape-attempt /
  SDK-error abort paths) against a disposable scratch git repo with an injected mock
  `queryFn`; `server/test/preToolUseHook.test.js` proves Layer B's "escape attempts fail
  closed" guarantee directly, independent of whether a real LLM call ever ran. An actual
  `node server/scaffold.js` run against this repo is a manual follow-up, same as every other
  "verified live post-deployment" gate in this repo (`/telegram`, Nango, Kite, WhatsApp).

---

## 3. Structured output - schema-validated manifest with auto-retry

The SDK does natively what would otherwise be a hand-rolled ajv + retry loop:
```ts
options.outputFormat = { type: "json_schema", schema };  // schema from Zod: z.toJSONSchema(ModuleManifest)
```
The SDK **validates the output and re-prompts on mismatch**. On success the result carries `structured_output`; on exhaustion `subtype === "error_max_structured_output_retries"`.
Define the manifest schema in **Zod** (one source of truth), end with `ModuleManifest.safeParse(structured_output)` for end-to-end type safety.
The agent emits the manifest summary (entityTypes/attrs/views/botCommands/agentTools ids) as structured output → it becomes the input to Validator 1 without re-reading files.

**Implemented (issue #73):** `server/lib/moduleManifest.js` defines `ModuleManifest` in Zod
(entityTypes with typed attrs, views, botCommands, agentTools, plus top-level id/name/icon/
color) and derives `moduleManifestJsonSchema` via `z.toJSONSchema(ModuleManifest)`.
`server/scaffold.js` sets `options.outputFormat = { type: "json_schema", schema:
moduleManifestJsonSchema }` on the `query()` call, then on a `success` result runs
`ModuleManifest.safeParse(result.structured_output)` - a validation failure, or a manifest
`id` that disagrees with the pre-agent directory slug (§2's note above), aborts the build
and discards the worktree, same failure path as a hook denial. The SDK's own
`error_max_structured_output_retries` result subtype is treated as a hard failure, not
retried again at this layer - the SDK already exhausted its retry budget.
`server/test/moduleManifest.test.js` proves the schema's accept/reject boundary directly;
`server/test/scaffold.test.js` adds cases for an invalid manifest, an id mismatch, and
retry exhaustion, alongside #72's existing happy-path/escape-attempt/SDK-error cases - all
still against a mocked `queryFn` and scratch git repo, no live API key (same rationale as
#72's note above). The parsed `ModuleManifest` is now part of `scaffoldModule`'s success
return value, ready for Validator 1 (#74) to consume without re-reading `module.js`.

**Live-run bug found and fixed (issues #79/#80):** the first actual live `scaffoldModule()`
run (Claude Code's own CLI-subprocess auth, no `ANTHROPIC_API_KEY`, both Haiku 4.5 and
Sonnet tested) surfaced a real defect this mocked test suite couldn't see: `z.toJSONSchema()`
emits a top-level `$schema` meta-key by default, and the Claude CLI's `--json-schema` flag
silently rejects a schema carrying that key - the model never sees a valid structured-output
tool to call, so it answers in prose/a markdown code fence instead, and `structured_output`
never populates even though the SDK reports `subtype: "success", terminal_reason: "completed"`
(no error, no denial - `runAgent()` was also missing a `terminal_reason` check for the
adjacent case of a run stopped by a hook/permission/sandbox boundary, added at the same time).
Root-caused by testing progressively simpler schemas directly against the bare `claude` CLI
until removing `$schema` alone fixed it, confirmed on both models. Fixed in
`server/lib/moduleManifest.js` by stripping `$schema` before exporting
`moduleManifestJsonSchema`. Once fixed, a live run against the literal prompt "add a reading
module" got past structured output cleanly and correctly failed at Validator 1 (#74) instead:
`entity type "article" is already used by module "reading"` - the duplicate-type-id guard
doing exactly its job, since `modules/reading/module.js` already exists as a hand-built day-1
module. That's a self-collision inherent to #79/#80's literal prompts (asking the builder to
reproduce a module that's already there), not a defect in the builder - see the issue closing
comments for the full reasoning.

---

## 4. The two validators

**Validator 1 - structural (pure Node, no LLM):**
- Load the written `module.js` in a `vm`/worker, capture the `osRegisterModule({...})` argument.
- ajv-check against `module.schema.json`.
- Assert: schema-valid; no duplicate `type` ids across existing modules (query the registry); every `view.type` / ref resolves to a known core renderer; every `botCommand`/`agentTool` id unique.
- Fail → discard the worktree.

**Implemented (issue #74):** `server/lib/loadManifest.js` runs `module.js` in a fresh
`vm.createContext` that only exposes a capturing `osRegisterModule` stub - real file-system/
network globals aren't in scope, so a malformed or even hostile `module.js` can't do
anything but populate the captured object (or throw, which is caught and reported, not
crashed on). `server/validators/module.schema.json` + `server/validators/structural.js`
(rewritten - the earlier version was a `content.includes('id:')`-style stub left over from
an earlier prototype commit) run that captured object through ajv (`ajv`'s 2020-12 draft
build), then two checks ajv alone can't express: **duplicate entity-type ids** against every
sibling directory under `modules/` (skipping `_template` and the module's own directory,
matched by directory name - not by the manifest's own `id` field, which is exactly the field
under test) and **dangling view refs** (`view.type` must be a declared `entityTypes` key,
`view.kind` must be one of the core renderers `ModuleManifestPage.jsx` actually mounts -
`list/table/board/calendar/gallery/timeline/map/metric` - and a `metric`-kind view's
`view.metric` must resolve in `manifest.metrics`). A third check, not in the issue's literal
checklist but a natural corollary of "no duplicate type ids" using the same directory-name
signal: **the manifest's own `id` must equal its directory name** - without it, a mismatched
`id` silently can't be excluded from its own dup-check pass. `agentTool`/`botCommand` id
uniqueness (this section's last clause) is not yet asserted - out of #74's own checklist,
left for a follow-up alongside real cross-module registry queries.
`scaffold.js` now calls this validator on the file the agent actually wrote (not the §3
structured-output summary) before `commitAndMerge` - a structural failure aborts the build
exactly like a Layer B hook denial, discarding the worktree with nothing merged to `main`.
Spot-checked against all 14 real `modules/*/module.js` files: 13 pass; `modules/learning`
correctly fails on a genuine pre-existing `kind: "graph"` view (no `GenericGraph` renderer
exists in `frontend/src/core/renderers/`) - fixing that module is out of this issue's scope,
noted here as a known finding.

**Validator 2 - render smoke (headless Playwright):**
- Boot the app against a **scratch derived/replica DB** (never canonical `lifeos.db`), on an **ephemeral port**.
- Mount the new tile; assert **0 console/page JS errors** for the full session (`page.on('console'|'pageerror')`); assert each declared view mounts a node; assert an app-emitted **`module-mounted:<id>`** ready event (not arbitrary timeouts).
- **One bounded retry** before declaring failure (a single transient render error shouldn't burn a valid module).
- Fail → discard the worktree.

**Implemented (issue #75):** `server/lib/appBoot.js` boots the real stack as disposable child
processes on ephemeral ports (`server/lib/appBoot.js`'s own `getEphemeralPort()`) - the real
`lifeos-api` debug binary with `LIFEOS_DB_PATH`/`LIFEOS_DERIVED_DB_PATH` pointed at a fresh
`mkdtemp` scratch directory and `TURSO_URL`/`TURSO_TOKEN` cleared (never syncs, never touches
`lifeos.db`), and the real Vite dev server (`--host 127.0.0.1` pinned explicitly - Vite's
default `localhost` bind resolved to the IPv6 loopback first on the machine this was built on,
which then refused the IPv4 readiness poll). `server/validators/render.js` (rewritten - the
earlier version was an unconditional `return true` stub) launches headless Chromium, sets the
SPA's `life_os_loggedin` localStorage flag via `context.addInitScript` to skip the login gate,
collects `console`/`pageerror` for the full page session, `POST /api/event` seeds the exact
`module.installed` event the real self-extension install path (§1 step 5) emits (no auth
needed - `/api/event` falls back to the default workspace with no bearer token, same as the
frontend does), and races the real `module-mounted:<id>` `CustomEvent`
(`frontend/src/lib/moduleRegistry.js`) against a first-error signal and a bounded timeout - not
an arbitrary sleep, the timeout is only the safety net for a event that genuinely never fires.
One bounded retry (`MAX_ATTEMPTS = 2`) on any failure; teardown (browser, both child processes,
scratch DB dir) always runs, even on failure. `scaffold.js` calls this after Validator 1 passes
and before `commitAndMerge`.
**Scope note (closed by issue #121, docs/SELF-EXTENSION-V2.md §6):** the hot-install path used to
carry only a minimal `{id, name, version, icon}` manifest through the real SSE event, so
`InstalledModulePage.jsx` rendered every hot-installed module as a flat list instead of through
the full multi-view `ModuleManifestPage` the 14 static day-1 modules get. `scaffold.js` now
persists the real object-shaped manifest (module.js's own `osRegisterModule({...})` argument) as
a generic entity - `module: 'system'`, `type: 'module_manifest'`, upserted by matching on a
`title: module_manifest_<id>` key since `POST /api/entity` always server-generates its own row
id (`server/lib/manifestEntity.js`). `useModuleStream.js` fetches that entity on `module.installed`
and `InstalledModulePage.jsx` mounts `ModuleManifestPage` once it has real `entityTypes`/`views`,
falling back to `GenericList` only for a genuinely manifest-less module. This validator now
asserts "each declared view mounts a node" for real too: it loads the manifest from
`opts.modulePath` via `loadManifestFromFile`, seeds the same manifest entity, then clicks each
view's `[data-view-tab="<id>"]` and asserts `[data-view-id="<id>"]` is attached to the DOM
(`ModuleManifestPage.jsx`'s view switcher and mounted-view container).
Tested via `server/test/renderSmoke.test.js` (real Playwright + real HTTP fixtures standing in
for the heavy cargo/Vite boot, so the suite stays fast: happy path, retry-then-succeed,
retry-exhausted, timeout-without-firing, teardown-always-runs) and
`server/test/appBoot.test.js` (`getEphemeralPort` returns distinct, bindable ports).
`server/scripts/renderSmokeLive.js` is a manual entry point that runs the real stack end-to-end
(not part of the vitest suite) - unlike #72's Agent SDK constraint, this needed no paid API key
and no git mutation, so it was actually run live in this session against the real `reading`
module id and passed (`{"valid":true,"errors":[]}`), confirming the full boot → mount →
assert → teardown pipeline works, not just the mocked orchestration tests. Requires
`cargo build --bin lifeos-api` and `npm install` in `frontend/` to have been run first, plus a
one-time `npx playwright install chromium` (documented in `docs/MANUAL-SETUP.md`).

---

## 5. Isolation & commit (use Claude Code's own worktree feature)

Pipeline: create worktree `.claude/worktrees/scaffold-<id>` on a fresh branch → `query()` with Layers A/B/C → Validator 1 → Validator 2 → if both green, `git commit` in the worktree and merge to main (revertable single commit) → SSE push the new tile → remove worktree.
Any failure: remove the worktree; nothing touches main.

---

## 6. Reuse & risk

- **Port from** `anthropics/claude-agent-sdk-demos` (canonical `query()` + hooks + structured-output wiring). SDK repos: `claude-agent-sdk-typescript`, `…-python`. OS sandbox primitives standalone as `@anthropic-ai/sandbox-runtime`.
- Reuse `knowledge-atlas/tools/server.js` + `memory.js` as the app-boot/DB harness Validator 2 drives.
- **Biggest reliability risk: Validator 2 (render smoke) flakiness**, not the LLM. Mitigate with the ephemeral-port + fresh-scratch-DB + explicit ready-event + full-session error capture + one bounded retry already specified above. Keep the SDK's structured-output retry (free) separate from the render retry.

---

## 7. Marketplace tie-in
The same validated, signed manifests are the unit the **module marketplace** distributes; an install runs the *same two validators* locally before register. See [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §4.
