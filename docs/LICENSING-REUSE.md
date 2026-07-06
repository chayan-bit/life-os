# Licensing & reuse audit

> Question answered: **is it all free to use?** Yes for our purpose. Exactly one dependency is source-available rather than pure-OSS (Nango, ELv2), and it is free to self-host as an internal vault. The only thing that costs money is Claude API tokens (cents/day on Haiku).

Legend: 🟢 EXISTS (reuse) · 🔵 FORK (OSS, extend) · 🟡 BUILD · 🦀 Rust

---

## 1. License table

| Component | License | Free for us? | Caveat / free fallback |
|---|---|---|---|
| libSQL / sqld (Turso engine) 🦀 | MIT | ✅ fully | Self-host sqld = $0; Turso cloud free tier covers personal |
| Cloudflare Workers | proprietary, free tier | ✅ (100k req/day) | Fallback: Deno Deploy / a $5 VPS |
| Claude Agent SDK | MIT | ✅ SDK free | Token usage costs (Haiku ≈ cents/day) |
| grammY | MIT | ✅ | — |
| Drizzle ORM | Apache-2.0 | ✅ | — |
| Refine | MIT | ✅ | — |
| sqlite-vec | Apache-2.0/MIT | ✅ | — |
| Playwright | Apache-2.0 | ✅ | — |
| MiniLM-384 | Apache-2.0 | ✅ | — |
| Telegram Bot API | free | ✅ | — |
| BLAKE3 / FastCDC 🦀 | Apache-2.0/MIT | ✅ | — |
| Jujutsu (jj) 🦀 | Apache-2.0 | ✅ | Reference-only (`external/jj` submodule) - `lifeos-vcs` is modeled on jj's concepts but implemented from scratch, no `jj-lib` dependency (see docs/VERSIONING.md §2.4) |
| whisper-rs / candle 🦀 | MIT/Apache | ✅ | — |
| browser-use / browser-harness | MIT (verify at fork) | ✅ | — |
| readability | Apache-2.0 | ✅ | Vendored at `external/readability` but unused today - extraction uses the `scraper` crate instead (see `reading.rs`'s own doc comment) |
| **Nango** | **Elastic License 2.0** ⚠️ | ✅ self-host internal | Can't resell Nango-as-a-service (we don't). Fallback: fork (ELv2 allows), or OpenBao (MPL-2.0) vault + hand-rolled OAuth |

**Only paid thing:** Claude API tokens (unavoidable - the intelligence).
**Only non-pure-OSS dependency:** Nango (free for us; the resale clause never applies to Life OS).

---

## 2. Exists / Fork / Build / Rust classification (whole system)

### 🟢 EXISTS - reuse as-is
libSQL/Turso 🦀 · Cloudflare Workers · Claude Agent SDK · grammY · Drizzle · Refine · sqlite-vec · Playwright · MiniLM/memvec.py (Python) · memory-recall · mcp-figma/higgsfield/game-engines (on-demand) · route.jsonl/costs.jsonl/session-capture · Cytoscape.

### 🔵 FORK - open-source, extend to fit
Nango (TS) · browser-use/browser-harness (Python) · Jujutsu 🦀 · whisper-rs/candle 🦀 · Refine `dataProvider` (TS) · knowledge-atlas (JS, our own) · readability (JS).

### 🟡 BUILD - net-new (Rust where marked 🦀)
- 🦀 lifeos-api · lifeos-vcs · lifeos-ingest · lifeos-pipelines · lifeos-drain · broker-guard · bin/lifeos · marketplace sign/verify · metrics endpoint · Life OS Actions engine.
- JS: SPA `core/` (registry/router/render/views/command/analytics) · all module manifests · scaffold.js + validators · PWA service worker · Worker bot glue.
- Python: (reuse) memvec; optional later Rust port.
- SQL: migrations + a handful of generated VIRTUAL columns.

---

## 3. The "fork even if partial" opportunities
- **Nango** - run as-is; fork only if ELv2 ever conflicts.
- **Refine** - adopt; write one custom `dataProvider` against our generic-entity API.
- **browser-use** - wrap/extend as a gated tool.
- **Jujutsu** - fork its CAS/commit engine for lifeos-vcs rather than reimplementing git internals.
- **knowledge-atlas** - this *is* the fork base for `core/`.

---

## 4. Bottom line
- All structural infrastructure is free; the design has no blocking license problem.
- We build less than the original plan implied: Refine + Nango + Claude Agent SDK + grammY + libSQL collapse most of Phases 3-5 and half the core.
- The net-new infrastructure that remains is exactly the security/throughput-critical set worth writing in **Rust**.
