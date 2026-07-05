# Security & permissions

Layered, fail-closed boundaries. Auditability over speed; gate the irreversible.

---

## 1. Permission boundary (hard, layered)

- **Common DB:** harness = full RW; Haiku bot = full RW **within its workspace**.
- **Trading: read-only for any agent/bot.** No order tool registered anywhere. A fail-closed `broker-guard` PreToolUse hook denies place/modify/cancel/GTT even if the Kite MCP is mis-loaded; broker keys are read-scoped. Orders flow agent → `proposed_order` entity → Telegram approve → a **separate interactive `trade-exec`** (never agent/hook/cron-callable; typed confirmation). **No autonomous trading.**
- **Outward actions (social/marketing publish, email send, calendar write, drive share, browser actions):** `gated` agent tools produce **drafts only**; publishing requires Telegram (or PWA) approval, then a Worker/Mac executor performs the provider call.
- **Secrets:** OAuth tokens live in **Nango** (encrypted), the agent holds only a `connectionId`; the few non-Nango secrets (Kite daily token, WhatsApp) are envelope-encrypted in `connections.secret_enc`. **Never in agent context, never in logs.**
- **`events` append-only:** no UPDATE/DELETE route, so even the RW token cannot rewrite history.

**Implemented (issue #70):** `worker/src/bot.ts` registers a `bot.use` middleware, ahead of
every `bot.command(...)` handler, that inserts `events(type='bot.command', actor='bot',
attrs={text})` for every incoming `/command` - so bot activity (not just #66's
approve/deny transitions, which already recorded their own richer-typed events) is fully
reconstructable from `events`. Only ever an INSERT, matching the append-only rule above.

---

## 2. The gating state machine

```
agent tool (gated:true) ──► draft entity + events('*.drafted')
                              │
                              ▼
              Telegram/PWA approve  ──deny──► events('*.rejected'), stop
                              │ approve
                              ▼
        Mac/Worker executor ──► Nango proxy / browser actuator / trade-exec
                              ▼
                        events('*.published'|'*.sent'|'*.executed')
```
Every transition is an `event`. Nothing outward happens without a human approve.

**Implemented (issue #66):** the Telegram half of this diagram, up to (not including) the
executor. `worker/src/approvals.ts` - `approveEntity`/`denyEntity` take a `pending_approval`
entity id, verify it's still `pending_approval` **for the caller's workspace** before
transitioning (a stale/duplicate tap is a safe no-op, `outcome: 'already_resolved'`, not a
crash or a double-transition), then:
- **approve** → `status='approved'`, `events('${type}.approved')`, and enqueues
  `jobs(kind='execute_approval', payload={entity_id, entity_type})` - the Worker never calls
  Nango's proxy, the browser actuator, or trade-exec itself (it holds no provider tokens,
  ARCHITECTURE.md §3.1); real dispatch of that job is `services/lifeos-drain`'s
  `dispatch()`, not yet wired for this kind (its other kinds are stubs too) - **so "approve"
  today means "queued," not "executed."** The acceptance bar ("nothing outward executes
  without an approve tap") holds either way: nothing executes at all yet without one.
- **deny** → `status='denied'`, `events('${type}.rejected')` (this doc's exact `*.rejected`
  naming), no job.

`worker/src/bot.ts`: `/draft <text>` creates a draft and attaches an inline Approve/Deny
keyboard to its own confirmation message; `/pending` lists every `pending_approval` entity
in the workspace (one message + keyboard per item, including ones `draft_action`-backed
routes in `lifeos-api` created, not just bot-originated ones) since there's no push
notification yet when a new draft appears - deferred alongside the digest in
`docs/PLATFORM-SYSTEMS.md` §3. A `callback_query:data` handler parses `approve:<id>`/
`deny:<id>`, calls the above, and edits the message + answers the callback with the
outcome.

---

## 3. Self-extension & marketplace sandbox

Codegen and untrusted manifests run under three layers ([SELF-EXTENSION.md](./SELF-EXTENSION.md) §2):
1. `allowedTools`/`disallowedTools` + `permissionMode:"dontAsk"` (hard-deny).
2. PreToolUse hook confining writes to `modules/<id>/` (absolute, holds under bypass).
3. macOS Seatbelt sandbox (`failIfUnavailable:true`) confining Bash; credential files/env denied.
Plus: only `modules/` is writable (never `core/`); every install is a git commit (one `git revert` away); marketplace manifests are signature-verified and re-validated locally.

**Implemented (issue #72):** `server/scaffold.js` + `server/lib/{preToolUseHook,sandbox,
worktree}.js` build layers 1-3 above and the worktree/commit-as-install plumbing, driving a
real `query()` from `@anthropic-ai/claude-agent-sdk`. `server/test/preToolUseHook.test.js`
proves layer 2's "confines writes to `modules/<id>/`, holds under bypass" guarantee directly
(prefix-match traps, path traversal, and absolute-path escapes all denied) without needing
the SDK. The two validators this section's last sentence implies ("every install is a git
commit") still gate on are not wired in yet - `docs/SELF-EXTENSION.md`'s #72 note has the
full real-vs-deferred breakdown.

**Implemented (issue #73):** the install-as-commit step above is now additionally gated on
a schema-valid manifest - `server/lib/moduleManifest.js`'s Zod `ModuleManifest` drives
`options.outputFormat` on the same `query()` call, and a failed `safeParse` (or an id that
disagrees with the sandboxed directory) aborts the build before anything is committed, the
same fail-closed path as a Layer B hook denial. See `docs/SELF-EXTENSION.md` §3's note.

**Implemented (issue #74):** Validator 1 (§4) now runs for real - `server/validators/
structural.js` re-loads the file the agent actually wrote (via `vm.createContext`, no
filesystem/network globals exposed) and ajv-checks it, plus dup-type-id and dangling-view-
ref cross-checks. `scaffold.js` calls it before `commitAndMerge`; a failure aborts and
discards the worktree, same as a hook denial or a bad structured-output manifest. See
`docs/SELF-EXTENSION.md` §4's note for the full breakdown and a genuine pre-existing
inconsistency it found in `modules/learning`.

**Implemented (issue #75):** Validator 2 (§4) now runs for real too - `server/validators/
render.js` boots the real app stack (lifeos-api + Vite) as disposable child processes on
ephemeral ports against a scratch DB directory (never `lifeos.db`, `TURSO_URL`/`TURSO_TOKEN`
cleared so it can never sync), headless-Chromium-navigates it, and requires 0 console/page
errors plus a real `module-mounted:<id>` event before declaring success. `scaffold.js` calls
it after Validator 1 and before `commitAndMerge` - a failure aborts and discards the
worktree, same fail-closed chain as every other gate in this section. See
`docs/SELF-EXTENSION.md` §4's note, including a real live end-to-end run performed in this
session (no paid API key or git mutation needed, unlike #72's Agent SDK constraint).

**Implemented (issues #101/#102):** "signature-verified" above is now real -
`services/lifeos-api/src/marketplace_sign.rs` ed25519-signs a published
manifest's canonical JSON bytes with a platform key resolved from
`LIFEOS_MARKETPLACE_SIGNING_SEED` (never an implicit/unconfigured key -
publish/verify honestly 501 without it); `POST /api/marketplace/install`
re-verifies the stored signature against the stored manifest before
recording an install - a single tampered byte in either changes the signed
bytes and fails closed with 400, never a silent install. This covers the
marketplace half only; the render validator above stays local to the Node
scaffold layer (a headless-Chromium boot doesn't belong in an HTTP route),
and "re-validated locally" for a marketplace-sourced module still means the
same Validator 1 + Validator 2 chain this section describes.

---

## 4. Browser actuator containment

- Mac-only (trusted host), loaded on-demand, unloaded after.
- Sessions/cookies encrypted at rest like `connections`; never in agent context.
- Every state-changing action is gated; reads/scrapes are free.
- It can do anything a logged-in you can - therefore it is **never** allowed an un-gated outward action.

---

## 5. Tenancy isolation

- Every query is `workspace_id`-scoped at the API layer (RLS-style); a second workspace cannot see the first's rows.
- SaaS path: Turso database-per-workspace; per-workspace envelope key for non-Nango secrets.
- Derived state (`lifeos-derived.db`) and blobs (CAS) are local/keyed and never leak cross-workspace.
- **Real login/session (issue #100):** passwords are argon2id-hashed
  (`users.password_hash`, never plaintext); a `sessions` table backs
  refresh-token rotation - `POST /api/session/refresh` revokes the
  presented token and issues a fresh session, so a leaked refresh token
  has a bounded window even if never explicitly revoked, and a reused
  (already-rotated-away) token is rejected. Refresh tokens are stored
  only as a SHA-256 hash, never plaintext, mirroring password hashing's
  "the DB never holds a usable credential" principle. `POST /api/login`
  and `POST /api/register` return generic "invalid email or password"
  errors on failure - never revealing whether the email exists.
  `POST /api/account/set-password` (bootstrap for the personal account
  seeded before #100) only ever succeeds while `password_hash IS NULL`,
  so it can never overwrite an already-secured account's password.
- **Per-workspace envelope key + database-per-workspace (issue #104):**
  `workspaces.envelope_key_enc` is a random AES-256-GCM key generated on
  first use, itself envelope-encrypted under the server's master
  `LIFEOS_SECRET_ENCRYPTION_KEY` (`crypto::random_key`, migration
  `0011_workspace_envelope_key.sql`). `POST /api/workspace/provision-db`
  creates a dedicated Turso database via the Turso platform API
  (`TURSO_PLATFORM_API_TOKEN`/`TURSO_ORG_SLUG` - honestly 501 without
  them) and stores its auth token encrypted under that workspace's own
  envelope key, never the shared master key alone and never returned in
  any response (`workspace_databases`, migration
  `0012_workspace_databases.sql`). **No billing/quota gating exists
  anywhere in this path** - this is a self-hosted, bring-your-own-database-
  and-AI-model project; the `plans`/`subscriptions` catalog stub from
  §5's original control-plane migration was dropped entirely (migration
  `0013_remove_billing.sql`, `DROP TABLE IF EXISTS`) rather than built
  out, since nothing ever read it. `workspaces.plan` stays as a free-text
  label only, never checked by any route.

- **Workspace resolution trust boundary (audit #5):** `resolve_workspace`
  (`services/lifeos-api/src/auth.rs`) picks the tenant a request operates on.
  A verified JWT's `workspace_id` claim is always checked first.
  `LIFEOS_TRUST_WORKSPACE_HEADER` (config field `trust_workspace_header`,
  default `true`) controls what happens without one.
  When `true` (the local-first Mac default) an unauthenticated
  `X-Workspace-Id` header, or an explicit `workspace_id` in the request body,
  is trusted outright, falling back to the seeded default workspace - this
  is the current single-user behavior and is unsafe for a shared deployment,
  since any caller can claim any workspace.
  When `false`, identity must be proven: a request with no verified JWT gets
  401, never the default workspace.
  With a verified JWT, its `workspace_id` claim is authoritative; a header or
  body `workspace_id` that disagrees with the claim is rejected with 403
  rather than silently honored or silently overridden.
  **Any shared/SaaS deployment of `lifeos-api` must set
  `LIFEOS_TRUST_WORKSPACE_HEADER=0`.**

---

## 6. Must-pass verification (security)

- Order attempt via agent **and** via a mis-loaded Kite MCP → `broker-guard` denies both; no order tool registered.
- `events` UPDATE/DELETE → 404.
- Social publish / email send blocked without approval; reads succeed.
- Tokens absent from agent context and logs; a leaked-token grep finds nothing.
- A second workspace cannot read the first's entities.
- Break the scaffold template → validator fails cleanly, no partial register, worktree discarded.
