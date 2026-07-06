# Agent Control Plane - universal in-app actuation

The headline capability of Life OS: the in-app AI can **operate the entire app the way you can** - read and mutate any entity, edge, draft, module configuration, view, dashboard, and navigation state - through one typed, audited, reversible action layer.
It is the UI-level twin of the harness's deep work: where the harness writes code and runs pipelines, the Agent Control Plane *drives the running app*.

This doc specifies that layer, its hard boundary (what the agent may **never** touch), and the supporting features that make broad actuation safe: a typed action registry, dry-run previews, an action ledger with one-click undo, and the capability matrix.

> Invariant alignment: this extends - never relaxes - [SECURITY.md](./SECURITY.md).
> Everything outward or irreversible stays human-gated; everything the agent does is an append-only `event` and is reversible; the protected set below is fail-closed.

---

## 1. The principle: actuate everything except the four protected domains

The agent is a **first-class operator of the app**, not a chatbot bolted onto the side.
Anything a human can do from the SPA, the agent can propose and (after the appropriate gate) perform - **except** four domains that are hard-denied for obvious safety reasons:

| # | Protected domain | Why it is off-limits to the agent | What the agent *may* still do |
| --- | --- | --- | --- |
| 1 | **VCS internals** (`lifeos-vcs`) | History is the source of truth; rewriting/deleting versions destroys auditability. | Commit *new* content versions (forward-only); read history/diffs. **Never** rewrite, branch-force, GC, or delete a version. |
| 2 | **Security & gating config** | The permission boundary cannot be edited by the thing it constrains. | Read the capability matrix. **Never** change gating rules, sandbox config, allow/deny lists, or its own permissions. |
| 3 | **OAuth / connections** (Nango) | Tokens and provider auth are the keys to your real accounts. | Use a `connectionId` to *read/draft* via the proxy. **Never** create/revoke connections, see tokens, or touch `connections.secret_enc`. |
| 4 | **API keys & secrets** | Self-evident: exfiltration / privilege escalation. | Nothing. Secrets never enter agent context (already enforced in [SECURITY.md](./SECURITY.md) §1). |

Everything else - tasks, trades-journal entries, topics, posts/drafts, campaigns, design briefs, module manifests' *runtime data*, dashboard layouts, saved views, navigation - is **in scope** for agent actuation.

This is deliberately the inverse of a deny-list product: the agent's reach is broad by default and narrowed only at the four cut-lines above, each of which maps to a fail-closed guard.

**Release-loop config promote/rollback (issue #98, `docs/HARNESS-LOOP.md`
§4) is human-typed-only, never agent/hook/cron-callable.** A learned
routing prior isn't literally "security & gating config" (row 2 above),
so the agent may draft a candidate via `POST /api/configs` - but the
`.../promote` and `.../rollback` routes are only ever called by
`harness config promote|rollback`, a CLI a human types. No tool in the
action registry (§2 below) wraps them, matching the same never-agent-
callable carve-out used for real order execution.

---

## 2. The typed action registry (how actuation works)

The agent never manipulates the DOM or the DB directly.
It emits **typed action calls** against a registry; the registry enforces the boundary, applies the change through the same `lifeos-api` routes the UI uses, and records an event.

```
agent ──► proposes ActionPlan = [ Action, Action, … ]
            │   each Action = { tool, args, scope }
            ▼
     capability matrix check  ──forbidden──► refuse visibly, log events('action.denied')
            │ allowed | gated
            ▼
     dry-run preview / diff  ──► human confirm (gated) | auto (allowed, reversible)
            │
            ▼
     execute via lifeos-api (entity/edge/event/llm/job routes)
            ▼
     events('action.applied', { reverse_patch })   ← append-only, reversible
```

- **Action tools are a closed set, split across two registries that must agree:**
  - **`server/agent/actionRegistry.js`** (backend, enforced by `/api/agent`) - `entity.create`, `entity.update`, `entity.list`, `entity.get`, `edge.create`, `edge.list`, `event.append`, `search.query`, `memory.recall`, `memory.network`, `web.scrape`, `pipeline.run`, `draft.create`, `config.draft`. This is the tool surface a real turn of the agent loop can actually call, gated per §3 of [AGENT-CORE.md](./AGENT-CORE.md).
  - **`frontend/src/lib/agentActions.js`** (browser console, client-side only) - `entity.create`, `entity.update`, `edge.create`, `draft.create`, `view.configure`, `dashboard.arrange`, `navigate`, `pipeline.run`, `module.requestBuild`, `search`. This backs the Cmd-K/`AIConsole.jsx` compiler (§3 below) and includes three tools with no backend equivalent (`view.configure`, `dashboard.arrange`, `navigate` - localStorage-level, no API route) plus `module.requestBuild`, which the backend registry does not expose.
  - The two registries are intentionally not identical (one drives server-side tool-calling, the other drives a browser-side compiled plan), but their *classification* of any name they share must agree - a parity test (in progress, another worker's addition) enforces that `classify()`/`classifyAction()` never diverge on overlapping tool names, so a tool can't be `allowed` in one surface and `gated`/`forbidden` in the other.
  There is intentionally **no** `vcs.rewrite`, `security.configure`, `connection.create`, or `secret.read` tool in either registry - the protected domains have no tool to call.
- **Classification** per action is `allowed` (reversible, internal → auto-apply), `gated` (outward/irreversible → human approve, reuses the [SECURITY.md](./SECURITY.md) §2 state machine), or `forbidden` (protected set → refuse).
- **Every applied action** writes `events('action.applied')` carrying a **reverse-patch** so it can be undone (§4).

---

## 3. Command-bar agent mode ("say it, preview it, do it")

The `Cmd-K` command bar ([PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md)) gains an agent mode: a natural-language instruction is compiled by the agent into an `ActionPlan`, shown as a preview, and executed on confirm.

- "Move every overdue task to this week and tag them urgent" → an `ActionPlan` of `entity.update` calls → preview diff → apply → undoable.
- "Draft a launch post from this design and queue it" → `draft.create` (gated, since publishing is outward) → Telegram/PWA approve.
- "Why is my trading dashboard empty?" → read-only `search` + `entity` queries, no mutation.

The same compiler powers `AIConsole.jsx`; the command bar is just a second entry point to the identical actuator.

---

## 4. Action ledger + one-click undo

Because broad actuation is only safe if it is trivially reversible, every agent mutation is reversible by construction.

- Each `events('action.applied')` row stores `{ tool, args, entity_id, actor:'agent' }` at the top level, with `attrs` carrying `{ tool, args, reverse_action, plan_id }` (`frontend/src/lib/agentActions.js::logApplied`) - `reverse_action` is the computed inverse tool call (or `null` when a tool genuinely has no inverse), and `plan_id` groups a multi-step `ActionPlan`'s applications for atomic undo.
- The UI renders an **agent activity ledger** (an "the agent did X, Y, Z" timeline) with an **undo** control per action and per `ActionPlan`.
- **Undo** applies the `reverse_patch` as a normal forward action (itself an event) - history is never rewritten, consistent with the append-only rule.
- Batch plans undo atomically (reverse order); a partially-applied plan can be rolled back to its pre-plan state.

This makes "let the agent reorganize my whole workspace" a safe operation: anything it does, you can see and revert in one click.

---

## 5. Capability matrix (single source of truth)

The real structure is three registries, not one canonical map:

- **`frontend/src/lib/capabilities.js`** - the legacy free-text guardrail layer (`LAYERS`, `canAI()`), used to route `AIConsole.jsx`'s natural-language intent to a coarse app surface (theme, dashboard, atlas-domains, …) and to gate that surface as `aiCanRead`/`aiCanModify`/`aiCanDelete`/`gated`.
- **`frontend/src/lib/agentActions.js`** - the typed action-tool registry (§2), entirely independent of `capabilities.js`: it does not import it, and its own `classifyAction()` is the only classifier the action registry uses.
- **`frontend/src/lib/capabilityMatrix.js`** - a read-only view, not a source of truth: `getCapabilityMatrix()` merges `agentActions.js`'s `ACTION_TOOLS`/`PROTECTED_TOOLS` with `capabilities.js`'s `LAYERS` into one flat list purely for display, and is consumed only by the read-only Profile view.

So there is no single canonical map "consumed by the action registry" - the action registry enforces its own closed set (§2) independently of the legacy layer registry; `capabilityMatrix.js` exists solely so `Profile.jsx` can show the user one combined picture of both.

- Rendered read-only in `Profile.jsx` so the user can *see* exactly what the agent can and cannot do, across both registries.
- The four protected domains appear as `forbidden` rows that **cannot be edited from the app** (only changed in code, behind review) - the matrix cannot be used to widen the agent's own reach.
- Forbidden actions **refuse visibly** in the console rather than silently no-op, so the boundary is always legible.

---

## 6. Dry-run / preview-diff for batch mutations

Any `ActionPlan` that touches more than one entity (or any single irreversible action) is shown as a **structured diff before apply**:

- a per-entity before/after view (reuse the VCS semantic-diff widgets where applicable),
- the full list of tools that will run and their classification,
- a single **Apply** (reversible plans) or **Approve** (gated plans) control.

This turns "the agent did something to 40 rows" from a leap of faith into a reviewed change set.

---

## 7. Build surface & verification

- **Backend:** no new privileged routes - the actuator rides the existing `entity`/`edge`/`event`/`llm`/`job` routes; the boundary is enforced by the registry + capability matrix + the absence of any protected-domain tool.
- **Frontend:** action registry + `ActionPlan` compiler in `lib/`, preview/diff component, ledger + undo UI, matrix view in `Profile.jsx` (tracked in [../frontend/FRONTEND.md](../frontend/FRONTEND.md) §4).
- **Must-pass checks:**
  - The agent can create/update/link/draft/navigate/reorganize, and every such action appears in the ledger and undoes cleanly.
  - A VCS rewrite, a gating-config change, a connection create/revoke, and a secret read each **refuse visibly** and log `action.denied` - and have **no tool to call** in the first place.
  - Every outward/irreversible action still routes through the gating state machine; nothing publishes/sends/executes without approval.
  - Undo of any single action and of a full batch restores prior state, verified against the `events` log.
