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
| **T2 Capability (agent tool)** | a thin tool: `bin/lifeos`/`~/.claude/bin` wrapper + an `agentTools` entry + JSON schema | tool-contract test + scratch-DB dry-run | none (tool is gated at *use*, not creation) | "give the AI a tool to compute my trading R-multiple" |
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

**Implemented (issue #131):** `server/lib/tierScopes.js` is the single source of truth - `TIER_SCOPES` maps each of T0-T5 to a function of build params returning the tier's repo-relative write globs (T0 `modules/<id>/**`; T1 the one `Generic<Kind>.jsx` + its `ModuleManifestPage.jsx` registration; T2 the module's `agentTools` entry + the CLI wrapper; T3 `services/<crate>/src/routes/**` or `lifeos-pipelines/src/**`; T4 a migration/derived index; T5 the new crate + `services/Cargo.toml`). `evaluateWrite`/`isWriteAllowed` resolve+normalize the path (rejecting `..`/absolute-outside/traversal), then apply **deny-wins**: a protected surface (§5) is denied before any tier allow is consulted. Layer B (`server/lib/preToolUseHook.js`) now takes `{tier, params, root}` and calls `evaluateWrite`, keeping a deprecated string-arg branch for the pre-v2 single-dir form. Layer C (`server/lib/sandbox.js`) takes the tier's top-level dirs from `scopeDirs(tier, params)`. Glob matching uses `picomatch` (no hand-rolled globbing).

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

---

## 9. Validator registry (per-tier gates)

Today's two validators (`server/validators/structural.js`, `render.js`) become a **registry keyed by tier**; the build pipeline (§4 step 4) dispatches to the right one per DAG node:

| Tier | Validator |
|---|---|
| T0 | structural (schema, no dup type-ids, view-refs resolve) + render-smoke |
| T1 | render-smoke of the new renderer against a scratch DB + **visual-diff** against a baseline + a11y lint (0 console errors, keyboard-reachable) |
| T2 | tool-contract test (schema round-trips, tool executes in a scratch-DB dry-run, **rejects any protected-domain call**) |
| T3 | integration test of the route/stage vs scratch DB (never `lifeos.db`) + eval-gate |
| T4 | apply the migration on a scratch copy, assert it is **additive** (`PRAGMA` before/after, no table rewrite), rebuild `lifeos-derived` and prove it converges |
| T5 | `cargo build` the new crate + its full test suite green + eval-gate + reviewer sign-off (§7) |

Every validator keeps Tier 0's discipline: run in a disposable worktree against a scratch/derived DB, fail → discard, nothing touches `main` or `lifeos.db`. The biggest reliability risk stays render-flakiness, mitigated as in [SELF-EXTENSION.md](./SELF-EXTENSION.md) §6 (ephemeral ports, scratch DB, explicit ready-event, one bounded retry).

**Implemented (issue #131):** `getValidators(tier)` in `server/validators/registry.js` returns the ordered validator list per tier. `protectedSurfaceValidator` (§5) runs **first at every tier**; T0 then adds the existing `structural` + `renderSmoke` entries (`scaffold.js` now dispatches through the registry instead of importing them directly). T1-T5 carry the protected-surface gate plus a placeholder that **fails closed** (`validator not yet implemented for <tier>`) until each tier's real validators land with its generator - an unvalidatable tier can never pass. An unknown tier resolves to the rejecting placeholder alone.

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

---

## 11. Reuse & risk

**Reuse:** everything Tier 0 already proved - Agent SDK `query()` + hooks + Zod structured output, the 3-layer sandbox, the worktree-commit flow, both existing validators, the `module_requests` state machine and offline drain; plus the [agent core](./AGENT-CORE.md) planner, the eval-gate and event store ([HARNESS-LOOP.md](./HARNESS-LOOP.md)), and the capability matrix's protected-domain guards ([AGENT-CONTROL.md](./AGENT-CONTROL.md)).

**Risks & mitigations:** (1) **scope-escape at higher tiers** - a T5 build touches many files, so Layer B's whitelist must be tight and validator-enforced, and §5's four surfaces are hard-excluded; (2) **validator flakiness** compounds across a DAG - keep each node's validator isolated and retried independently, never share scratch state; (3) **cost** - a T5 build is many model calls, so gate it behind explicit human confirm and the spend budget ([AGENT-CORE.md](./AGENT-CORE.md) §3); (4) **partial builds** - the DAG must leave the repo in a coherent, committed-or-discarded state per node, never half-written, and report partial success honestly.

---

## 12. Relationship to Tier 0 and the marketplace

[SELF-EXTENSION.md](./SELF-EXTENSION.md) remains the authoritative spec for **Tier 0** (the manifest builder) and its issue history; this doc is the superset that places T0 as rung one and specifies T1-T5. The same signed, validated artifacts each tier produces are the unit the **module marketplace** ([PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §4) distributes - an install re-runs the tier's validator locally before register, so a downloaded T5 subsystem is verified on the installing machine exactly as a locally-built one is.
