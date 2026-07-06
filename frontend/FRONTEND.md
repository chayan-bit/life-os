# Frontend - changes to align the SPA with `lifeos-api`

This document is the authoritative work-list for turning the React SPA from a polished showcase into a real client of `lifeos-api`.
It maps **every route declared in `src/lib/api.js`** to the concrete page/component change that makes it live, in dependency order.
Read `docs/ARCHITECTURE.md`, `docs/DATA-MODEL.md`, `docs/AGENT-CONTROL.md`, and `frontend/DESIGN.md` before non-trivial work here.

> Status: the base `lifeos-api` is built and verified (15 live routes, 2 queued, 3 planned).
> The frontend declares all of them in `API_ROUTES` but currently **calls only 4** (`/api/health`, `POST /api/entity`, `POST /api/register`, `POST /api/llm`), and even those bypass the shared client and send no tenant headers.
> The job below is wiring, not redesign - the shell, design system, and AI console already exist and are good.

---

## 0. Current state (what exists vs what is wired)

- **Shell is real:** `Layout.jsx` (sidebar nav, dark mode, mobile drawer), `BrandMark.jsx`, routing in `App.jsx`, the Neo-Brutalist design system in `index.css` + `frontend/DESIGN.md`. Keep all of it.
- **AI surface is real and partly wired:** `AIConsole.jsx` + `lib/ai.js` already call `POST /api/llm` through `apiCall`, route intent through `lib/capabilities.js`, and fall back to a deterministic plan offline. This is the seed of the Agent Control Plane (see `docs/AGENT-CONTROL.md`).
- **API Explorer is real and wired:** `APIExplorer.jsx` (embedded in `/integrations`) reads `API_ROUTES`, pings GET routes, and "tries" any route with its sample payload. It is the one surface that already exercises the whole API.
- **Integrations page is real (issue #55):** `Integrations.jsx` lists real `connections` rows from `GET /api/connections` and drives `POST /api/connections/session` + Nango's `openConnectUI` (`@nangohq/frontend`) + `POST /api/connections/complete` for the connect flow, and `DELETE /api/connections/:id` for disconnect.
- **Almost everything else is showcase:** Dashboard KPIs, the jobs queue, the harness telemetry table, the agent registry, the self-extension terminal, and the ingest clips are all hardcoded constants or `localStorage`, not backend data.

The legacy `core/` IIFE layer (the pre-React prototype) has been **removed**.
The React `apiCall` in `src/lib/api.js` now owns the tenant-header pattern it used to reference (see P0, done).

---

## 1. P0 - tenant + auth header propagation (blocks everything) - DONE (#13)

Nothing else can be trusted until every request carries the workspace and (optional) bearer token.

**Implemented:**
1. `apiCall(method, path, body)` in `src/lib/api.js` attaches, on every request:
   - `X-Workspace-Id: <workspace_id>` read from `localStorage['life_os_workspace_id']` (fallback `default-personal-workspace`).
   - `Authorization: Bearer <key_token>` when `localStorage['life_os_key_token']` is set.
   - Uses the tenant-header shape the removed `core/db.js` prototype defined, now owned by `lib/api.js`.
2. `key_token` is persisted under the canonical key `life_os_key_token` (exported as `KEY_TOKEN_KEY` from `lib/api.js`) both on `POST /api/register` success and on a matched-registered-user login; the demo identity (`chayan@lifeos.app`) clears it, since it has no backend-minted token.
3. The three raw-`fetch` call sites now go through `apiCall`: the health check and entity POST in `Database.jsx`, and the register call in `LoginPage.jsx`.
4. **Auth model decision: soft auth, kept as-is.** There is no `/api/login` on the backend and none is being added. `POST /api/register` is the only place a `key_token` (an HS256 JWT, see `services/lifeos-api/src/auth.rs::issue_token`) is minted; the React "login" form either accepts the hardcoded demo identity or validates an email/key pair against `localStorage['life_os_registered_users']` entirely client-side, then promotes that user's `key_token` to the canonical `life_os_key_token` slot apiCall reads. The backend independently verifies the JWT on every request that carries it (`resolve_workspace`: verified JWT claim > `X-Workspace-Id` header > body param > seeded default) - so client-side "login" never grants tenant access by itself, it only selects which already-issued token gets attached.

**Acceptance:** every network call in the app shows `X-Workspace-Id` (and `Authorization` when registered) in the Network tab; switching workspace in `Profile.jsx` changes the header and the data returned.

---

## 2. Live routes - page-by-page wiring

Each row is one work item: the route, where it lands in the UI, and what to replace.

| Route | Lands in | Replace |
| --- | --- | --- |
| `GET /api/metrics` | `Dashboard.jsx` KPI cards | hardcoded "4,892 / 843 / …" with live aggregates |
| `GET /api/entity` | `Database.jsx` table | seed mock rows + `localStorage` list with real, filtered, paginated entities |
| `GET /api/entity/:id` | new entity detail slide-over / page | nothing (does not exist yet) |
| `PATCH /api/entity/:id` | `Modules.jsx` Kanban + `Database.jsx` | `localStorage`-only status/attr edits; persist + emit `entity.updated` |
| `POST /api/entity` | `Database.jsx` create form | raw fetch → `apiCall`; drop client-built ids |
| `POST /api/edge` | entity detail "link" action | static mock edges |
| `GET /api/edge` | entity detail "relations" panel + graph view | static mock edges |
| `POST /api/event` | `HarnessLoop.jsx` promote, `Modules.jsx` social approve | `localStorage` event append |
| `GET /api/event` | `HarnessLoop.jsx` telemetry table | 4 hardcoded rows |
| `GET /api/jobs` | `Database.jsx` jobs queue | 4 hardcoded rows; add refresh/poll |
| `GET /api/agents` | `AgentHarness.jsx` detected-agents list | hardcoded `AGENT_REGISTRY` |
| `POST /api/llm` | `AIConsole.jsx`, `lib/ai.js` | already wired - keep, route through Agent Control actuator (§4) |
| `POST /api/module-request` | `SelfExtension.jsx` builder | `setTimeout` terminal animation; call the API, then poll `jobs`/`events` |
| `POST /api/register` | `LoginPage.jsx` | wire through `apiCall`; persist `key_token` canonically (P0) |
| `GET /api/health` | `Layout.jsx` status pill | raw fetch in `Database.jsx`; surface a global online/offline indicator |

**Queued routes (return 202, drain later) - build the trigger UI now:**

| Route | Lands in |
| --- | --- |
| `POST /api/ingest` | `VcsIngest.jsx`'s "Ingest Status" panel (issue #91) - file picker from `GET /api/entity?module=files&type=file`, submit, poll `GET /api/entity/:id` + `GET /api/entity?type=segment&parent_id=<id>` for status/segment count |
| `POST /api/pipeline/run` + `GET /api/pipeline/registry` | `Harness.jsx`'s "Pipelines" tab (`PipelineBuilder.jsx`, issue #94) - lists every registered pipeline's DAG stages from the registry route, triggers a run, and polls `GET /api/event?run_id=<job_id>` every 2s via the shared `usePipelineRun` hook (`lib/usePipelineRun.js`) for each stage's real status (`idle/running/done/failed/gated`) plus the run's terminal state (`completed/failed/awaiting_approval/gated`) - the `publish` stage always ends in `awaiting_approval`, never a fake "done". Run history comes from `GET /api/entity?type=pipeline_run`, expandable per row into that run's `GET /api/event?run_id=<attrs.run_id>` stage log. `Dashboard.jsx`'s original pipeline widget (issue #92) now calls the same shared hook instead of its own inline poll loop. |

**Planned routes (honest 501) - keep declared, show a "ships in phase N" state, do not fake success:**
`GET /api/broker/positions` (read-only; **no order route will ever exist**).

**Live (issue #86/#87):** `TimeTravel.jsx` (`Storage.jsx` → Versions tab) is wired to the real `lifeos-vcs` HTTP surface - `GET/POST /api/vcs/{history,commit,checkout,diff}` for per-file version timelines + real per-type diffs, `GET/POST /api/vcs/{refs,branch,tag,snapshot}` for read/forward-only branch/tag creation and snapshot inspection. `lib/vcsApi.js` is the thin wrapper. The pre-existing localStorage app-settings checkpoint UI in the same component is a separate, unrelated concern (browser-only preferences, not file content).

`POST /api/logout` (refresh-token revocation, issue #100) is also live: `App.jsx`'s `handleLogout` posts the stored refresh token before clearing local session state, best-effort (a local logout still proceeds if the API call fails, matching the app's general fail-open-to-offline posture).

### 2.1 Routes landed since this doc's original write-up

`frontend/src/App.jsx` now declares more routes than this table's original page-by-page list:

| Route | Page | Notes |
| --- | --- | --- |
| `/m/:id` | `InstalledModulePage.jsx` | the generic manifest-driven module page (§3) - one route serves every installed module. |
| `/graph` | `GraphView.jsx` | Cytoscape graph over `GET /api/edge`. |
| `/dashboards` | `ModuleDashboards.jsx` | per-module metric dashboards. |
| `/agent-ledger` | `AgentLedger.jsx` | the Agent Control Plane action ledger (docs/AGENT-CONTROL.md §4). |
| `/marketplace` | `Marketplace.jsx` | module marketplace publish/browse/install (issue #101/#102). |
| `/memory` | `MemoryInspector.jsx` | the cognitive-memory inspector (docs/AI-MEMORY.md §11). |
| `/refine-demo` | `RefineDemo.jsx` | the one place Refine's `dataProvider` is actually proven live (see §3 below - it is not used by the generic renderers). |

Back-compat redirects for the pre-merge information architecture: `/self-extension`, `/agent-harness`, `/harness-loop` → `/harness`; `/repository`, `/vcs-ingest` → `/storage`; unmatched paths and `/` → `/dashboard`.

---

## 3. Generic, manifest-driven views (the real product shape)

The current per-module hardcoded boards must become **generic renderers driven by module manifests**, per `docs/MODULES.md` and `docs/PLATFORM-SYSTEMS.md`.
This is what makes a new module render with zero bespoke UI.

**Implemented:**
- **Generic manifest-driven module pages exist** - `InstalledModulePage.jsx` (`frontend/src/core/ModuleManifestPage.jsx` underneath), routed at `/m/:id` (§2.1), renders any installed module's `views` from its manifest with zero bespoke screen code.
- **The React module registry is real** - `frontend/src/lib/moduleRegistry.js` implements the old `osRegisterModule` contract (`registerModule`/`getModules`/`getModule`/`hydrateFromStorage`) and dispatches `lifeos:module-mounted` (plus a per-module `module-mounted:<id>` event) on registration; `Layout.jsx` listens for `lifeos:module-mounted` to hot-add a module's nav entry without a reload.
- **The graph view is built** - `GraphView.jsx` at `/graph`, Cytoscape over `GET /api/edge`.

**Still work (honestly not done):**
- **Refine's `dataProvider` is proven in exactly one place: `RefineDemo.jsx`** (`frontend/src/lib/refineDataProvider.js`, a real `useList` call over the generic-entity API). The generic renderers described above (`frontend/src/core/ModuleManifestPage.jsx` and friends) do **not** use Refine - they call `apiCall` directly. Adopting Refine as the data layer for the generic renderer system itself remains open work, not something this proof-of-concept already delivers.
- Building out the full renderer set (`list, table, board, calendar, gallery, timeline, map`) to cover every manifest `views` kind consistently, beyond what's already exercised by shipped modules.

---

## 4. Agent Control Plane surface (new - see `docs/AGENT-CONTROL.md`)

`AIConsole.jsx` becomes the front door to **universal agent actuation**: the in-app agent can read and mutate everything the user can, through a typed action registry, **except** the protected set (VCS-rewrite, security/gating config, OAuth/connections, API keys/tokens), which are hard-denied.

**Implemented:**
- The capability matrix exists as `frontend/src/lib/capabilityMatrix.js` (`getCapabilityMatrix()`), a read-only merge of `agentActions.js`'s action-tool registry and `capabilities.js`'s legacy guardrail layers - see [AGENT-CONTROL.md](../docs/AGENT-CONTROL.md) §5 for the real (not single-canonical-map) structure. Rendered read-only in `Profile.jsx`.
- The action ledger is live at `/agent-ledger` (`AgentLedger.jsx`, §2.1), driven by `GET /api/event?type=action.applied` and `type=action.undone`.

**Still work:**
- Have the agent emit a structured **action plan** (typed tool calls), render it as a **dry-run preview/diff**, and require confirm before applying (outward/irreversible stays human-gated as today) - `actionPlanCompiler.js` compiles a plan today, but the dry-run preview/diff UI and one-click undo wiring in the console itself are not fully built out.
- Forbidden surfaces must visibly refuse in the console (not silently no-op) so the boundary is legible.

---

## 5. Cross-cutting cleanups

- Replace all remaining raw `fetch` with `apiCall`; one client, one place for headers, retries, and the `{ ok, data, error, offline }` envelope.
- Replace `localStorage` as a *source of truth* with `localStorage` as an *offline cache only*; the backend is canonical.
- Wire `DocsHub.jsx` to the real `docs/` markdown instead of in-file strings.
- Surface a global offline banner driven by `apiCall`'s `offline` flag + `/api/health`.

---

## 6. Suggested order (matches the issue backlog / build phases)

1. **P0 header propagation** (§1) - unblocker.
2. **Read paths**: metrics, entity list/detail, jobs, events, agents (§2) - makes the app show real data.
3. **Write paths**: entity create/update, edge, event, module-request (§2).
4. **Queued triggers**: ingest, pipeline (§2).
5. **Generic renderers + Refine dataProvider** (§3) - the structural payoff.
6. **Agent Control Plane** (§4) - the headline capability.
7. **VCS/broker** wiring when those phases ship (§2 planned).

Each numbered group maps to GitHub issues labeled `area:frontend` under the matching phase milestone.
