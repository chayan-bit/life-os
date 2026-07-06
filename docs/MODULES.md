# Modules - the plugin system

Every domain in Life OS is a **declarative module** - a manifest that says *what data exists* and *how to render it*, never DOM/router/DB code.
Rendering lives in the React SPA under `frontend/` (generic renderers in `frontend/src/core/`).
**Adding a module = a manifest + (rarely) one generated column. No bespoke table, ever.**

---

## 1. The manifest contract

There are two manifest layers, and they deliberately do not carry identical data:

- **`modules/<id>/module.js`** calling `osRegisterModule({...})` - the full manifest schema below, consumed only by the server build pipeline (self-extension's validators run it inside a `vm` sandbox). `osRegisterModule` is a sandboxed convention that pipeline provides, not a browser global - this file is never loaded by the SPA directly.
- **`frontend/src/lib/moduleManifests.js`** - a smaller, display-only subset (`entityTypes`, `views`, mostly) that the React SPA actually renders. This is the manifest the generic renderer system reads at runtime.

The two layers can and do diverge: `modules/projects/module.js` registers id `"projects"`, while the live SPA's equivalent is `frontend/src/lib/moduleManifests.js::CODING_MANIFEST` under id `"coding"`. This is an accepted, known state today (not a bug to silently hide) - the server-side manifest and the frontend manifest are authored and evolved somewhat independently until the two layers are unified.

The generalization of the knowledge-atlas's `atlasAdd` merge-by-id contract:

```js
osRegisterModule({
  id, name, icon, color /* light palette */, num, version,

  entityTypes: {
    <typeId>: {
      label, plural, icon,
      attrs: { <field>: { type:'text|number|boolean|date|select|enum|json|blob|file', enum?, required? } },
      display: { title, subtitle?, badge? },
      lifecycle: [/* statuses */],
    }
  },

  views: [ { id, label, kind:'list|table|board|calendar|gallery|timeline|map|graph|metric',
             type, groupBy?, sortBy?, filter?, columns? } ],

  events:      [ /* emitted event types */ ],
  botCommands: [ { cmd, help, handler } ],          // Telegram surface
  agentTools:  [ { name, schema, impl, gated? } ],  // harness surface; gated=true → draft/approve
  integrations:[ { provider, scopes, onConnect } ], // OAuth/API providers (via Nango)
  pipelines:   [ { id, stages:[…] } ],              // user/module agent DAGs (PLATFORM-SYSTEMS.md)
  actions:     [ { on, if?, run } ],                // "Life OS Actions" event-triggered automation
  metrics:     [ { id, source:'events', where, agg, bucket?, viz } ], // dashboards (no new storage)
  diff:        { <typeId>: fn },                    // per-type semantic diff for lifeos-vcs
  syncTargets: [ { kind:'notion', db, map } ],      // optional outbound sync, no lock-in
  seed:        [ /* optional starter entities/edges */ ],
});
```

A module manifest is consumed by the React SPA (`frontend/`); the self-extension builder writes new ones; the marketplace distributes them.

`ref` (a typed reference to another entity) and `secret` (a value that should never render/leave the app) are conceptually useful attr types this manifest contract does not yet support - `server/validators/module.schema.json` has no such enum values today. Cross-entity references are modeled via `edges` instead; there is no secret-typed attr anywhere in the schema, so a module needing to store one must not put it in `attrs` at all (see [SECURITY.md](./SECURITY.md)).

### View kinds (rendered generically from `entityTypes.display` + `views`)
`list` · `table` · `board` (Kanban) · `calendar` · `gallery` · `timeline` · `map` · `graph` (Cytoscape) · `metric`.
A trade → journal table + equity calendar; a task → Kanban; a topic → atlas article + connection chips; an asset → gallery + version timeline; a trip → itinerary timeline + map. (An entity's own detail view - the slide-over panel, `GenericDetail.jsx` - is not one of the manifest's `views` kinds; it's a generic per-entity affordance every module gets automatically.)

### Gating convention (applies to every `agentTool` and `botCommand`)
`gated: true` ⇒ the tool produces a **draft only**; execution requires Telegram (or PWA) approval, then a Mac/Worker executor performs the outward call. See [SECURITY.md](./SECURITY.md).

---

## 2. Day-1 seed modules

### 2.1 Learning / Study
The knowledge-atlas generalized to *any subject*.
- **Entity types:** `domain`, `topic`, `subtopic`, `resource`, `gap`, `question`.
- **attrs:** topic → `{summary, mastery(0-1), last_review, next_due, difficulty}`; resource → `{url, kind, blob_ref?}`; gap → `{description, severity}`.
- **Edges:** `topic ─connection→ topic` (cross-domain), `resource ─derived_from→ topic`, `gap ─blocks→ topic`, `topic ─thesis→ trade` (Trading link).
- **Events:** `study.review`, `topic.added`, `gap.opened`, `quiz.answered`.
- **Views:** atlas (list, domains), topics (table), gaps (list, gaps inbox), due (calendar, spaced-repetition due), graph (cross-domain topic connections).
- **Tools:** `learn.add_topic`, `learn.quiz` (examiner/teach-back), `learn.recall` (memvec).
- **Bot:** `add topic`, `quiz me`, `what's due`.
- **Migration:** atlas data files (`01_dsa.js … 13_gpu.js`) wrapped via an `atlasAdd → osRegisterModule` shim.
- **Implemented (issue #65):** `/topic <text>` (`worker/src/commands.ts::captureTopic`) captures a
  `learning`/`topic` entity with no status, so it also surfaces via `/inbox` until triaged.
  `/quiz` picks the topic untouched the longest (naive - no SM-2 scheduling yet, same
  naive-but-real precedent as `reading.rs`'s extraction). See `docs/PLATFORM-SYSTEMS.md`.

### 2.2 Tasks / Productivity
- **Entity types:** `task`, `project`, `schedule_block`.
- **attrs:** task → `{due, priority, estimate, tags[], project_id}`.
- **Edges:** `task ─depends_on→ task`, `task ─blocks→ task`, `project ─owns→ task`.
- **Events:** `task.created`, `task.completed`, `task.blocked`.
- **Views:** board (Kanban), list ("today"), calendar.
- **Tools:** `task.create`, `task.complete`, `task.today`.
- **Bot:** `/task`, `/done`, `/today`.
- **Implemented (issue #65):** `worker/src/commands.ts::captureTask`/`markDone`/`today`.
  `/today` returns open tasks with no due date or due on/before today (UTC day boundary) -
  no natural-language due-date parsing on capture yet, so most tasks are undated until the
  SPA/API sets `attrs.due`. `/done <id-suffix>` matches the tail of a task's id (shown in
  `/task`'s and `/today`'s replies), since a full ULID isn't practical to retype on a phone.

### 2.3 Coding / Projects
Seeded from a git scan of the ~27 repos in `04_Projects`.
- **Entity types:** `project`, `repo`, `gap`, `ci_run`, `review`.
- **attrs:** repo → `{path, remote, default_branch, last_commit, ci_state}`.
- **Edges:** `repo ─owns→ project`, `gap ─blocks→ project`, `project ─uses_asset→ design_file`.
- **Events:** `repo.scanned`, `ci.observed`, `review.requested`.
- **Views:** table (status), board (blocked/active/done), detail (harness links).
- **Tools:** `proj.status`, `proj.blocked`, `proj.scan`.
- **Integrations:** GitHub via Nango (issues/PRs/CI as entities; merge/release gated). See [INTEGRATIONS.md](./INTEGRATIONS.md).
- **Bot:** project status, what's blocked.

### 2.4 Trading (from scratch - NOT `ai-trade`)
- **Entity types:** `trade`, `setup`/`playbook`, `proposed_order`.
- **attrs:** trade → `{symbol, side, entry, exit, stop, target, qty, r_multiple, pnl, emotion, opened_at, closed_at}`.
- **Edges:** `trade ─thesis→ topic` (Learning), `trade ─derived_from→ setup`.
- **Events:** `trade.planned`, `trade.closed`, `setup.defined`.
- **Views:** table (journal), calendar (equity curve from `events`), detail (R-multiple breakdown).
- **Tools:** `trade.log_plan`, `trade.close`, `trade.pnl` - **all read/log only**.
- **HARD CONSTRAINT:** broker is **read-only for any agent/bot**. No order tool registered anywhere. `proposed_order` entity → Telegram approve → a **separate interactive `trade-exec`** (never agent/hook/cron-callable, typed confirmation). `broker-guard` PreToolUse hook fails closed on place/modify/cancel/GTT. See [SECURITY.md](./SECURITY.md).
- **Bot:** `/buy` (logs a *planned* trade), `/close`, `/pnl`.
- **Implemented (issue #65):** `/pnl` (`worker/src/commands.ts::pnl`, `worker/src/events.ts`)
  sums `attrs.pnl` across every `trade.closed` event for the workspace - a pure read over
  `events`, never a broker call, consistent with the read-only hard constraint above.
  `/buy`/`/close` (logging planned/closed trades) are deferred - not yet needed to exercise
  the read path, and better scoped alongside a real trade-logging flow later.

### 2.5 Social (multi-account, owned OAuth via Nango)
- **Entity types:** `social_account`, `post`, `reply`, `dm`, `mention`, `thread`.
- **attrs:** social_account → `{provider, handle, nango_connection_id}`; post → `{body, media_refs[], scheduled_for, status}`.
- **Edges:** `post ─publishes_to→ social_account`, `post ─uses_asset→ asset`, `mention ─relates_to→ campaign`.
- **Events:** `post.drafted`, `post.published` 🔒, `mention.received`, `dm.received`.
- **Views:** list (inbox: mentions/DMs), gallery (media posts), calendar (schedule), board (draft/approved/published).
- **Tools:** read (feeds/mentions/DMs/threads) = free; `social.draft` = free; `social.publish` = **gated**.
- **Providers:** Instagram, X, Reddit, Slack, WhatsApp - via Nango. **Kite-style exceptions** Slack/WhatsApp may use native APIs where Nango is awkward; see [INTEGRATIONS.md](./INTEGRATIONS.md).
- **Bot:** `/inbox`, `/draft`, approve/deny buttons.

### 2.6 Marketing
- **Entity types:** `campaign`, `content`, `audience`/`segment`, `lead`, `channel`.
- **attrs:** campaign → `{goal, budget, start, end, kpis}`; content → `{copy, asset_refs[], channel, utm}`.
- **Edges:** `content ─publishes_to→ social_account`, `content ─uses_asset→ asset`, `lead ─same_as→ contact` (Email).
- **Events:** `campaign.launched` 🔒, `content.sent` 🔒, funnel/UTM metrics as events.
- **Views:** calendar (content), table (leads), funnel (metrics from `events`).
- **Tools:** draft with `copywriting` + `marketingskills-ai-agents` skills; outward sends gated.
- **Bot:** campaign status, draft content, approve sends.

### 2.7 Design (Figma read+write + Higgsfield + asset library)
- **Entity types:** `design_file` (Figma ref), `component`, `token`, `asset` (exported media), `brief`.
- **attrs:** asset → `{kind, mime, blob_ref, dims, source}`; design_file → `{figma_url, last_synced}`.
- **Edges:** `asset ─uses_asset←` anything; `component ─derived_from→ design_file`.
- **Events:** `asset.generated`, `design.synced`, `version.created`.
- **Views:** gallery (assets), table (component library), detail (Figma inspect).
- **Tools:** read/inspect Figma (mcp-figma), generate media (mcp-higgsfield), build design system (`figma-generate-library`), implement to code (`figma-implement-design`).
- **Integration MCPs loaded on-demand via mcp-multiplexer, unloaded after.** Assets flow into Marketing/Social via edges and are versioned by `lifeos-vcs`.

---

## 3. Extended modules (owned-integration; ride Phase 3+)

> All replace the claude.ai MCP connectors with **owned OAuth via Nango** - see [INTEGRATIONS.md](./INTEGRATIONS.md). Zero new tables.

### 3.1 Email (Gmail, owned Google OAuth)
- **Entity types:** `email_thread`, `email`, `contact`, `mail_label`.
- **attrs:** email → `{from, to[], subject, snippet, body_ref(blob), ts, label_ids[], unread}`.
- **Edges:** `email ─relates_to→ task/project`, `contact ─same_as→ lead`, `thread ─derived_from→ topic`.
- **Events:** `email.received`, `email.triaged`, `email.drafted`, `email.sent` 🔒.
- **Views:** list (inbox), detail (thread), board (triage now/later/done).
- **Tools:** `gmail.sync|read|search` (free), `gmail.draft`, `gmail.send` 🔒.
- **AI:** triage (label + summarize + suggest action), draft replies; send gated.

**Implemented (issue #56):** `POST /api/gmail/sync` (free) materializes Gmail messages as `email`/`email_thread`
entities, idempotently (`services/lifeos-api/src/routes/gmail.rs`). The triage board's now/later/done columns are
driven by the entity's top-level `status` column (not `attrs`) - `GenericBoard`'s drag-to-move only PATCHes
top-level fields, so triage state has to live there to persist. The live SPA's Email module
(`frontend/src/lib/moduleManifests.js::EMAIL_MANIFEST`, routed at `/m/email`) renders inbox/triage/threads through
the generic renderer system with zero bespoke view code, plus a generic "Sync inbox" action
(`ModuleManifestPage.jsx`'s `manifest.sync`, reusable by Calendar/Notion/Slack later). `email.sent` stays gated
through the existing `/api/gmail/send` draft-only path (#53) - not duplicated here. `contact`/`mail_label`
materialization and reply-drafting are deferred; the triage board (the acceptance criterion) works end to end.

### 3.2 Calendar (Google Calendar, owned)
- **Entity types:** `calendar`, `calendar_event`.
- **attrs:** event → `{title, start, end, attendees[], location, recurrence, source_uid}`.
- **Edges:** `event ─blocks→ task`, `event ─relates_to→ trip`, `event ─owns→ milestone`.
- **Events:** `cal.synced`, `cal.created` 🔒, `cal.updated` 🔒.
- **Views:** calendar, list (agenda), today.
- **Tools:** `cal.sync|read` (free), `cal.create|move` 🔒.
- **AI:** "schedule X around my free slots" → drafts event; daily agenda in digest.

**Implemented (issue #57):** `POST /api/calendar/sync` materializes Google Calendar's `events.list` proxy response as
`calendar_event` entities, idempotently (`services/lifeos-api/src/routes/calendar.rs`), keyed on the provider's own
event id (`source_uid`) so re-syncing never duplicates. `POST /api/calendar/move` was added alongside the existing
`POST /api/calendar/create` (#53) - both only ever create a `pending_approval` draft entity
(`calendar_create`/`calendar_move`) and have no code path to Calendar's insert/patch APIs. The live SPA's Calendar
module (`frontend/src/lib/moduleManifests.js::CALENDAR_MANIFEST`, routed at `/m/calendar`) renders calendar/agenda
views through the generic renderer system, reusing the same `manifest.sync` "Sync events" action introduced for
Email (#56). `GenericCalendar` is a read-only agenda view with no drag-to-move affordance, so gating `move` is
enforced structurally the same way as `send`/`act` elsewhere: the UI has no path to it beyond drafting.
`calendar`(container)-entity materialization and the approve→execute queue for
`calendar.move.drafted`/`calendar.create.drafted` are deferred to the Bot-phase executor work.

### 3.3 Files (Drive + local, versioned by lifeos-vcs)
- **Entity types:** `file`, `folder` (reuse `asset` for media).
- **attrs:** `{name, mime, size, blob_ref, drive_id?, version_no, parent_folder}`.
- **Edges:** `file ─uses_asset←` anything; version lineage via `events`.
- **Events:** `file.imported`, `version.created`, `file.shared` 🔒.
- **Views:** gallery, table, detail with **version timeline + semantic diff** (see [VERSIONING.md](./VERSIONING.md)).
- **Tools:** `drive.sync|read` (free), `file.commit|diff|checkout` (free, local), `drive.upload|share` 🔒.
- **Key value:** Drive files gain real semantic version history Drive itself doesn't provide.

**Implemented (issue #58):** `POST /api/drive/sync` materializes Google Drive's `files.list` proxy response as `file`
entities, idempotently (`services/lifeos-api/src/routes/drive.rs`), keyed on the provider's own file id (`drive_id`).
`POST /api/drive/share` was added alongside the existing `POST /api/drive/upload` (#53) - both only ever create a
`pending_approval` draft entity and never call Drive's upload/permissions APIs.
`POST /api/files/commit` (`services/lifeos-api/src/routes/files.rs`) is the first real slice of `lifeos-vcs`
(docs/VERSIONING.md §2.1/§2.3): it hashes text content with `lifeos_vcs::hash_bytes` (BLAKE3), upserts the `file`
entity's `blob_ref`/`version_no`, and appends a `version.created` event whose `parent_blob_ref` chains to the prior
version - version history is exactly `GET /api/event?entity_id=<id>&type=version.created`, no separate table, per
§2.3. Re-committing identical content is rejected (400) rather than creating a no-op version. The live SPA's Files
module (`frontend/src/lib/moduleManifests.js::FILES_MANIFEST`, routed at `/m/files`) browses files through a
generic table view (name/type/version/folder columns) plus the shared "Sync Drive" action. FastCDC chunking for
large binary blobs, per-type semantic diff, snapshot/branch/tag, and a dedicated version-timeline UI are deferred -
this ships the content-addressed commit/history data model and the free read/local-commit + gated upload/share
tool surface the acceptance criterion calls for.

### 3.4 Notion (migrate-in, two-way, owned)
- **Entity types:** `note`/`doc`, `notion_db`, `notion_page` (mirror).
- **Edges:** `note ─mirrors→ notion_page`.
- **Events:** `note.synced`.
- **Flow:** import pages/databases → entities; two-way `syncTargets`; migrate gradually, then **deprecate Notion for real.**

**Implemented (issue #59):** `POST /api/notion/sync` materializes Notion's `/v1/search` results
(`services/lifeos-api/src/routes/notion.rs`): each page becomes a `notion_page` mirror entity (raw Notion state)
plus a native `note` entity - the one a user actually edits - linked by a `note ─mirrors→ notion_page` edge; each
database becomes a `notion_db` entity. All three are idempotently keyed on the provider's own id. "Edits propagate
back" is `POST /api/notion/push`: gated the same way as every other outward write - it only ever creates a
`pending_approval` draft carrying the note's current title/content, never calls Notion's page-update API. Reachable
from the live SPA via `EntityDetailPanel`'s "Push to Notion" button (shown for `notion.note` entities) rather than a
bulk manifest action, since a push is per-entity. `frontend/src/lib/moduleManifests.js::NOTION_MANIFEST`
(routed at `/m/notion`) browses notes/mirrored pages/databases through the generic list renderer plus the shared
"Sync from Notion" pull action. Deferred: real Notion block-content sync (only titles/metadata are mirrored today),
the `syncTargets` config-driven two-way engine, conflict resolution for concurrent edits, and the approve→execute
queue that would actually write `notion_push` drafts back out - the same Bot-phase boundary as every other gated
write in this milestone (#52/#53/#56/#57/#58).

### 3.5 Slack (owned)
- **Entity types:** `channel`, `message`.
- **Tools:** read free; `slack.post` 🔒. Doubles as a second capture/notify surface alongside Telegram.

**Implemented (issue #60):** `POST /api/slack/sync` (`services/lifeos-api/src/routes/slack.rs`) materializes
`conversations.list` as `channel` entities and each channel's `conversations.history` as `message` entities,
idempotently keyed on Slack's own channel id / message `ts` (Slack's per-channel-unique message timestamp) - both
capture, and re-syncing never duplicates. `POST /api/slack/post` (#53) is unchanged: gated by construction, only
ever creates a `pending_approval` draft and has no code path to `chat.postMessage`. The live SPA's Slack module
(`frontend/src/lib/moduleManifests.js::SLACK_MANIFEST`, routed at `/m/slack`) browses channels/messages through the
generic list renderer plus the shared "Sync channels" pull action. Thread replies, reactions, and the
approve→execute queue for `slack_post` drafts are deferred to later work.

### 3.6 Reading
- **Entity types:** `source` (RSS/site/author), `article`, `highlight`, `read_note`.
- **attrs:** article → `{url, title, author, published, text_ref(blob), read_state, est_minutes}`; highlight → `{quote, t_offset, color}`.
- **Edges:** `article ─derived_from→ topic`, `highlight ─supports→ note`, `article ─cites→ article`.
- **Events:** `article.saved`, `article.read`, `highlight.created`.
- **Views:** list (read-later), detail (reader + highlights), board (unread/reading/done).
- **Tools:** `read.save <url>` (fetch+parse via readability; `browser-use` for paywalled), `read.highlight`.
- **AI:** summarize, extract highlights, link to learning topics; PDFs feed the media pipeline → "find the article where I read about X."

**Implemented (issue #61):** `POST /api/reading/save` (`services/lifeos-api/src/routes/reading.rs`) is free - fetching
a public URL needs no owned credential, so unlike every prior module there is no Nango connection gate here. It
fetches the URL via `reading::ArticleFetcher` (real `HttpArticleFetcher`/`reqwest`, or `mock::MockArticleFetcher` in
tests), extracts title + paragraph text with the `scraper` crate (a lighter, dependency-thin stand-in for the
vendored Mozilla Readability.js submodule - real Readability-quality extraction and `browser-use` for paywalled
sites are deferred), computes a naive extractive summary (first two sentences - real AI summarization stays
available via the existing `POST /api/llm` route, not auto-triggered here to keep `save` fast/deterministic/free to
test), and idempotently upserts a `source` entity (keyed by domain) plus an `article` entity (keyed by
`lifeos_vcs::hash_bytes(url)`, reusing the `lifeos-vcs` crate first wired for #58's file versioning). On first save
only, it naive-keyword-links the article to any existing `learning` topic whose title appears in the article's
title/excerpt via an `article ─derived_from→ topic` edge (deterministic substring match, not embeddings). `POST
/api/reading/highlight` is free too - capturing a quote is local and reversible - and 404s if the target article
doesn't exist. The live SPA's Reading module (`frontend/src/lib/moduleManifests.js::READING_MANIFEST`, routed at
`/m/reading`) browses articles/highlights/sources through the generic list renderer; unlike other modules there is
no bulk "sync" button since articles are saved one URL at a time, not polled from an inbox. Deferred: real
Readability-quality parsing, `read_note`, the `article ─cites→ article` edge, `article.read`, the reader UI itself
(detail/board views), and AI-generated (non-naive) summaries/highlight-extraction.

### 3.7 Travel
- **Entity types:** `trip`, `leg` (flight/train/drive), `booking` (hotel/ticket), `place` (POI).
- **attrs:** trip → `{name, start, end, destination[], budget, status}`; booking → `{provider, confirmation, cost, file_ref}`.
- **Edges:** `leg ─relates_to→ calendar_event`, `booking ─uses_asset→ file`, `place ─derived_from→ article`.
- **Events:** `trip.created`, `booking.added`, `itinerary.changed`.
- **Views:** timeline (itinerary), calendar, map (place pins), table (bookings/costs).
- **Tools:** `travel.plan` (AI drafts itinerary from constraints), parse confirmation emails (Email module → auto-create bookings). Flight/hotel *booking* = 🔒 browser actuator.

**Implemented (issue #62):** `trip`/`leg`/`place` are plain user-authored entities - created through the generic
`POST /api/entity` like Trading/Social/Marketing, with no bespoke route file (`services/lifeos-api/src/routes/travel.rs`
holds only the two genuinely special actions below). `POST /api/travel/parse-emails` is free: it scans
already-synced `email` entities (issue #56) for booking-shaped subject/snippet text via a naive, deterministic
keyword match (`flight`, `itinerary`, `confirmation`, `reservation`, `hotel`, `booking`, `e-ticket`, `boarding pass`
- real AI-driven extraction deferred, same "real but simple" precedent as #61's `naive_summary`), and idempotently
derives a `booking` entity per matching email (id keyed on a BLAKE3 hash of the email's own id), pulling out a
best-guess confirmation code with a similarly naive alphanumeric-token heuristic. `POST /api/travel/book` is gated
the same way as every other outward write in this repo: it only ever creates a `pending_approval` draft via
`integrations::draft_action`, which has no reference to `state.browser` at all - there is no code path from a
booking request to an actual purchase. The live SPA's Travel module (`frontend/src/lib/moduleManifests.js::TRAVEL_MANIFEST`,
routed at `/m/travel`) reuses the `GenericTimeline`/`GenericMap` renderers first built in #27 (only just wired into
`ModuleManifestPage.jsx`'s `KIND_RENDERERS` here) for the Timeline (legs) and Map (places, via Leaflet/OpenStreetMap,
no API key needed) views, plus the shared sync-button mechanism for "Parse confirmation emails" since trips/legs/
places have no external inbox of their own. Deferred: real flight/hotel search+purchase execution (the
approve→execute queue doesn't exist anywhere in this repo yet, same Bot-phase boundary as every prior gated-write
module), the `leg ─relates_to→ calendar_event` / `booking ─uses_asset→ file` / `place ─derived_from→ article` edges,
AI-driven `travel.plan` itinerary drafting, and the `itinerary.changed` event.

---

## 4. Future domains
Health, finance, CRM, fitness, journaling, … are added **the same way** - via the self-extension builder ([SELF-EXTENSION.md](./SELF-EXTENSION.md)) or the marketplace.
**You never enumerate domains up front.**
Reading and Travel above are deliberately specified as the first *self-extension demos* - proof the builder produces production modules end-to-end.
