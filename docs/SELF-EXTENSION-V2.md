# Self-extension v2 - from manifest emitter to system generator

> Today the in-app AI can generate exactly one thing: a declarative module manifest (`osRegisterModule({...})` in a single file, choosing from 8 fixed renderer kinds). That is the *floor*, not the ceiling.
> This spec turns self-extension into a **capability ladder**: on request, the agent climbs as many rungs as the ask needs - manifest → new view/renderer → new agent tool → new backend route/pipeline → additive migration → **whole new subsystem** - where **each rung is a separate sandboxed write-scope with its own validator gate, and every rung ends in a revertable git commit.**

> Invariant alignment: this **extends, never relaxes**, [SELF-EXTENSION.md](./SELF-EXTENSION.md) (which is Tier 0 of this ladder), [SECURITY.md](./SECURITY.md), and [AGENT-CONTROL.md](./AGENT-CONTROL.md). Codegen still runs **only on the trusted Mac**; the cloud bot still only enqueues. The four protected domains are never generable at any tier (§5).

**Status: design spec, pre-implementation.** Tier 0 is built and battle-tested (see [SELF-EXTENSION.md](./SELF-EXTENSION.md), issues #72-#80); Tiers 1-5 and the pipeline below are the design for the next round. "Implemented (issue #N)" annotations get added as each tier lands.

---

## 1. The reframe

The reason self-extension is manifest-only is **not** a model limitation - it is a deliberate sandbox contract. `server/scaffold.js:40-47` instructs the agent to *"edit `modules/<id>/module.js` … keep it a single `osRegisterModule({...})` call … only edit files under `modules/<id>/`,"* and three defense-in-depth layers (tool allowlist, the `preToolUseHook.js` path guard, the Seatbelt sandbox) **enforce** it so hard the agent cannot write a React renderer, a backend route, a migration, or anything outside that one directory even if it tried ([SELF-EXTENSION.md](./SELF-EXTENSION.md) §2). It picks from a **closed set of 8 renderer kinds** (`frontend/src/core/renderers/`) and cannot invent a 9th - which is precisely why `modules/learning` fails its own structural validator: it declares a `kind:"graph"` view and no `GenericGraph` exists, and the agent is forbidden from writing one.

The insight: **that contract is not a wall, it is the first rung of a ladder.** The safety machinery is already parameterized in spirit - a per-write-scope allowlist, a path-confinement hook, an OS sandbox, a validator, a git commit. v2 keeps all of it and simply lets the *scope* and *validator* vary by tier, instead of hardcoding `modules/<id>/` and one manifest schema. "Generate an entire system" = the agent climbs several rungs in one build, each independently sandboxed, validated, and committed.

---

## 2. The generation ladder

| Tier | The agent may write (its sandboxed scope) | Validator gate (§9) | Human gate | Example request |
|---|---|---|---|---|
| **T0 Manifest** *(built today)* | `modules/<id>/module.js` (one `osRegisterModule` call) | structural + render-smoke | none (reversible commit) | "add a habit tracker" |
| **T1 View / renderer** | a new `frontend/src/core/renderers/Generic<Kind>.jsx` + its registration | render-smoke + visual-diff + a11y lint | none | "show my topics as a **graph**" *(fixes the `learning` bug)* |
| **T2 Capability (agent tool)** | ONE pure request-descriptor file, `server/agent/tools/generated/<name>.js` | tool-contract test + scratch-DB dry-run | none (tool is gated at *use*, not creation) | "give the AI a tool to compute my trading R-multiple" |
| **T3 Backend route / pipeline** | a `services/lifeos-*` route **or** a `lifeos-pipelines` DAG stage | integration test vs scratch DB + **eval-gate** | **Telegram approve** | "add an endpoint that summarizes my week" |
| **T4 Migration / derived** | an **additive** `GENERATED … VIRTUAL` column or a `lifeos-derived.db` index | migration-apply-on-scratch + rebuild proof + no-rewrite assert | **Telegram approve** | "make 'due date' a fast-queryable field" |
| **T5 Subsystem** | a whole new crate/service scaffold, multi-file across scopes | full `cargo build` + test suite green + **eval-gate** | **Telegram approve** + explicit confirm | "build me a finance module with its own ingest pipeline" |

Tiers compose: a real "add a finance module" build might emit **T0** (the manifest) + **T1** (a runway-chart renderer) + **T4** (a `GENERATED` column for `amount`) in one pipeline run, three commits, three validators - a genuine subsystem, assembled rung by rung.

Zero-migration growth still holds for ~90% of requests: most land at **T0** and need no code at all ([DATA-MODEL.md](./DATA-MODEL.md) §4.4). The ladder exists for the 10% that genuinely need new code - it does not lower the bar for the common case.

---

## 3. Per-tier scope, sandbox, and gate - defense-in-depth, generalized

The three layers from [SELF-EXTENSION.md](./SELF-EXTENSION.md) §2 are **parameterized by tier**, not rewritten. Nothing about the guarantee weakens; the *shape* of the allowed scope is what varies.

- **Layer A - locked tool surface.** Same `allowedTools`/`disallowedTools`/`permissionMode:"dontAsk"`; unchanged across tiers.
- **Layer B - PreToolUse path hook.** Today it hardcodes `targetModuleDir`. v2 passes a **per-tier allowlist of path globs** (T1 → `frontend/src/core/renderers/**`; T3 → the specific `services/<crate>/src/routes/**` or `lifeos-pipelines/**`; T5 → the new crate dir + its `Cargo.toml` workspace entry). Any write resolving outside the tier's globs **fails closed**, exactly as today. Crucially, the allowlist is a *whitelist*, and the protected surfaces (§5) are never in any tier's whitelist.
- **Layer C - Seatbelt sandbox.** `filesystem.allowWrite` is set to the tier's scope; `failIfUnavailable:true` unchanged. Bash children stay physically confined; credentials/env still denied.
- **Human gate.** T0-T2 are internal + reversible → auto-commit (a `git revert` away). T3-T5 are higher-blast-radius → **draft → Telegram/PWA approve → commit**, reusing the [SECURITY.md](./SECURITY.md) §2 gating state machine. This mirrors "outward or irreversible actions are human-gated" - generating a backend route or a whole crate is treated with the same seriousness as an outward action.

**Implemented (issue #131):** `server/lib/tierScopes.js` is the single source of truth - `TIER_SCOPES` maps each of T0-T5 to a function of build params returning the tier's repo-relative write globs (T0 `modules/<id>/**`; T1 the one `Generic<Kind>.jsx` + its `ModuleManifestPage.jsx` registration; T3 `services/<crate>/src/routes/**` or `lifeos-pipelines/src/**`; T4 a migration/derived index; T5 the new crate + `services/Cargo.toml`). `evaluateWrite`/`isWriteAllowed` resolve+normalize the path (rejecting `..`/absolute-outside/traversal), then apply **deny-wins**: a protected surface (§5) is denied before any tier allow is consulted. Layer B (`server/lib/preToolUseHook.js`) now takes `{tier, params, root}` and calls `evaluateWrite`, keeping a deprecated string-arg branch for the pre-v2 single-dir form. Layer C (`server/lib/sandbox.js`) takes the tier's top-level dirs from `scopeDirs(tier, params)`. Glob matching uses `picomatch` (no hand-rolled globbing).

**Implemented (issue #134) - T2's scope narrowed to one generated file:** `TIER_SCOPES.T2` is `({ name }) => [\`server/agent/tools/generated/${name}.js\`]` - the original design note above (a module's `agentTools` entry + a CLI wrapper) was superseded before any T2 code shipped, once the actual T2 contract (§9's tool-contract validator, below) settled on ONE self-contained pure-descriptor file per tool rather than editing a module manifest + a Rust CLI crate. This is a strictly tighter scope than either alternative: a T2 build can add exactly one new file and touch nothing else, including `server/agent/tools/generated/index.js` itself (the loader auto-discovers new files; a build is never allowed to edit the loader).

---

## 4. The build pipeline - spec → plan → build → validate → gate → commit

v2 replaces the single "edit one manifest" prompt with a pipeline driven by the [agent core](./AGENT-CORE.md) planner:

```
 POST /api/module-request { prompt, workspace_id }         (unchanged intake; bot still only enqueues)
        │
        ▼
 1. SPEC     — agent restates the request as a structured build spec (what entities/views/tools/routes it implies)
        │
        ▼
 2. PLAN     — planner (AGENT-CORE §3) decides which tiers the spec needs → emits a build DAG
        │       (e.g. [T0 manifest] → [T1 renderer] → [T4 column]); persisted, inspectable, resumable
        │
        ▼
 3. BUILD    — for each DAG node, in its own git worktree with the tier's scope (§3):
        │       agent writes only within scope → structured-output summary (Zod schema per tier)
        │
        ▼
 4. VALIDATE — the tier's validator (§9) runs; fail → discard that worktree, mark the node failed
        │
        ▼
 5. GATE     — T3+ nodes run the eval-gate (HARNESS-LOOP §2, sampled Haiku judge) + Telegram approval
        │
        ▼
 6. COMMIT   — each passing node = one conventional-commit merge to main; SSE hot-reload; events('module.installed'|'capability.installed'|…)
        │
        ▼
    Any node failing aborts its subtree; already-committed nodes stay (forward-only, each revertable). Partial success is surfaced honestly, never silently.
```

Every stage writes `events` (run_id, tier, node, outcome) so the whole build is visible in `harness observe` and replayable - the same auditability Tier 0 already has, extended across the DAG.

**Implemented (issue #132):** `server/build/` is the pipeline.
`spec.js` (`generateSpec`) restates the request as a Zod `BuildSpec`;
`plan.js` (`generateBuildPlan` + `validatePlan`) emits the DAG and
deterministically post-validates it - per-tier required params present,
plus a Kahn topological sort that rejects unknown deps and cycles (fail
closed before any worktree is created).
The DAG persists as a `pipelines`/`pipeline_run` entity
(`attrs.pipeline_id = "build:<runId>"`, `origin: "build"`, `nodes: [...]`),
the same route + shape the agent planner uses, PATCHed as the build
progresses (inspectable + resumable-by-inspection).
`node.js` builds one node per worktree: **T0 delegates to the existing
`scaffoldModule`** end to end (its worktree + structured manifest + all
three T0 validators + commit are reused, not reimplemented); T1-T5 get an
honest minimal build (Layer B hook + Layer C sandbox scoped by tier, a
per-tier prompt, a Zod `{ tier, files, summary }` structured output).
`validate.js` runs `getValidators(tier)` (§9) - a fail discards the
worktree, marks the node `failed`, and the orchestrator (`pipeline.js`)
skips its transitive dependents (`reason: "dependency failed"`).
`commit.js` merges each passing node as one conventional commit
(`feat: <tier> <description> (build:<runId>)`) via the additively-extended
`commitAndMerge`, and emits `build.node.completed|failed|gated` +
`build.completed` events (stamped with `tier`/`outcome` like the agent
loop's run-log rows). `pipeline.js::runBuildPipeline(request, workspaceId,
opts)` is the orchestrator (DI: `queryFn`/`httpFn`/`validateFn`), sequential
in topo order; `run.js` is the CLI (`node build/run.js <prompt>
<workspaceId>`, same last-JSON-line stdout contract as `scaffold.js`).
Partial success is honest: committed nodes STAY, and `{ success, runId,
nodes: [{ id, tier, status, commit? }], summary }` never reports a
failed/skipped/gated node as installed.

---

## 5. The four never-generable surfaces (fail-closed at every tier)

The generation ladder is broad, so its floor must be explicit and hard. The generator's per-tier allowlist (§3, Layer B) **never** includes, and the validators **reject any build touching**, the [AGENT-CONTROL.md](./AGENT-CONTROL.md) §1 protected domains:

| Never generable | Why | Enforcement |
|---|---|---|
| **Security & gating config** (capability matrix, sandbox config, allow/deny lists) | The boundary cannot be edited by the thing it constrains. | not in any tier's write-glob; validator hard-rejects a diff touching it |
| **`broker-guard`** and any order path | Trading stays read-only for every agent; no order tool may ever be generated. | `broker-guard`'s crate is outside all write-scopes; a T2 "trading tool" build that emits an order call fails the tool-contract validator |
| **OAuth / connections / secrets** (`connections.secret_enc`, Nango config) | Tokens are the keys to real accounts. | outside all scopes; secrets never enter agent context ([SECURITY.md](./SECURITY.md) §1) |
| **VCS internals** (history rewrite/GC) | History is the audit source of truth. | forward-only commits only; no tier writes to `lifeos-vcs` internals |

This is the one place the ladder is a strict deny-list: the agent's generative reach is broad by default and narrowed only at these four cut-lines, each mapping to a fail-closed guard - the exact inverse-of-deny-list principle the Agent Control Plane already uses for actuation, now applied to codegen.

**Implemented (issue #134) - T2's route allowlist is the fourth enforcement layer for a self-authored tool.** A T2 tool cannot write a file outside its own scope (§3), but it authors a `request()` function that decides which *lifeos-api route* the tool calls at runtime - a second, orthogonal surface the four surfaces above don't cover by file path alone. `server/lib/routeAllowlist.js`'s `isRouteAllowed(method, path)` is a single shared **whitelist** (GET/POST `/api/entity*`, `/api/edge*`, GET `/api/search`, POST `/api/memory/recall`, POST `/api/event`, POST `/api/browser/scrape` - nothing else, so `configs`/`module-request`/`jobs`/`llm`/`agent`/`whatsapp`/`storage`/`travel`/`notion`/`connections`/anything with `order`/`broker` are denied by omission, not by a growing deny-list) enforced at **three** points: the loader (`server/agent/tools/generated/index.js`) dry-runs `request()` against the tool's own `example` args at load time and refuses to install a tool whose route fails; the T2 build validator (`server/validators/t2Tool.js`) re-proves the same dry-run before a build ever commits; and the executor (`server/agent/executor.js`) re-validates the ACTUAL runtime path on every call, since a tool's route can be a function of the model's live args (not just its author's `example`) - a "trading tool" whose `request()` computes an order path from caller-supplied args fails at the very moment it would matter, not just at its one-time load check.

**Implemented (issue #131):** `PROTECTED_SURFACES` in `server/lib/tierScopes.js` is the authoritative glob deny-list, enforced at **two** layers and **every** tier. (1) Layer B never lets a write land on a protected path (deny-wins over any tier allow). (2) `protectedSurfaceValidator` (`server/validators/registry.js`) `git diff`s the build (`base...HEAD` ∪ `status --porcelain -uall`) and hard-rejects if any changed path matches - failing closed if the diff can't be inspected. The list covers all four domains plus self-protection: security/gating config + capability matrix + action registry (`server/lib/sandbox.js`, `preToolUseHook.js`, `tierScopes.js`, `server/validators/**`, `server/agent/actionRegistry.js`, `frontend/src/lib/{capabilities,capabilityMatrix,agentActions,actionPlanCompiler}.js`); `broker-guard` + order-execution paths (`**/broker-guard*`, `**/*order*.rs`, `**/orders/**`); OAuth/connections/secrets (`infra/nango/**`, `migrations/0002_control_plane.sql`, `migrations/0011_workspace_envelope_key.sql`); VCS internals (`services/lifeos-vcs/**`); and git/CI/harness config (`.git/**`, `.github/**`, `.claude/**`).

---

## 6. Runtime-consumption fix (ship this first, standalone)

Independent of the ladder, there was a live bug that made even **Tier 0** half-invisible: the SSE `module.installed` event carried only `{id, name, version, icon}`, so `frontend/src/pages/InstalledModulePage.jsx` rendered every hot-installed module as a **flat `GenericList`**, ignoring the `views`/`board`/`calendar` the agent declared (confirmed in `server/validators/render.js` lines 6-17 and the render-smoke scope note in [SELF-EXTENSION.md](./SELF-EXTENSION.md) §4). The rich manifest was validated, git-committed, and then **unused at runtime.**

**Implemented (issue #121):** `scaffold.js` persists the real manifest as a generic entity (`module: 'system'`, `type: 'module_manifest'`, `server/lib/manifestEntity.js`) right after Validator 1 passes; the frontend fetches it by id on `module.installed` (`frontend/src/lib/manifestApi.js`'s `fetchInstalledManifest`, wired into `useModuleStream.js` and `InstalledModulePage.jsx`) and mounts the real `ModuleManifestPage` - the same multi-view renderer the 14 static day-1 modules get - once the manifest carries real `entityTypes`/`views`, falling back to `GenericList` only for a genuinely manifest-less module. This also let render-smoke (Validator 2) finally assert *each declared view mounts a node* (`[data-view-tab]`/`[data-view-id]`), closing the scope gap it explicitly flagged.

---

## 7. Multi-agent - scoped to T5 only

A T0-T3 build is a single-agent job. A **T5 subsystem** build (a whole crate + routes + tests) genuinely benefits from a split, and the Agent SDK supports subagents ([PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §1 already anticipates *"user/module agent pipelines (DAGs via the Agent SDK)"*):

- **supervisor** - owns the build DAG, sequences the subagents, decides done.
- **scaffolder** - writes the code within the tier's scope.
- **tester** - writes and runs the test suite against the scratch DB.
- **reviewer** - a maker-checker adversarial pass before the eval-gate.

This is a bounded supervisor+3 pattern, **not** Founder OS's 12-path swarm - the swarm is deliberately declined ([AGENT-CORE.md](./AGENT-CORE.md) §11). Multi-agent is a T5 tool, not a default.

---

## 8. Human gate + revertability (unchanged guarantees, wider surface)

- **Every rung is a git commit** → one `git revert` away, exactly as Tier 0 promises today.
- **T3+ require Telegram/PWA approval** before commit, reusing the existing gating state machine and inline approve/deny buttons.
- **Eval-gate on T3/T5** blocks a low-quality ship and posts the judge's rationale to Telegram ([HARNESS-LOOP.md](./HARNESS-LOOP.md) §2).
- **Offline path unchanged:** `/addmodule` while the Mac is off still enqueues to `module_requests`; the `lifeos-drain` poller runs the identical local pipeline on wake and notifies ([SELF-EXTENSION.md](./SELF-EXTENSION.md) §1b) - the ladder does not change the cloud-only-enqueues invariant.

**Implemented (issue #132) - approval-only gate boundary:** `server/build/gate.js`
gates every T3+ node that passed validation by creating a
`pipelines`/`pending_approval` entity linked to the run row, emitting
`build.node.gated`, and halting the node `awaiting_approval` (it is never
committed). This is **APPROVAL-only today**: the sampled Haiku eval-judge
(HARNESS-LOOP.md §2) lives only inside the Rust pipeline runner
(`services/lifeos-pipelines`) with no callable surface from this JS
pipeline - the same "wiring only" boundary #125 hit - so the judge score is
not consulted at the gate here. **Resume-on-approval** (re-building the
node's worktree and committing it once a human approves) is deferred to a
follow-up; the gated node's worktree is discarded, and the absence of a
commit is the safe default.

---

## 9. Validator registry (per-tier gates)

Today's two validators (`server/validators/structural.js`, `render.js`) become a **registry keyed by tier**; the build pipeline (§4 step 4) dispatches to the right one per DAG node:

| Tier | Validator |
|---|---|
| T0 | structural (schema, no dup type-ids, view-refs resolve) + render-smoke |
| T1 | render-smoke of the new renderer against a scratch DB + **visual-diff** against a baseline + a11y lint (0 console errors, keyboard-reachable) |
| T2 | tool-contract test (import/usage posture scan, schema round-trip, dry-run `request()` against the shared route allowlist, **rejects any protected-domain call**) - **implemented (issue #134)** |
| T3 | integration test of the route/stage vs scratch DB (never `lifeos.db`) + eval-gate |
| T4 | apply the migration on a scratch copy, assert it is **additive** (`PRAGMA` before/after, no table rewrite), rebuild `lifeos-derived` and prove it converges |
| T5 | `cargo build` the new crate + its full test suite green + eval-gate + reviewer sign-off (§7) |

Every validator keeps Tier 0's discipline: run in a disposable worktree against a scratch/derived DB, fail → discard, nothing touches `main` or `lifeos.db`. The biggest reliability risk stays render-flakiness, mitigated as in [SELF-EXTENSION.md](./SELF-EXTENSION.md) §6 (ephemeral ports, scratch DB, explicit ready-event, one bounded retry).

**Implemented (issue #131):** `getValidators(tier)` in `server/validators/registry.js` returns the ordered validator list per tier. `protectedSurfaceValidator` (§5) runs **first at every tier**; T0 then adds the existing `structural` + `renderSmoke` entries (`scaffold.js` now dispatches through the registry instead of importing them directly). T1-T5 carry the protected-surface gate plus a placeholder that **fails closed** (`validator not yet implemented for <tier>`) until each tier's real validators land with its generator - an unvalidatable tier can never pass. An unknown tier resolves to the rejecting placeholder alone.

**Implemented (issue #133):** T1 is the first tier past T0 to get a real generator + validator, proving the ladder generalizes past the manifest-only builder. `frontend/src/core/rendererKinds.js` is a new plain-JS (no JSX/React) module exporting `RENDERER_KINDS` - the single source of truth for every kind a `Generic<Kind>.jsx` exists for. `ModuleManifestPage.jsx`'s `KIND_RENDERERS` component map stays separate (it needs the React imports rendererKinds.js deliberately avoids), guarded against drift by a unit test asserting the two key sets match. `server/validators/structural.js` now imports `RENDERER_KINDS` directly via a relative ESM import across the frontend/server package boundary (plain Node resolves it fine, no bundler needed) instead of a hardcoded `KNOWN_VIEW_KINDS` list, and patches the compiled ajv schema's `view.kind` enum from it at validate time - `modules/learning`'s pre-existing `kind:"graph"` view (docs/SELF-EXTENSION.md §4's noted finding) now passes. `frontend/src/core/renderers/GenericGraph.jsx` is the T1 proof artifact: a node-link SVG graph (deterministic radial layout, no `Math.random`/`Date.now`), fetching `GET /api/edge` and filtering client-side to edges between the view's own entities (the real edge route has no per-module scoping), with every node keyboard-focusable (`tabIndex=0`, `role="button"`, `aria-label`, Enter/Space activates via the same handler as click). `server/validators/t1Render.js` is the new T1-specific validator (`getValidators('T1')` now returns `[protectedSurfaceValidator, t1RenderValidator]`, no longer the placeholder): it statically checks the renderer file exists and is registered in both `KIND_RENDERERS` and `RENDERER_KINDS`, then reuses `render.js`'s boot pattern (real `launchApi`/`launchFrontend`/Playwright in production, fully faked in `t1Render.test.js`) to mount a synthetic one-view manifest and assert 0 console/page errors, an a11y lint (a focusable element exists inside the mounted view and genuinely accepts `.focus()`), and a visual baseline. `server/lib/tierScopes.js`'s T1 scope gained one deliberate third glob, `frontend/src/core/rendererKinds.js`, alongside the original `Generic<Kind>.jsx` + `ModuleManifestPage.jsx` pair. `server/build/node.js`'s `TIER_PROMPTS.T1` is now a real prompt (not the generic tier-role template) naming the sibling props contract, the edge-fetch pattern, and both registrations the agent must make.

**Known limitation, honestly documented:** the visual baseline in `t1Render.js` is a manual dimension-free byte-size-ratio comparison against a saved PNG (bootstraps on first run), not a real perceptual pixel diff - this repo takes zero new npm dependencies, and no pixelmatch-equivalent is already vendored. It catches a blank/broken render or a wildly different layout, but not a subtle color or spacing regression within the same byte budget. A real perceptual diff library, if ever justified, is a follow-up.

**Implemented (issue #135):** T3 is the third tier past T0 to get a real generator + validator - a single generated **backend route**, higher blast radius than T0-T2 so it is the first tier this doc's approval-only gate (§8) actually exercises end to end with a real validator behind it. `server/lib/tierScopes.js`'s `TIER_SCOPES.T3` narrowed from the original `services/<crate>/src/routes/**` glob to the three concrete files a T3 build may ever touch - `services/<crate>/src/routes/<name>.rs` (the new route), `services/<crate>/src/routes/mod.rs` (its registration - unavoidable, so it stays in scope), and `services/<crate>/tests/<name>_integration.rs` (the route's own integration test) - dropping the `services/lifeos-pipelines/src/**` DAG-stage half of the original design note in favor of the tighter, single-purpose scope this issue specifies; a future issue can add a distinct pipeline-stage tier if that need materializes. `server/build/plan.js`'s `REQUIRED_PARAMS.T3` is now `["crate", "name"]` (previously `["crate"]` alone) since the write-scope needs both to resolve. `server/build/node.js`'s `TIER_PROMPTS.T3` is a real prompt: author the route following the crate's existing axum style (`State(state): State<AppState>`, workspace resolution, `ApiResult<Json<...>>`), register it ADDITIVELY in `mod.rs` under a `// --- generated (T3) ---` banner, and write the integration test against a scratch DB using the crate's own established `Config`-literal + `std::env::temp_dir()` pattern (never the real `lifeos.db`) - the prompt explicitly forbids a route that performs an outward effect directly, requiring a `pending_approval` draft instead (SECURITY.md §2), reinforcing (not replacing) the human gate below. `server/validators/t3Route.js` (`getValidators('T3')` now returns `[protectedSurfaceValidator, t3RouteValidator]`, no longer the placeholder) is the build-time gate: a scope check (only the three allowed paths may have changed - git `status --porcelain` is the build's full diff since nothing is committed yet at validate time), a mod.rs additive-only diff check (a pure-addition unified diff never emits a body line starting with `-`; `git diff` and `git status` always shell the real `git`, never mocked), an integration-test scratch-DB check (the file must exist, must not reference `lifeos.db`/`~/`/a hardcoded `/Users/...` path, and must construct its DB via `temp_dir()`), and finally `cargo build -p <crate>`, `cargo test -p <crate> --test <name>_integration`, and `cargo clippy -p <crate> -- -D warnings` - the only three calls shelled through an injectable `opts.execFn` (threaded from `runBuildPipeline`'s `opts.execFn` through `server/build/validate.js`'s `ctx.execFn`), so vitest never runs real cargo. **Eval-gate boundary, honestly scoped:** as documented in §8's issue #132 note, the sampled Haiku judge has no callable surface from this JS pipeline, so T3's gate (`server/build/gate.js`, unchanged by this issue) remains approval-only - a validated T3 node creates a `pending_approval` entity and halts `awaiting_approval`, never auto-committing; `server/test/buildPipeline.test.js` extends the existing T3-is-gated test to build a real in-scope route + additive mod.rs + scratch-DB test and run it through the REAL `t3Route` validator (cargo mocked) before confirming the halt, and the pre-existing "T3 validator placeholder fails closed" test is retargeted to T4 (the next tier still awaiting its real validator).

**Implemented (issue #134):** T2 is the second tier past T0 to get a real generator + validator - a self-authored **agent tool** (Voyager-style `create_tool`, docs/AGENT-CORE.md §6), gated at *use* rather than at creation. A generated tool is ONE file, `server/agent/tools/generated/<name>.js`, exporting a default **pure request descriptor** - `{ name, description, classification: 'allowed'|'gated', inputSchema: <zod schema>, example, request: ({args, workspaceId}) => ({method, path, body?}) }` - never a handler that performs I/O itself; `server/agent/executor.js` remains the ONLY chokepoint that ever calls `fetch`/`httpFn` (capability check, ledger, retry, untrusted-wrapping), so generated code never owns the wire. `server/agent/tools/generated/index.js`'s `loadGeneratedTools()` is the lazy, failure-tolerant loader: it scans the directory, dynamic-imports each file, structurally validates the contract (`validateToolShape`, shared with the build validator so the shape check is defined exactly once), dry-runs `request()` against its own `example` and the shared `isRouteAllowed` (§5) allowlist, and skips - with a warning, never a crash - any file that fails any of those checks, including a name collision with `PROTECTED_TOOLS` or an already-registered tool (the loader itself rejects the shadow attempt). `server/agent/actionRegistry.js` merges `loadGeneratedTools()`'s output into `REGISTRY` at module-load time, so `classify()`, the executor's SDK tool builder, and Tool-RAG's digest (`server/agent/toolRag.js::indexTools`, §4) all pick up a new generated tool automatically - a new tool changes the registry's content digest, which triggers a re-embed, with zero new wiring beyond the merge itself. `server/validators/t2Tool.js` is the build-time gate (`getValidators('T2')` now returns `[protectedSurfaceValidator, t2ToolValidator]`, no longer the placeholder): it asserts exactly one new file exists at the expected path and loads cleanly, statically scans the source text for anything beyond a bare `import { z } from "zod"` (no other import/require, no raw `child_process`/`fs`/`net`/`http`/`https`/`fetch(`, no `process.env` reads, no dynamic `import(` - an AST-posture check done as a text scan, adding zero new deps), re-runs the same shape + example-round-trip + route dry-run the loader performs, and fails closed on any protected/order/broker/secret route target. `server/lib/routeAllowlist.js`'s `isRouteAllowed` is the single shared allowlist all three call sites (loader, validator, executor) import - defense-in-depth is duplicated call sites, never duplicated logic. Gating rides the EXISTING capability matrix unchanged: a T2 tool's own `classification` (`'allowed'` or `'gated'`) is what the executor checks at call time, exactly like a hand-written tool - `draft.create`-shaped gated behavior for a generated tool costs no new code. `server/lib/tierScopes.js`'s T2 scope and `server/build/node.js`'s `TIER_PROMPTS.T2` (a concrete R-multiple-over-trade-entities example) are described in §3 and §4 respectively.

---

## 10. Build surface & verification

- **Orchestration (JS):** extend `server/scaffold.js` into `server/build/` - the spec→plan→build pipeline, the per-tier scope config, the validator registry; reuses the [agent core](./AGENT-CORE.md) planner and Agent SDK wiring.
- **Backend (Rust):** `lifeos-pipelines` dispatches the build DAG (it already "shells to Agent SDK", [RUST-COMPONENTS.md](./RUST-COMPONENTS.md) §1); `lifeos-drain` claims `module_requests` and runs the pipeline (extends the existing `ScaffoldJsBuilder`); `lifeos-api` gains `capability.installed`/tier events on the SSE stream.
- **Frontend:** the §6 render fix; a build-progress view showing the DAG's per-node tier + validator status (reuse the action-ledger UI shape).
- **Must-pass checks:**
  - "Add a habit tracker" → T0 only, appears as its **full multi-view** layout (not a flat list), `git log` shows one commit.
  - "Show my topics as a graph" → T1 generates `GenericGraph.jsx`, render-smoke + visual-diff pass, the `learning` module finally validates.
  - "Give the AI a tool to compute R-multiple" → T2 tool built, gated at use, and a tool that tries to emit an order **fails validation**.
  - "Add a weekly-summary endpoint" → T3 route built, integration test green, **does not ship without Telegram approval + eval-gate**.
  - A build attempting to write security/gating config, `broker-guard`, a connection, or a secret **fails closed at Layer B**, at every tier.
  - A multi-tier "finance module with ingest" build runs its DAG, commits passing nodes, and surfaces any failed node honestly rather than a false "installed".
  - Offline `/addmodule` of a multi-tier build queues, drains on wake, and notifies with the true per-node outcome.

**Implemented (issue #132):** the JS orchestration landed in `server/build/`
(§4 note) reusing `scaffold.js`'s exports (T0 delegates to `scaffoldModule`;
`commitAndMerge` extended additively so `scaffold.test.js` stays green
unmodified). On the Rust side, `lifeos-drain`'s `ScaffoldJsBuilder` now spawns
`node build/run.js` for a claimed `module_requests` row, gated by
`LIFEOS_BUILD_PIPELINE` (default on; `0` falls back to plain `scaffold.js`);
its last-JSON-line parse tolerates both the scaffold `{moduleId}` and the
pipeline `{runId, nodes, summary}` shapes, and a success still calls the
existing `complete_module_request`. Still deferred: the T1-T5 generators and
their real validators (they fail closed until #133+), the eval-judge at the
gate and resume-on-approval (§8 note), `lifeos-pipelines` dispatching the DAG
itself, and the frontend build-progress view.

---

## 11. Reuse & risk

**Reuse:** everything Tier 0 already proved - Agent SDK `query()` + hooks + Zod structured output, the 3-layer sandbox, the worktree-commit flow, both existing validators, the `module_requests` state machine and offline drain; plus the [agent core](./AGENT-CORE.md) planner, the eval-gate and event store ([HARNESS-LOOP.md](./HARNESS-LOOP.md)), and the capability matrix's protected-domain guards ([AGENT-CONTROL.md](./AGENT-CONTROL.md)).

**Risks & mitigations:** (1) **scope-escape at higher tiers** - a T5 build touches many files, so Layer B's whitelist must be tight and validator-enforced, and §5's four surfaces are hard-excluded; (2) **validator flakiness** compounds across a DAG - keep each node's validator isolated and retried independently, never share scratch state; (3) **cost** - a T5 build is many model calls, so gate it behind explicit human confirm and the spend budget ([AGENT-CORE.md](./AGENT-CORE.md) §3); (4) **partial builds** - the DAG must leave the repo in a coherent, committed-or-discarded state per node, never half-written, and report partial success honestly.

---

## 12. Relationship to Tier 0 and the marketplace

[SELF-EXTENSION.md](./SELF-EXTENSION.md) remains the authoritative spec for **Tier 0** (the manifest builder) and its issue history; this doc is the superset that places T0 as rung one and specifies T1-T5. The same signed, validated artifacts each tier produces are the unit the **module marketplace** ([PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §4) distributes - an install re-runs the tier's validator locally before register, so a downloaded T5 subsystem is verified on the installing machine exactly as a locally-built one is.
