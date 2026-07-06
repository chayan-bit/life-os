# Harness loop - Event store, Eval+Gate, Observe, Release

> Closes the four "diagram gaps" by reusing existing harness infra at near-zero new always-on cost.
> Everything is logged, scored, and auto-improved; boundaries are enforced.

---

## 1. Event store (the foundation)

The `events` table doubles as the harness run-log (see [DATA-MODEL.md](./DATA-MODEL.md) §2.3).
A **`lifeos-sync-events` bridge** in the existing **Stop hook** joins:
- `~/.claude/logs/route.jsonl` (routing decisions),
- `~/.claude/metrics/costs.jsonl` (tokens/cost),
- `session-capture` (session outcome),

into **one append-only `events` row per run** (`run_id, tier, model, tokens_in/out, cost, latency_ms, error, outcome, eval_score, gated`).
Cloud rows are written by the Worker for bot runs. No new event store - the domain log *is* the run log.

**Implemented (issue #95):** `~/.claude/bin/lifeos-sync-events` (global
harness repo, not this one - see `~/.claude/SYSTEM.md`) is a third `Stop`
hook entry that runs after `session-capture`. It reads the Stop hook's
`session_id` off stdin, sums `metrics/costs.jsonl` rows matching that
`session_id` for `tokens_in/out`/`cost`/`model` (exact key - authoritative),
best-effort sums `logs/route.jsonl`'s `ms` field for entries whose
timestamp falls inside that session's cost-log window for `latency_ms`
(route.jsonl carries no `session_id`, so this is a time-window
approximation, not a real join - documented gap), and derives `outcome`
from a minimal transcript-tail heuristic (last ~50 lines, any `is_error`
tool-result block → `outcome:"error"` + a truncated `error` snippet, else
`"completed"`). `eval_score` is always `null` and `gated` always `0` -
Eval+Gate (§2) is a separate, unbuilt system. Posts `type:"harness.run"` to
the local `POST /api/event` (life-os's `lifeos-api`, port from
`LIFEOS_API_PORT`, default `8080`); fails open (silently skipped, exits 0)
if `lifeos-api` isn't running, matching `session-capture`'s own philosophy
of never blocking Stop.

---

## 2. Eval + Gate

- **LLM-as-judge on the Mac** via the existing `claude -p --model haiku` pattern, reusing archived `eval-harness` / `agent-eval` rubrics.
- **Sampled + ship-class only + content-cached** → cents/day.
- **`eval-gate`** wraps the commit/sync/job-complete boundary: below threshold → `gated=1`, ship blocked, rationale to Telegram.
- **Trade analysis is judged on data-grounding only - never PnL, never auto-acts.** (Reinforces the trading read-only guarantee in [SECURITY.md](./SECURITY.md).)

**Implemented (issue #96):** the pipeline `gate: "eval"` stage (the one
concrete gate boundary that exists - `verify` before `publish` in
`post-from-topic`, see §6) now uses a real judge. The lane ordering, wired in
`services/lifeos-drain/src/main.rs`, is **keyless-CLI-first**: if any local
agent CLI is detected on PATH (`lifeos_agents::detect()` - Claude Code,
Gemini CLI, Codex, OpenCode, Hermes, Antigravity, ...), the judge is
`AgentCliJudge` (`services/lifeos-drain/src/ai.rs`), which routes the judging
prompt through that CLI as a subprocess - no API key needed. Only when no CLI
is detected does it fall back to `HaikuJudge`
(`services/lifeos-pipelines/src/eval_gate.rs::HaikuJudge`, a direct `reqwest`
call to the Anthropic Messages API, gated on `ANTHROPIC_API_KEY`) and, when
neither a CLI nor a key is available, to the length-only heuristic from #92
(`HeuristicJudge`, the always-available last resort - the run must never fail
because judging failed). Content-cached
via a `blake3`-hashed `entities` row (`module='harness', type='eval_cache'`
- "zero new tables"); sampled via `PIPELINE_EVAL_SAMPLE_RATE` (default
0.2) using **deterministic** hash-based sampling (no `rand`, so the same
content always makes the same sample decision and tests stay
reproducible) to keep cost at cents/day. On a gate, `lifeos-drain` posts
the judge's rationale to `TELEGRAM_ADMIN_CHAT_ID` via the existing
`Notifier`/`TelegramNotifier` (already wired for module-build pings) -
`services/lifeos-drain/src/lib.rs::notify_pipeline_gated`; unset chat id
degrades to a local log line, matching the existing missing-bot-token
behavior. **Scope note:** only the existing pipeline eval-gate boundary is
upgraded in this pass - `lifeos-vcs` commits and libSQL sync are not
gated (commits are mechanical saves with no natural quality judgement,
and sync has no interception point at all); a future issue would extend
gating there if ever wanted.

---

## 3. Observe

A `harness observe` case beside `route-stats`:
- Reads `events` + quotas.
- Breaks down tokens / cost / latency / error / gated **per tier / module / phase**.
- Surfaces a **cloud free-tier + Haiku-spend meter** (so the always-on lane never surprises you).
- Feeds the per-module dashboards (see [PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §2) - same `events` source, different lens.

**Implemented (issue #97):** `GET /api/metrics`
(`services/lifeos-api/src/routes/metrics.rs`) gained `events_by_tier` and
`events_by_phase` (real SQL `GROUP BY`, the latter via
`json_extract(attrs, '$.stage')` - populated only for events that stamp
`attrs.stage`, i.e. pipeline stage events from #92/#96; most event types
have no phase concept yet, which is honest, not a bug). `~/.claude/bin/harness-observe`
(global harness repo, not this one - see `~/.claude/SYSTEM.md`), wired as
`harness observe`, renders this plus `entities_by_module` (the existing
module lens) as a `route-stats`-style report, and a real Haiku-spend-today
meter (`SUM(cost) WHERE model LIKE '%haiku%' AND` today, vs
`HARNESS_HAIKU_DAILY_BUDGET_USD`). It prefers `GET /api/metrics` when
`lifeos-api` is reachable, falling back to a direct `sqlite3` read of the
DB file otherwise (same fail-open shape as #95's `lifeos-sync-events`).
**Scope note:** the cloud free-tier meter is a deferred gap - there is no
Cloudflare Workers API wiring in this repo to read live Workers/Turso
usage, so `harness observe` prints an explicit "not tracked" line rather
than a fabricated number.

---

## 4. Release loop

A `lifeos-release` learner turns logged outcomes into **candidate** versioned `configs`:
- Includes a "learned reranking prior" as a JSON bias on `route_core.py`.
- **Shadow-replayed** against recent runs → **Telegram-approved** → `config promote` (human-gated) flips active atomically.
- Rollback = one pointer flip; every flip is an `event`.
- Nothing auto-activates: a candidate only goes live after explicit human `config promote`.

**Implemented (issue #98):** a new `configs` table
(`migrations/0006_release_configs.sql`, `kind='route_prior'` is the only
kind so far) holds versioned candidates (`draft -> shadow -> promoted |
rejected`); the active pointer per `kind` reuses the existing `vcs_refs`
named-pointer table (`kind='config_active'`, same atomic-flip shape
`lifeos-vcs` branches/tags already use, issue #84) instead of a second
pointer table. `POST /api/configs`, `.../:id/shadow`, `.../:id/promote`,
`.../rollback`, `GET /api/configs` (`services/lifeos-api/src/routes/configs.rs`)
implement the state machine; `promote`/`rollback` emit
`events(type='config.promoted'|'config.rolledback')`.

`~/.claude/bin/lifeos-release` (global harness repo, not this one) is the
learner: it freezes the same frequency-based generality-penalty formula
`route_core.py`'s live `attractor_prior()` already computes (see that
function's docstring) into a versioned `route_prior` candidate, shadow-
replays it against recent `route.jsonl` entries (re-ranks each entry's
surfaced assets with the candidate bias and summarizes how much would
have changed), and pings Telegram (`TELEGRAM_BOT_TOKEN`/
`TELEGRAM_ADMIN_CHAT_ID`, same env vars `lifeos-drain` uses, degrades to
a log line if unset) telling the human to review it. It never promotes
anything itself.

`~/.claude/bin/harness-config` (wired as `harness config
{list,promote,rollback}`) is the **only** thing that ever calls the
promote/rollback routes - always human-typed, never agent/hook/cron-
callable (docs/AGENT-CONTROL.md §1). On promote/rollback it also
materializes the now-active payload to
`~/.claude/logs/route_prior_active.json` - `route_core.py`'s new
`promoted_prior()` reads that static file (fail-open: missing/
unparseable → `{}`) and additively merges it with the live
`attractor_prior()` penalty before the rerank subtraction. This keeps the
hot routing path free of any DB/network dependency: the DB is the audit
trail and rollback history, the file is a disposable materialization of
"whatever is currently promoted." **Scope note:** only `kind='route_prior'`
exists today; `configs`/`vcs_refs(kind='config_active')` generalize to
other candidate-config kinds if ever wanted, with no schema change.

---

## 5. Cloud ↔ Mac queue (recap)

`jobs` table in Turso (not Cloudflare Queues); atomic `UPDATE … RETURNING` claim + reaper; `lifeos-drain` (Rust) runs headless harness jobs; triggered by a `launchd` poller while awake + on wake.
Turso is the only always-on piece.
See [DATA-MODEL.md](./DATA-MODEL.md) §2.5 for the claim SQL.

---

## 6. Agent pipelines integration

User/module-defined agent DAGs ([PLATFORM-SYSTEMS.md](./PLATFORM-SYSTEMS.md) §1) run through this same loop: each stage writes `events` (run_id, stage, tokens, outcome), is subject to Eval+Gate, and surfaces in Observe.
The Eval rubric for a publish pipeline gates the final outward stage.

---

## 7. Why this is near-zero new always-on cost

- The event store reuses logs you already produce.
- Eval is sampled + cached + Haiku.
- Observe is a read over `events`.
- Release is shadow-replay + a pointer.
- The only always-on piece is Turso (free tier) and the Worker (free tier).
