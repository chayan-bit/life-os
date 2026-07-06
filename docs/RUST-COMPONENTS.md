# Rust components

> Principle: **Rust where it is security- or throughput-critical; JS/TS at the browser & orchestration edge; Python where reuse already exists (memvec).**
> libSQL/Turso (our DB) is itself Rust, so the Rust services compose with it natively (no FFI friction).

---

## 1. Build-ourselves, in Rust (the native core)

| Service | Why Rust | Key crates |
|---|---|---|
| **lifeos-api** | The single DB-token owner + security boundary; workspace scoping; secret injection; metrics agg. The trust anchor. | `axum`, `tokio`, libSQL client, `serde`, `jsonwebtoken` |
| **lifeos-vcs** | Tight hashing/IO loops, large-file throughput, memory-safe object store. | `blake3`, `fastcdc`, `jj-lib`, `object_store`, `rusqlite` |
| **lifeos-ingest** | Media→text orchestration, transcription throughput. | `whisper-rs`, `reqwest` (Haiku vision captioning), `pdf-extract`, `zip`+`quick-xml` (docx), `tesseract` CLI subprocess (OCR), (opt) `candle` CLIP |
| **lifeos-pipelines** | Hot event loop dispatching agent DAGs + Life OS Actions. | `tokio`, libSQL client; shells to Agent SDK |
| **lifeos-drain** | Atomic job claim + reaper; crash recovery; claims `module_requests` and spawns the Node build entry (`build/run.js` multi-tier pipeline by default, `scaffold.js` when `LIFEOS_BUILD_PIPELINE=0`). | libSQL client, `tokio` |
| **lifeos-agents** | Keyless local agent-CLI router: detects installed coding-agent CLIs on PATH (Claude Code, Gemini CLI, Codex, OpenCode, Hermes, Antigravity, ...) and runs judging/captioning/pipeline-stage/memory-consolidation completions through them as no-shell subprocesses - no API key needed. Primary AI lane for `lifeos-drain` and `lifeos-api`'s `/api/agents` + `/api/llm`. | `tokio::process`, `serde` |
| **lifeos-memory** | Event-sourced cognitive memory: episodic/semantic/procedural layers, activation-scored recall, sleep-job consolidation (migrations `0017`-`0019`). | libSQL client, `tokio` |
| **lifeos-actions** | Event → action rules engine ("GitHub Actions for everything"); scans `events` each drain tick and enqueues `jobs` for declared rules. | libSQL client, `tokio` |
| **lifeos-cli** (binary `lifeos`) | Thin allow-listed CLI (one statically-linked binary beats curl wrappers). | `clap`, `reqwest` |
| **marketplace sign/verify** | ed25519 signing inside lifeos-api. | `ed25519-dalek` |

**Planned, not built:** `broker-guard` - a fail-closed PreToolUse guard scoped to broker order verbs. A first attempt was built then fully reverted (commit `f3bd18d`, issue #99): registered as a broad PreToolUse hook (matcher `*`), it fail-closed on almost every ordinary tool call, not just order verbs, and locked out the harness session that wired it in. Today's read-only-trading guarantee is enforced instead by `KiteClient` exposing no order method, no order route being registered, and `server/lib/routeAllowlist.js` denying `broker`/`order` paths by omission - see `docs/SECURITY.md` §1/§6. A future, narrowly-matcher-scoped `broker-guard` remains the target spec for closing the one residual gap: an independently loaded order-capable MCP server that nothing in this repo currently intercepts.

---

## 2. Already Rust (reuse for free)

| Component | Note |
|---|---|
| **libSQL / sqld / Turso engine** | Our canonical DB + embedded-replica sync are Rust under the hood at zero effort. |
| **Jujutsu (jj)** | Fork its CAS/commit model for lifeos-vcs. |
| **BLAKE3, FastCDC, whisper-rs/candle, octocrab** | Rust crates/libs we depend on. |

---

## 3. Stays JS/TS (do not rewrite - YAGNI)

| Component | Why JS |
|---|---|
| SPA `frontend/` (registry, router, render, views, command bar, analytics) | Runs in the browser. |
| Module manifests (`modules/*/module.js`) | Declarative JS is the manifest format; the structural validator must run JS. |
| Worker bot (grammY) | Cloudflare Workers + grammY are JS; a Rust/WASM `workers-rs` port would lose grammY for no real gain. |
| `scaffold.js` + validators | Orchestrates the Agent SDK; manifests are JS. |
| Refine / Drizzle | TS libraries we adopt. |
| Nango | TS service we self-host (not ours to rewrite). |
| PWA service worker | Browser/JS. |

---

## 4. Stays Python (reuse)

| Component | Why |
|---|---|
| **memvec.py** / **memory-recall** | Existing harness infra, reused unchanged. A Rust+`candle` port is a *later* optimization, not a phase-1 need. |

---

## 5. Forked OSS (extend to fit, per the "fork even if partial" rule)

| Repo | Lang | Use |
|---|---|---|
| **Nango** | TS | OAuth vault + proxy; fork only if ELv2 ever conflicts. |
| **browser-use / browser-harness** | Python | No-API browser actuator; wrap as a gated tool. |
| **Jujutsu (jj)** | 🦀 Rust | VCS commit/CAS model for lifeos-vcs. |
| **whisper-rs / candle** | 🦀 Rust | Transcription in lifeos-ingest. |
| **Refine** | TS | Admin shell + generic views; write one custom `dataProvider`. |
| **knowledge-atlas** | JS | The fork base for `core/` (our own code). |
| **readability** | JS | Article extraction for Reading. |

---

## 6. Summary

- **New Rust services (the `services/` workspace, 9 crates):** lifeos-agents, lifeos-api, lifeos-vcs, lifeos-ingest, lifeos-pipelines, lifeos-actions, lifeos-memory, lifeos-drain, lifeos-cli (binary `lifeos`), plus marketplace signing inside lifeos-api.
- **Planned, not built:** broker-guard (see §1).
- **Rust we reuse:** libSQL/Turso, jj, blake3, fastcdc, whisper-rs, octocrab.
- **Not Rust:** frontend SPA, manifests, Worker bot, scaffolder, Refine, Drizzle, Nango (TS); memvec (Python); browser-use (Python).
