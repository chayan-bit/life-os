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
1a. **Cache probe (API-key mode, side-effect-free turns only).** For a plain completion, check the two-layer LLM cache (§10) before spending a token; a hit returns immediately. Skipped entirely for tool-using turns and on the free keyless-CLI path.
2. **Context assembly.** Pull (a) a compact world snapshot (counts of open tasks/trades/drafts/jobs for the workspace, a read over `entities`), (b) memory hits via `lifeos-memory` activation recall (§5), (c) relevant lessons/skills (§6). No re-explaining context across turns.
3. **Plan (conditional).** A cheap heuristic decides if the request is multi-step; if so, the planner emits a short ordered plan and persists it as a `jobs`/subtask-style DAG so it is inspectable and resumable (reuse the pipeline DAG shape from [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §1, not a new table).
4. **Execute (bounded).** Up to `MAX_STEPS` (default 8) tool-calling rounds. Each tool call: capability-matrix classify (`allowed | gated | forbidden`, [AGENT-CONTROL.md](./AGENT-CONTROL.md) §2) → gated ones enqueue for Telegram/PWA approval → forbidden ones refuse visibly → allowed ones execute via the existing `entity/edge/event/draft/pipeline` routes. External-origin tool results (web, inbox, provider proxy) are wrapped as untrusted content and never treated as instructions ([SECURITY.md](./SECURITY.md)).
5. **Verify (conditional).** For deliberate turns, a critic pass judges the draft against the goal; a single, real, fixable problem triggers exactly one refinement round (bounded - not an open loop).
6. **Persist + trace.** The turn appends `events('agent.turn')` (§7) and the outcome feeds consolidation (§5).

`MAX_STEPS`, the one-refinement cap, and the spend gate are the three back-pressure controls that keep a turn from running away - the same "bounded recovery budget" discipline, kept deliberately small (Life OS does **not** adopt Founder OS's 28-technique self-healing plane; see §11).

---

## 4. Tool registry + Tool-RAG

- **Registry.** The agent's tools are the existing thin surfaces - `bin/lifeos` CRUD, the Agent Control Plane action tools (`entity.create`, `edge.create`, `draft.create`, `view.configure`, `pipeline.run`, `module.requestBuild`, `search`, …), and heavy on-demand capabilities loaded via mcp-multiplexer (Figma, Higgsfield). **CRUD is never an MCP** ([CLAUDE.md](../CLAUDE.md)); the registry is a manifest of thin HTTP/CLI tools plus their JSON schemas.
- **Tool-RAG.** Mounting every tool every turn is token waste and dulls tool choice. Instead, embed each tool's description once (reuse `memvec.py` / sqlite-vec in `lifeos-derived.db` - the same infra `entity_vec` already uses) and **retrieve the top-K relevant tools per turn** plus an always-on **core set** (search, entity read/write, the gate-respecting actuators). Falls back to the full catalog on any retrieval failure. This is the mechanism that lets the tool count grow to hundreds (every self-authored tool, every module's `agentTools`) without bloating the prompt.

---

## 5. Memory integration - reuse `lifeos-memory`, don't rebuild it

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

---

## 7. Tracing & replay - reuse the event store

No new flight recorder. The `events` table already doubles as the harness run-log ([HARNESS-LOOP.md](./HARNESS-LOOP.md) §1). The loop appends one `events('agent.turn')` row per turn carrying `{ run_id, goal, plan, tool_calls:[{tool, decision, ms, ok}], model, tokens, latency_ms, refined, outcome }`. This gives, for free:
- **Observe** - `harness observe` already breaks down tokens/cost/latency/gated per tier/module/phase; agent turns slot into the same lens.
- **Eval + Gate** - deliberate agent turns are a natural `gate:"eval"` boundary (sampled Haiku judge, content-cached, [HARNESS-LOOP.md](./HARNESS-LOOP.md) §2).
- **Replay** - a turn's `plan` + `tool_calls` reconstruct exactly what it did, the same auditability the Agent Control Plane's action ledger gives at the data layer.

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

## 11. World-model snapshot + first-class guardrails

Two cheap, high-value guards the loop assumes in §3 but that deserve to be explicit:

- **World-model snapshot.** Step 2 assembles a compact situational block from a single read over `entities` (open tasks / trades / drafts / pending jobs / due follow-ups / approvals for the workspace) and injects it every turn, so the agent never re-asks for context it could compute. This is a read, not a new store - the same `events`/`entities` source [Observe](./HARNESS-LOOP.md) §3 and the per-module dashboards already use, a different lens.
- **Kill-switch + spend cap as fail-closed gates.** A per-workspace `config` kill-switch and a daily token/spend ceiling (the [Observe](./HARNESS-LOOP.md) §3 meter) are checked *before every model call* and fail closed to "stop and ask" - the same discipline as `broker-guard`. This bounds a runaway loop in cost, not just in steps.

## 12. Retrieval quality - corrective-RAG + honest abstention

`services/lifeos-memory/gate.rs` already has a self-RAG gate + multi-hop detector - the seed of a corrective loop, not the whole thing. Upgrade it to the full cycle for question-answering turns: **grade** the retrieved context for sufficiency → on weak grade, **rewrite** the query and **re-retrieve** (activation recall again) → still weak, **fall back to web/proxy** read (wrapped untrusted) → answer **with citations** to the entities/segments used. Pair it with a **calibrated confidence signal**: a genuinely low-confidence turn **abstains and asks a clarifying question** instead of emitting a confident guess - the single cheapest defense against hallucinated actuation, and a natural fit with the verify pass (§3 step 5).

## 13. Additional components (captured roadmap, lower priority)

Identified from a full audit of the reference agent architecture; specced here as direction, not yet detailed:

- **Strategy optimizer (DSPy-lite).** Per-decision epsilon-greedy A/B over prompt/approach variants, scored by logged outcome - a finer-grained sibling of the [Release loop](./HARNESS-LOOP.md) §4 (which already learns one routing prior). Reuses `events` for outcomes; no new always-on cost.
- **GraphRAG global queries.** Community detection (label propagation) over the entity graph + LLM cluster summaries, map-reduced to answer "how is my world connected / which parts touch X" questions. Extends `lifeos-memory` `graph.rs` (which already does 1-2-hop spreading activation) with a global lens; rebuilt on the sleep cycle.
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
