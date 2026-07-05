# Agent core - the general plan → execute → verify loop

> The reasoning engine Life OS is missing today: a bounded **plan → execute → verify** tool-calling loop that any surface (console, command bar, bot, pipeline, self-extension builder) calls, instead of the current single-shot text completion.
> It turns `/api/llm` from "one prompt in, one blob out" into "think → use a tool → check its own work → repeat," while reusing every substrate Life OS already has: `lifeos-memory` as the brain, `events` as the flight recorder, the Agent Control Plane as the actuation surface, and the four protected domains as the hard boundary.

> Invariant alignment: this **extends, never relaxes**, [SECURITY.md](./SECURITY.md) and [AGENT-CONTROL.md](./AGENT-CONTROL.md). Every tool call the loop makes still passes the capability matrix; every outward/irreversible action still gates; the four protected domains still have no tool to call.

**Status: design spec, pre-implementation.** No issue numbers yet - the "Implemented (issue #N)" annotations used elsewhere in `docs/` are added as this gets built.

---

## 1. Why - the gap this closes

The only tool-using agent in the repo today is `server/scaffold.js`, and it is welded to module-building. Every other AI surface routes through `/api/llm` (`services/lifeos-api/src/routes/llm.rs`) → the `lifeos-agents` CLI router (`services/lifeos-agents/src/lib.rs`), which is **single-shot**: one prompt, one text answer, **no tool use, no iteration, no planning, no self-verification.** Consumers - `AIConsole.jsx`, `AgentHarness.jsx`, `actionPlanCompiler.js`, `KnowledgeAtlas.jsx` - each re-implement their own thin prompt-and-parse on top of that.

The result: the AI can *answer* about the app but cannot *work in it over multiple steps* - it can't research a thing, act on the result, observe the outcome, and correct itself in one turn. That multi-step loop is the single largest capability the [Agent Control Plane](./AGENT-CONTROL.md) (which already defines *what* the agent may actuate) and [self-extension](./SELF-EXTENSION-V2.md) (which needs a planner) are both waiting on.

**This doc specifies the loop. It does not add new powers** - it composes the tools, memory, and gates that already exist into an iterative reasoning cycle.

---

## 2. Position in the stack

```
   surfaces:  AIConsole · Cmd-K bar · Telegram bot · pipelines · self-extension builder
                                   │  all call the same
                                   ▼
        ┌──────────────────────  /api/agent  ──────────────────────┐
        │  plan → execute → verify loop  (JS agent runtime, §3)     │
        │   • Tool-RAG retrieves K relevant tools + a core set (§4) │
        │   • memory context injected from lifeos-memory (§5)       │
        │   • each tool call → capability-matrix gate (AGENT-CONTROL)│
        │   • each turn → events('agent.turn') flight recorder (§7) │
        └───────────────┬───────────────────────┬──────────────────┘
        tool calls ride │                        │ text-only completions
        existing routes ▼                        ▼ still use the cheap path
   entity/edge/event/draft/pipeline/…    lifeos-agents CLI router (llm.rs)
```

- **The loop is JS/TS**, living beside `scaffold.js` (e.g. `server/agent/`), for the same reason `scaffold.js` is JS: it orchestrates the **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`, `query()` with tool-calling), which is TS-first and already a dependency ([RUST-COMPONENTS.md](./RUST-COMPONENTS.md) §3: *"scaffold.js + validators stays JS - orchestrates the Agent SDK"*; §1: *lifeos-pipelines "shells to Agent SDK"*).
- **`lifeos-api` exposes `/api/agent`** (a sibling of `/api/llm`); the Rust side stays the trust anchor and DB-token owner, shelling to the JS runtime the same way `lifeos-drain` shells to `node scaffold.js`.
- **`lifeos-agents` (the CLI router) stays** as the cheap, single-shot completion path - the loop uses it (or the Agent SDK's keyless CLI subprocess auth) as its underlying model call, so "keyless AI everywhere" is preserved. `/api/llm` is not removed; `/api/agent` is the loop-mode superset.

---

## 3. The loop - plan → execute → verify

Modeled on the disciplined turn lifecycle, adapted to Life OS's substrate. Bounded and cheap by default.

1. **Gate check.** Honor a kill switch (`config` row) and per-workspace daily spend cap ([HARNESS-LOOP.md](./HARNESS-LOOP.md) Observe meter) before any model call - fail closed, same shape as `broker-guard` (§11).
1a. **Cache probe (API-key mode, side-effect-free turns only).** For a plain completion, check the two-layer LLM cache (§10) before spending a token; a hit returns immediately. Skipped entirely for tool-using turns and on the free keyless-CLI path. **Implemented (issue #127):** `loop.js` computes eligibility as `isCacheMode() && !needsPlanning(prompt) && !looksActiony(prompt)` right after the gate passes and before `buildWorldSnapshot`/`fetchMemoryContext` run, so a hit costs zero HTTP calls beyond the probe itself; a hit short-circuits straight to `finalize()` with `tokens:0`/`tier:'mac'`/`outcome:'completed'` and an `attrs.cache` stamp of `'exact'` or `'semantic'`.
2. **Context assembly.** Pull (a) a compact world snapshot (counts of open tasks/trades/drafts/jobs for the workspace, a read over `entities`), (b) memory hits via `lifeos-memory` activation recall (§5), (c) relevant lessons/skills (§6). No re-explaining context across turns.
3. **Plan (conditional).** A cheap heuristic decides if the request is multi-step; if so, the planner emits a short ordered plan and persists it as a `jobs`/subtask-style DAG so it is inspectable and resumable (reuse the pipeline DAG shape from [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §1, not a new table).
4. **Execute (bounded).** Up to `MAX_STEPS` (default 8) tool-calling rounds. Each tool call: capability-matrix classify (`allowed | gated | forbidden`, [AGENT-CONTROL.md](./AGENT-CONTROL.md) §2) → gated ones enqueue for Telegram/PWA approval → forbidden ones refuse visibly → allowed ones execute via the existing `entity/edge/event/draft/pipeline` routes. External-origin tool results (web, inbox, provider proxy) are wrapped as untrusted content and never treated as instructions ([SECURITY.md](./SECURITY.md)).
5. **Verify (conditional).** For deliberate turns, a critic pass judges the draft against the goal; a single, real, fixable problem triggers exactly one refinement round (bounded - not an open loop).
6. **Persist + trace.** The turn appends `events('agent.turn')` (§7) and the outcome feeds consolidation (§5).

`MAX_STEPS`, the one-refinement cap, and the spend gate are the three back-pressure controls that keep a turn from running away - the same "bounded recovery budget" discipline, kept deliberately small (Life OS does **not** adopt Founder OS's 28-technique self-healing plane; see §11).

---

## 4. Tool registry + Tool-RAG

**Implemented (issue #123):** `server/agent/toolRag.js` (`indexTools`/`retrieveTools`), wired into `server/agent/loop.js` before the execute stage and into `server/agent/executor.js`'s SDK tool build; tests in `server/test/toolRag.test.js`.

- **Registry.** The agent's tools are the existing thin surfaces - `bin/lifeos` CRUD, the Agent Control Plane action tools (`entity.create`, `edge.create`, `draft.create`, `view.configure`, `pipeline.run`, `module.requestBuild`, `search`, …), and heavy on-demand capabilities loaded via mcp-multiplexer (Figma, Higgsfield). **CRUD is never an MCP** ([CLAUDE.md](../CLAUDE.md)); the registry is a manifest of thin HTTP/CLI tools plus their JSON schemas.
- **Tool-RAG.** Mounting every tool every turn is token waste and dulls tool choice. Instead, embed each tool's description once (reuse `memvec.py` / sqlite-vec in `lifeos-derived.db` - the same infra `entity_vec` already uses) and **retrieve the top-K relevant tools per turn** plus an always-on **core set** (search, entity read/write, the gate-respecting actuators). Falls back to the full catalog on any retrieval failure. This is the mechanism that lets the tool count grow to hundreds (every self-authored tool, every module's `agentTools`) without bloating the prompt.

---

## 5. Memory integration - reuse `lifeos-memory`, don't rebuild it

**Implemented (issue #124):** `server/agent/memoryContext.js` (`fetchMemoryContext`/`ingestTurnOutcome`), wired into `server/agent/loop.js`'s context-assembly step (recall in, appended to the planner/executor prompt as a labeled block) and its finalize/catch paths (write-out via `/api/memory/ingest`, skipped on gate refusal); tests in `server/test/memoryContext.test.js`.

Founder OS bolts a vector+SQL+graph triplex onto its agent. Life OS already has a **stronger** brain in `services/lifeos-memory` (activation recall `A(m)=relevance·recency·importance·frequency`, spreading activation, sleep-cycle consolidation, procedural store, bi-temporal supersede - see [AI-MEMORY.md](./AI-MEMORY.md)), and it is already wired into the live `lifeos-drain` loop via `Dispatch::MemorySleep`. The agent core is a **consumer** of it:

- **Recall in:** step 2 calls `lifeos-memory` retrieval (the token-budgeted context compiler, `compiler.rs`) to inject working/episodic/semantic context - not a flat top-K, the activation-scored recall the crate already computes.
- **Write out:** each turn's salient facts and the consolidated outcome flow back as `events` + `entities`, so the same sleep cycle that already runs (`consolidate.rs`: segment/consolidate/importance/surprise/decay/supersede) folds agent experience into durable memory with **no new subsystem.**
- **Procedural rules** (`procedural.rs`) are exactly where self-evolution lives (§6).

---

## 6. Self-evolution - lessons, skills, an operating manual

The behavior that makes Founder OS "get better each turn" (its `lessons`/`skills`/`instructions.md`) maps onto memory Life OS already has, so it is plumbing, not architecture:

- **Lessons & skills** are `procedural` memory entities (`module='agent', type='lesson'|'skill'`) written after substantive turns and **retrieved by activation recall into the build/turn prompt** - the "distill after, recall before" cycle, riding `consolidate.rs` for decay/supersede so stale advice ages out instead of accreting forever.
- **The operating manual** is a single versioned `config` (reuse the `configs` + `vcs_refs(kind='config_active')` machinery from [HARNESS-LOOP.md](./HARNESS-LOOP.md) §4). The agent may **draft** a manual candidate; promoting it live is **human-typed only** (`harness config promote`) - identical to the release-loop carve-out, because the manual shapes the agent's own behavior and must not be self-promotable ([AGENT-CONTROL.md](./AGENT-CONTROL.md) §1, protected domain #2 spirit).
- **Self-authored tools** (Voyager-style `create_tool`) are **not** a special path here - they are a **Tier-2 self-extension build** ([SELF-EXTENSION-V2.md](./SELF-EXTENSION-V2.md) §2), so they inherit that ladder's sandbox, validator, human gate, and git commit. The agent proposing a new tool is the agent asking the builder to generate one.

**Implemented (issue #134):** self-authored tools are real, not a future path. A T2 build writes ONE pure request-descriptor file (`server/agent/tools/generated/<name>.js` - `{name, description, classification, inputSchema, example, request}`, no I/O of its own) that `server/agent/tools/generated/index.js`'s `loadGeneratedTools()` merges into the SAME `server/agent/actionRegistry.js::REGISTRY` this section's `classify()` and Tool-RAG (§4) already read - so a newly built tool is `classify()`-able, retrievable by Tool-RAG, and offered to the loop's execute stage next turn with no additional wiring. Gating is genuinely "at use, not creation": creation auto-commits via the build pipeline (T2 is not in `gate.js`'s T3+ human-approval set), and at use time the tool's own `classification` (`'allowed'` or `'gated'`) rides the executor's existing capability-matrix path exactly like a hand-written tool - a `'gated'` generated tool enqueues a `pending_approval` draft and never calls its own `request()`; an `'allowed'` one calls through `executor.js`'s one chokepoint like every other tool. The one genuinely new piece is the route allowlist (`server/lib/routeAllowlist.js`'s `isRouteAllowed`, docs/SELF-EXTENSION-V2.md §5): since a generated tool's author controls which lifeos-api route it targets, the executor re-validates the tool's actual runtime path on every call (not just at load time) before ever dispatching the HTTP request - a tool whose `request()` resolves to an order/secret/connection path at call time is refused in the moment, never blindly trusted from its one-time load-time dry-run.

**Implemented (issue #128):** lessons/skills ride the EXISTING procedural
pipeline verbatim rather than a new `module='agent'` entity type - the code
reality is that `memory_rules` (`services/lifeos-memory/src/procedural.rs`)
is populated ONLY from `feedback.given` events via `HeuristicPolicyLearner`,
which reads `attrs.feedback` (not `attrs.rule` - a naming trap in the
original issue text) and hardcodes confidence by event type rather than
trusting a caller-supplied one. `server/agent/reflect.js`'s
`distillLesson(queryFn, prompt, outcome, resultText, opts)` is the
"distill after" half: it runs ONLY for a **completed** turn whose prompt
reads as corrective (`looksCorrective` - a conservative allow-list of
words like "always"/"never"/"instead"/"next time"; skip on any doubt), makes
one structured-output call (`{ rule, confidence, kind: 'lesson'|'skill' }`,
`rule: null` when nothing durable applies), and - only when a rule comes
back - appends exactly one `feedback.given` event with
`attrs.feedback = "<kind>: <rule>"` and `attrs.confidence` (kept for a
future learned `PolicyLearner`, even though `HeuristicPolicyLearner` doesn't
read it today). It is wired into `loop.js`'s `finalize()` alongside
`ingestTurnOutcome`, and is best-effort like every other write-back in that
function - a distillation failure never affects an already-computed turn
result. **"Recall before" needed no new code**: `memory_rules` already feeds
`rules_for_prompt`, which the compiler already folds into
`POST /api/memory/context`'s response, which `fetchMemoryContext` (issue
#124) already injects into every turn - so a distilled lesson is live on the
turn *after* the next sleep cycle consolidates it (`consolidate.rs`), by
design, not as a shortcut.

Rule aging is a small addition to that same sleep cycle:
`consolidate.rs::retire_stale_rules` retires any `memory_rules` row that is
both older than `RULE_TTL_DAYS` (45) and below `RULE_RETIRE_CONFIDENCE`
(0.6), by emitting `memory.rule.retired` events through the same
event-append path `HeuristicPolicyLearner`'s own retractions use - never a
direct `UPDATE` outside the projector. It runs in the same
consumable-events-required branch as `decay_sweep` (so it fires whenever a
sleep cycle actually runs, not on a separate schedule).

The operating manual reuses the release-loop `configs` +
`vcs_refs(kind='config_active')` machinery ([HARNESS-LOOP.md](./HARNESS-LOOP.md)
§4) with `kind='agent_manual'`: `server/agent/manual.js`'s
`fetchActiveManual(httpFn, workspaceId)` does `GET /api/configs?kind=agent_manual`,
cross-references the response's `active.agent_manual` pointer against its
`configs` list, and returns that config's payload as a labeled
`## Operating manual` block (`null` on absence or any failure -
failure-tolerant like the memory-context fetch). `loop.js` injects it
alongside the world snapshot and memory block. The agent may **draft** a
candidate via the new `config.draft` action-registry tool
(`server/agent/actionRegistry.js`, `allowed` - a draft is inert until
promoted) - but there is deliberately no `config.promote`/`config.rollback`
tool anywhere in the registry, and both names are now listed explicitly in
`PROTECTED_TOOLS` (not just relying on "unknown name -> forbidden") so the
never-agent-callable carve-out ([AGENT-CONTROL.md](./AGENT-CONTROL.md) §1)
is intentional in the code, not incidental. Only `harness config promote`
(human-typed CLI) ever flips the active pointer.

---

## 7. Tracing & replay - reuse the event store

No new flight recorder. The `events` table already doubles as the harness run-log ([HARNESS-LOOP.md](./HARNESS-LOOP.md) §1). The loop appends one `events('agent.turn')` row per turn carrying `{ run_id, goal, plan, tool_calls:[{tool, decision, ms, ok}], model, tokens, latency_ms, refined, outcome }`. This gives, for free:
- **Observe** - `harness observe` already breaks down tokens/cost/latency/gated per tier/module/phase; agent turns slot into the same lens.
- **Eval + Gate** - deliberate agent turns are a natural `gate:"eval"` boundary (sampled Haiku judge, content-cached, [HARNESS-LOOP.md](./HARNESS-LOOP.md) §2).
- **Replay** - a turn's `plan` + `tool_calls` reconstruct exactly what it did, the same auditability the Agent Control Plane's action ledger gives at the data layer.

**Implemented (issue #125):** `server/agent/loop.js`'s `persistTurn` now
additionally stamps the run-log lens fields `lifeos-pipelines::emit_run_event`
already writes for pipeline stages - `tier:"mac"`, `tokens_in`/`tokens_out`
(split per stage via the new `server/agent/usage.js` accumulator, replacing
three duplicated `usageTokens` helpers in
`planner.js`/`critic.js`/`executor.js`), `gated` (`1` iff the turn ends
`awaiting_approval`), and `error` (the caught exception message, or `null`)
- all as **top-level** `/api/event` columns, not nested in `attrs`, so
`GET /api/metrics` (`services/lifeos-api/src/routes/metrics.rs`) counts
and sums them with **no Rust change**: its aggregation SQL already runs
over the whole `events` table regardless of `type`. A deliberate turn
(had a plan, finished `completed`) also stamps `attrs.stage:"eval"` - the
exact field `events_by_phase` already reads via
`json_extract(attrs, '$.stage')` - so agent turns slot into the same
phase lens pipeline stages do and become selectable as the natural eval
boundary. **Scope note:** actually invoking the sampled Haiku judge on
that boundary stays out of scope here (wiring only, per the issue) -
today `eval_gate::judge_stage_output` is only called inline from
`lifeos-pipelines::process_pipeline_job`'s Rust stage runner; there is no
HTTP surface or batch consumer that scores arbitrary `events` rows yet. A
future issue would add one keyed on
`type='agent.turn' AND json_extract(attrs,'$.stage')='eval'`.
`server/scripts/replayTurn.js` (`node server/scripts/replayTurn.js
<run_id>`) is the read-only replay inspector: fetches the one
`agent.turn` row via `GET /api/event?run_id=...&type=agent.turn` and
renders goal/plan/tool-calls/outcome/tokens step-by-step
(`formatTurnReplay`, exported separately from the CLI wrapper, is a pure
function over the event row - never re-executes a tool call). Tests:
`server/test/agent.test.js` (Observe stamps, exactly-one-event-on-refine)
and `server/test/replayTurn.test.js` (formatter + not-found path).

---

## 8. Safety - the loop invents no new authority

The loop is a *sequencer* of already-gated actions, so its safety is inherited, not re-argued:

- Every tool call is classified by the **capability matrix** ([AGENT-CONTROL.md](./AGENT-CONTROL.md) §5); the **four protected domains** (VCS internals, security/gating config, OAuth/connections, secrets) have **no tool to call** and refuse visibly.
- Outward/irreversible tools stay **draft → Telegram approve → execute** ([SECURITY.md](./SECURITY.md) §2).
- Trading stays **read-only for any agent** - `broker-guard` fails closed regardless of what the loop plans; no order tool is registered.
- `events` is append-only; the loop cannot rewrite history.
- External content (web/inbox/proxy reads) is wrapped untrusted and never obeyed as instructions.

The one genuinely new guard is the **spend + step budget** (§3) - back-pressure so a looping agent can't burn tokens or hammer routes; it fails closed to "stop and ask."

---

## 9. Self-healing - deliberately minimal

Adopt only the cheap, high-value recoveries, in cheapest-first order, bounded by a per-turn recovery budget: **retry with backoff** (transient route/model blips) → **argument repair** (fix a malformed tool call from its schema + error) → **tool substitute** (an equivalent read tool) → **one replan** (on a failed step, preserving completed work) → **degrade** (return the partial result honestly) → **escalate** (surface to Telegram). Circuit-breaker per model provider reuses the `lifeos-agents` router's existing detect/fallback ordering. Everything past this (compensating transactions, checkpoint/rollback, metacognitive anomaly scoring, watchdog self-test) is **explicitly out of scope** as premature for a single-tenant-first system - revisit only if real failure data demands it.

**Implemented (issue #129):** `server/agent/recovery.js` holds the shared primitives - `RECOVERY_BUDGET = 4` (one counter shared across every recovery action this turn), `RETRY_BACKOFF_MS = 250`, the `ARG_REPAIR_STATUSES`/`SUBSTITUTES` tables, `schemaHint()`, and `wrapQueryFnWithBreaker()`. `executor.js`'s `runTool` wires the first three rungs into `runAllowed`: a throw or a `>=500` response retries once after backoff (budget-decrementing, never for a 4xx); a `400`/`422` gets a `repair_hint` (schema summary + server error) attached to the tool result, spending budget at most once per tool per turn; a read tool (`search.query`, `memory.recall`, `entity.get`) whose retry is still `>=500` gets a `substitute_hint` appended - write/gated tools are never in `SUBSTITUTES` so they never get one. `loop.js` wires the last three rungs: a thrown execute-stage error triggers `recoverExecuteFailure` - one replan (planner re-run with the failure message + the completed-tool ledger appended so the model doesn't repeat successful steps; `ctx.stepCount` already threads the remaining `MAX_STEPS` budget since it's the same mutable counter `runTool` checks) - capped by `ctx.replanned`, a per-turn boolean. Any further failure, an open breaker, or an exhausted budget calls `degradeAndEscalate`: an honest `outcome: 'degraded'` text listing what completed vs. what failed, plus a best-effort `agent.escalation` event (Telegram consumption is downstream of the events bus, out of scope here). Every recovery action appends a `{kind, tool, ok}` entry to `ctx.recoveries`, stamped additively into the `agent.turn` event's `attrs.recoveries`.

**Deviation - circuit breaker:** the issue calls for a breaker "per model provider reusing the `lifeos-agents` router's existing detect/fallback ordering," but that Rust router has no *runtime* provider fallback - its registry order only governs the default pick at startup, and it isn't reachable from the JS agent loop in-process anyway. Implemented instead: `QUERY_BREAKER_THRESHOLD = 2` consecutive `ctx.queryFn` throws within a single turn (any call site - plan, execute, verify, replan) opens `ctx.breakerOpen`, which fails every further model call fast for the rest of the turn and skips the replan rung entirely, going straight to degrade+escalate. A true per-provider runtime fallback belongs in `lifeos-agents` (Rust) itself - follow-up scope for the #138-140 era, not this issue.

---

## 10. LLM cache - for API-key mode only

Life OS has **no LLM cache today**, and the keyless path (a local Claude Code / Gemini CLI already on `$PATH`) is free, so caching there buys nothing. But when a user runs on a raw `ANTHROPIC_API_KEY` (the SaaS/no-local-CLI path), repeat and near-duplicate calls cost real tokens. A **two-layer cache, engaged only in API-key mode**, cuts that.

There is already a proven precedent to copy: `services/lifeos-pipelines/src/eval_gate.rs` content-caches the Haiku judge via `BLAKE3(content) →` one `entities` row (`module='harness', type='eval_cache'`) - "zero new tables." The LLM cache reuses that shape.

- **Layer 1 - exact hash.** `BLAKE3(model ∥ system ∥ prompt ∥ params)` → an `entities` row (`module='agent', type='llm_cache'`). An exact re-ask returns instantly, 0 tokens, before any model call.
- **Layer 2 - semantic.** On an exact miss, embed the prompt (reuse `memvec.py` / sqlite-vec in `lifeos-derived.db`, the same infra `entity_vec` and Tool-RAG use) and match against cached prompts; a hit within `CACHE_DISTANCE_THRESHOLD` (conservative default ~0.08, tunable) returns the cached completion. A miss falls through to the model, and the result is written to both layers.

**Hard constraints (correctness over hit-rate):**
- **Only side-effect-free calls are cacheable** - plain completions (analysis, research summary, draft text). A **tool-using loop turn is never cached** (its effect is state change, not text), and the cache probe sits *before* step 3's context assembly, keyed on the raw request, so a cached answer can never skip a needed mutation.
- **Workspace-scoped.** Cache rows carry `workspace_id`; one tenant never serves another's answer (multi-tenant invariant).
- **Cache lives in the un-synced derived DB / a local `entities` partition**, rebuilt locally, never a sync-reconciliation source ([DATA-MODEL.md](./DATA-MODEL.md) §4.3) - a stale cache row must never masquerade as canonical state.
- **Off by default on the keyless path**, controlled by `SEMANTIC_CACHE` + `CACHE_DISTANCE_THRESHOLD`; every hit is counted in Observe so the token savings are visible.

**Implemented (issue #127) - two deliberate deviations from the text above:**
- **Both layers live in the un-synced derived DB, not an `entities` row.** The
  "an `entities` row" phrasing above can't satisfy this section's own hard
  constraint ("never a sync-reconciliation source") - libSQL has no
  table-level no-sync flag ([DATA-MODEL.md](./DATA-MODEL.md) §4.3), so a
  canonical `entities` row for the cache would itself be a sync artifact.
  Instead `server/memvec.py` gained a new derived-DB-only table,
  `llm_cache(key, workspace, model, prompt, completion, created_at)`, plus two
  subcommands: `cache-put` (upserts the row and embeds the prompt into the
  existing `entity_vec` index under id `llmcache:<key>`, mirroring Tool-RAG's
  (#123) `tool:`-prefixed convention) and `cache-get` (exact lookup by
  `key`+`workspace`, else a vector query over `llmcache:` ids in the same
  workspace within `CACHE_DISTANCE_THRESHOLD`, re-verifying the resolved row's
  workspace before ever returning it - cross-tenant serving stays impossible
  by construction, not just by convention).
- **`blake2b512` (node/python stdlib) instead of BLAKE3.** BLAKE3 was named
  above for parity with `eval_gate.rs`'s Rust cache, but neither Node nor
  Python ship BLAKE3 in stdlib, and adding a `blake3` dependency would violate
  the machine's Nix-only package policy for this JS runtime. The hash
  algorithm is opaque to correctness (any stable hash of
  `model||system||prompt||params` works), so this is a naming-parity
  deviation, not a functional one.

`server/agent/llmCache.js` is the JS side: `isCacheMode()` gates everything on
`ANTHROPIC_API_KEY` being set (a no-op on the keyless CLI path), `cacheKey()`
hashes the raw request, `probe()`/`store()` shell `memvec.py cache-get`/
`cache-put` (both fully mockable via `opts.cacheGetFn`/`opts.cachePutFn`, and
both fail closed to a plain miss / a swallowed error - a cache outage never
fails a turn). `looksActiony()` is the conservative imperative-verb denylist
(create/update/delete/send/draft/schedule/…) that keeps mutation requests out
of the cache. Wired into `server/agent/loop.js` at step 1a (probe, before
context assembly - see below) and step 6 (store, only on a completed turn with
zero tool calls). Tests: `server/test/llmCache.test.js` (the issue's six
required scenarios, plus actiony-prompt and cache-error-proceeds; a
`describe.skipIf`-guarded python round-trip test covers the derived-DB side
including cross-workspace misses when `sentence-transformers`/`sqlite-vec` are
installed locally).

## 11. World-model snapshot + first-class guardrails

Two cheap, high-value guards the loop assumes in §3 but that deserve to be explicit:

- **World-model snapshot.** Step 2 assembles a compact situational block from a single read over `entities` (open tasks / trades / drafts / pending jobs / due follow-ups / approvals for the workspace) and injects it every turn, so the agent never re-asks for context it could compute. This is a read, not a new store - the same `events`/`entities` source [Observe](./HARNESS-LOOP.md) §3 and the per-module dashboards already use, a different lens.
- **Kill-switch + spend cap as fail-closed gates.** A per-workspace `config` kill-switch and a daily token/spend ceiling (the [Observe](./HARNESS-LOOP.md) §3 meter) are checked *before every model call* and fail closed to "stop and ask" - the same discipline as `broker-guard`. This bounds a runaway loop in cost, not just in steps.

**Implemented (issue #126, delta on top of #122/#125's guards):** `server/agent/gate.js`'s
kill-switch and daily-token-budget checks, and `agent.turn`'s tier/tokens/gated Observe stamps,
were already shipped by #122/#125 (see §4/§5/§7). This issue closed the remaining two gaps:
- `server/agent/worldSnapshot.js`'s `buildWorldSnapshot` grew from a two-count stub into the
  full snapshot - open tasks, open trades, drafts/pending approvals, pending jobs, and tasks
  due today-or-overdue - via four parallel bounded reads (`GET /api/entity?module=tasks`,
  `GET /api/entity?module=trading&type=trade`, `GET /api/entity?status=pending_approval`,
  `GET /api/jobs?status=pending`), each capped at `LIST_LIMIT` rows. "Drafts" and "pending
  approvals" collapse to one query in this schema: `draft.create` (`server/agent/actionRegistry.js`,
  the only gated write) is what sets `status='pending_approval'`, on any module - there is no
  separate universal draft type to query, so inventing a second one would double-count, not add
  signal. "Due follow-ups" reuses the `/today` bot command's own convention
  (`attrs.due` on the `tasks` module, [MODULES.md](./MODULES.md) §2.2) rather than a
  non-existent follow-up entity type. "Open trades" reads the trade schema's own
  `attrs.closed_at` ([MODULES.md](./MODULES.md) §2.4) since trades have no separate lifecycle
  status convention. The snapshot is failure-tolerant at two levels: if every read fails
  outright it returns `null` and `loop.js` omits the block entirely (the turn still proceeds -
  this is context, not a gate); if only some reads fail, the rest still render and the failed
  categories degrade to `0` rather than losing the whole block over one flaky route.
- `server/agent/gate.js`'s kill-switch refusal now also appends an `events('agent.paused')` row,
  best-effort, mirroring the `agent.budget_exhausted` escalation already next to it - so a
  paused agent shows up in Observe instead of just going quiet. `loop.js`'s refusal response
  also carries a `text: "Agent paused: the kill switch is on for this workspace."` field for
  the kill-switch case specifically, on top of the existing `outcome`/`error` reason codes.
  Tests: `server/test/worldSnapshot.test.js` (per-category assembly, JSON-string attrs,
  total-failure -> `null`, partial-failure -> degraded zero) and `server/test/agent.test.js`
  (`agent.paused` emission, best-effort on escalation-write failure, snapshot injected into the
  execute prompt, snapshot omitted without a stray `"null"` when every read fails, and both the
  kill-switch and `gate_unavailable` guards proven to block before `queryFn` or any world-snapshot
  route is ever touched).

## 12. Retrieval quality - corrective-RAG + honest abstention

`services/lifeos-memory/gate.rs` already has a self-RAG gate + multi-hop detector - the seed of a corrective loop, not the whole thing. Upgrade it to the full cycle for question-answering turns: **grade** the retrieved context for sufficiency → on weak grade, **rewrite** the query and **re-retrieve** (activation recall again) → still weak, **fall back to web/proxy** read (wrapped untrusted) → answer **with citations** to the entities/segments used. Pair it with a **calibrated confidence signal**: a genuinely low-confidence turn **abstains and asks a clarifying question** instead of emitting a confident guess - the single cheapest defense against hallucinated actuation, and a natural fit with the verify pass (§3 step 5).

**Implemented (issue #130):** `server/agent/correctiveRag.js` - `gradeRecall()` classifies the `/api/memory/context` `recall` outcome (already forwarded by `fetchMemoryContext`, issue #124) as `sufficient` (recalled, ≥2 memories) / `weak` (recalled-thin or abstained) / `none` (skipped); `correctiveRetrieve()` runs the cycle for question turns only (`isQuestionTurn()` reuses `llmCache.js`'s non-actiony heuristic, no duplication) - a weak/none grade triggers one bounded `rewriteQuery()` structured call + one re-fetch with the sharpened query, and a still-weak re-grade appends an explicit web-fallback suggestion line to the context block pointing at the new `web.scrape` tool (`server/agent/actionRegistry.js` - `allowed`, `external: true`, backed by the already-read-only-by-construction `POST /api/browser/scrape`, docs/SECURITY.md untrusted-wrapping applies via `executor.js`'s existing `wrapUntrusted()` path). A citation instruction line is appended whenever any real memory content is present, pointing at the `(src=...)` ids `compiler.rs:108` already inlines - no new formatting pipeline. Abstention rides the verify pass: `critic.js`'s `CritiqueSchema` gained a `confidence` field (0-1, defaults to 1 so pre-existing scripted verdicts without it still parse); `loop.js` replaces the answer with `buildAbstentionResponse()` (an honest "I don't know" + a clarifying question derived from the critic's `issue`) and sets `outcome: 'abstained'` only when the turn is a question, the final memory grade never reached `sufficient`, critic confidence is below the named `ABSTAIN_THRESHOLD` (0.4), and no tool call in the ledger succeeded - real completed work is always reported, never discarded. Every turn stamps `attrs.rag: { grade, rewritten, regraded, web_suggested }` (or `null` for non-question turns) on its `agent.turn` event. Tests: `server/test/correctiveRag.test.js`.

## 13. Additional components (captured roadmap, lower priority)

Identified from a full audit of the reference agent architecture; specced here as direction, not yet detailed:

- **Strategy optimizer (DSPy-lite).** Per-decision epsilon-greedy A/B over prompt/approach variants, scored by logged outcome - a finer-grained sibling of the [Release loop](./HARNESS-LOOP.md) §4 (which already learns one routing prior). Reuses `events` for outcomes; no new always-on cost. **Implemented (issue #138):** `server/agent/strategy.js` - `recordOutcome()`/`loadOutcomes()` ride `events(type='agent.strategy.outcome')`, `chooseVariant()` explores any zero-play variant first then epsilon-greedy exploits the best success rate (injectable `rng`, deterministic tie-break), `leaderboard()` ranks a group's variants by rate. Library-only for now - no live decision group calls it yet, so there is no new always-on cost; wiring a real group (e.g. `draft_tone`) is a follow-up once one exists.
- **GraphRAG global queries.** Community detection (label propagation) over the entity graph + LLM cluster summaries, map-reduced to answer "how is my world connected / which parts touch X" questions. Extends `lifeos-memory` `graph.rs` (which already does 1-2-hop spreading activation) with a global lens; rebuilt on the sleep cycle. **Implemented (issue #139):** `services/lifeos-memory/src/communities.rs` - deterministic label propagation over `memory_edges`, a `CommunitySummarizer` trait (default `HeuristicSummarizer`, no network) persisted to `memory_communities` (migration 0019), rebuilt in `consolidate::run_sleep_cycle`; `GET /api/memory/network` + `POST /api/memory/network/ask` (map-reduce-lite ranking, the agent composes the final answer) and the `memory.network` tool in `server/agent/actionRegistry.js`.
- **Self-eval regression suite.** Golden tool-routing scenarios (given a request, does the loop reach a sensible tool and avoid a wrong one?) run side-effect-free, so self-evolution and prompt/manual changes can't silently regress routing. Complements the quality-focused [eval-gate](./HARNESS-LOOP.md) §2.

---

## 14. Build surface & verification

- **Backend:** `/api/agent` on `lifeos-api` (shells to the JS runtime); no new privileged routes - the loop rides existing `entity/edge/event/draft/pipeline` routes, boundary enforced by the capability matrix. **Implemented (issue #122):** `services/lifeos-api/src/routes/agent.rs` spawns `node agent/run.js <prompt> <workspaceId>` from `config.server_dir`, wraps it in a 300s timeout, and parses the last stdout JSON line - the same process contract `lifeos-drain`'s `ScaffoldJsBuilder` uses.
- **Agent runtime:** `server/agent/` (JS) - the loop, planner, critic, tool registry, Tool-RAG retriever; reuses `scaffold.js`'s Agent SDK wiring and sandbox primitives. **Implemented (issue #122):** `server/agent/{loop,gate,worldSnapshot,planner,executor,critic,actionRegistry,http,run}.js` - the bounded gate -> snapshot -> plan -> execute -> verify loop over in-process Claude Agent SDK MCP tools, `MAX_STEPS=8`, one-refine cap, capability-matrix classification (`allowed`/`gated`/`forbidden`), and one append-only `events('agent.turn')` row per turn. Tool-RAG retrieval (§4) and memory injection (§5) are still stubs (the latter deferred to issue #124).
- **Frontend:** `AIConsole.jsx` / Cmd-K bar / `actionPlanCompiler.js` migrate from bespoke prompt-and-parse to `/api/agent`; the action ledger already renders the results.
- **Must-pass checks:**
  - A multi-step request ("find my overdue tasks, tag them urgent, and draft a summary") plans, executes across tools, self-verifies, and lands every mutation in the Agent Control Plane ledger, each undoable.
  - A forbidden action (edit gating config, read a secret, place an order) refuses visibly and logs `action.denied` - and has no tool to call.
  - A gated action (publish a draft) does not execute without Telegram approval.
  - The step/spend budget halts a runaway turn and escalates rather than looping.
  - Every turn appears in `harness observe` and replays from its `events('agent.turn')` row.
  - In API-key mode, a repeated side-effect-free question is served from cache at 0 tokens (exact then semantic), the hit is counted in Observe, and a **tool-using turn is never served from cache**; on the keyless CLI path the cache is bypassed.

---

## 15. Reuse & non-goals

**Reuse:** Agent SDK (already wired in `scaffold.js`), `lifeos-agents` router (cheap completions, keyless), `lifeos-memory` (the brain), `events` (tracing/eval/observe), Agent Control Plane (actuation + ledger + capability matrix), `configs`/`vcs_refs` (the operating manual), sqlite-vec/memvec (Tool-RAG).

**Explicit non-goals** (things Founder OS has that Life OS deliberately does *not* copy): its parallel Chroma+SQLite+graph triplex (our single event-sourced libSQL + `lifeos-derived.db` is cleaner and already richer); its 12-path agent swarm (scoped down to a supervisor+subagents split for T5 subsystem builds only, [SELF-EXTENSION-V2.md](./SELF-EXTENSION-V2.md) §7); its 28-technique self-healing plane (trimmed to §9); and the whole founder/CRM/outreach domain framing (out of scope by design).
