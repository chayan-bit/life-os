# AI memory - the cognitive memory architecture

Memory for an AI that manages a whole life is not "a vector database you dump things into."
It is a **cognitive-architecture** problem: years of heterogeneous, cross-domain, temporal data, retrieved under a fixed context window, with auditable provenance and tenant isolation.
This doc specifies Life OS's memory as a **layered, event-sourced cognitive memory** built on the substrate the system already has (`events`, `entities`, `edges`, derived FTS5 + sqlite-vec), plus the three pieces that turn that substrate into a brain: consolidation, activation-scored retrieval, and a context compiler.

> Design grounding: this synthesizes the field's best ideas - Stanford Generative Agents (reflection + recency/importance/relevance scoring), Zep/Graphiti (bi-temporal knowledge graph), Letta sleep-time compute, ENGRAM (typed partitioning), ActiveGraph/ESAA/SSGM (event-sourcing for agent memory), and ACT-R (activation decay) - onto Life OS's existing append-only spine.
> The novel angles versus the standard "LLM-writes-structured-notes" or "flat top-k RAG" approach are called out as **Novel:** inline.

> Invariant alignment: nothing here relaxes [SECURITY.md](./SECURITY.md) or [DATA-MODEL.md](./DATA-MODEL.md).
> `events` stays append-only; the derived store stays un-synced; secrets never enter memory; agent memory-writes are themselves events (reversible via the [AGENT-CONTROL.md](./AGENT-CONTROL.md) ledger).

---

## 1. Four memory layers, mapped to primitives Life OS already has

The reference model is the cognitive-science taxonomy (Soar/ACT-R), not RAG:

| Layer | "What it is" | Life OS primitive |
| --- | --- | --- |
| **Working memory** | the live context window + a small scratchpad | the per-turn compiled context (§6) |
| **Episodic** | timestamped "what happened" | the append-only `events` log (the source of truth) |
| **Semantic** | distilled "what is true" - facts, profiles, entity summaries | `entities` + `edges` (a temporal knowledge graph) + derived `memory_nodes`/`memory_summaries` |
| **Procedural** | "how to behave" - learned rules/preferences | a procedural-memory store feeding the system prompt (§8); ties to the harness Release loop |

A whole-life system needs all four plus a maintenance loop between them.
A flat vector store is a weak version of one.

---

## 2. Core invariant - memory is a derived, rebuildable projection of the event log

**Novel:** Most memory systems make the memory store *primary* and mutate it in place (Mem0's ADD/UPDATE/DELETE, A-MEM's note evolution, LangMem's manager).
That is exactly what produces contradictory, un-auditable, hallucinated memory and painful multi-tier sync.

Life OS inverts this.
The append-only `events` log is the **single source of truth**; *all* semantic memory - summaries, embeddings, profiles, the FTS5/vector indices, the whole graph projection - is a **read model that can be deleted and recomputed from events** (event-sourcing / CQRS applied to memory).

This gives, for free:
- **Provenance:** every fact carries `source_event_ids`; nothing is a memory without a traceable origin (no fabricated memory).
- **Non-destructive forgetting:** you never delete a fact; you supersede it (§7) and let it decay in ranking.
- **Conflict reconciliation by replay:** the documented sync model (last-push-wins; `events` as reconciliation truth) extends directly to memory.
- **Counterfactual forking:** branch the log at any event and recompute ("what would my month look like if I hadn't taken that trade?").

This pattern is barely in the published literature (ActiveGraph, ESAA, SSGM, all early 2026) - Life OS is ahead of the field here precisely because the append-only invariant was already committed to.

**Pitfalls this section must engineer around (validated by the SOTA):**
- **LLM non-determinism breaks naive replay.** Replay needs a **content-addressed response cache**: hash each model request (BLAKE3) and serve the cached response on recompute. Without it, "deterministic replay" is fiction.
- **Storage grows with run length.** Cold events compact to summaries + pointers and tier out to the [storage backends](./STORAGE-BACKENDS.md); raw events are never deleted, only relocated.
- **Schema evolution.** Event types carry a `schema_version` from day 1; replay applies versioned upcasters.
- **Single writer per row** (already the system's discipline) - the event-sourcing papers don't solve concurrent writers, and a personal OS does not need them to.

---

## 3. The memory read models (rebuildable projections)

Layer 0 (source of truth, synced `lifeos.db`):
```
events(id, type, schema_version, payload_json,
       caused_by_event_id,        -- Novel: causal pointer ("what led to this?")
       entity_id, actor, ts)      -- APPEND-ONLY (no UPDATE/DELETE)
```

Layer 1 (derived projections, rebuildable; metadata in `lifeos.db`, indices in `lifeos-derived.db`):
```
memory_nodes(id, content, importance, access_count, last_accessed,
             confidence, source_event_ids_json, embedding_ref)
memory_edges(from_id, to_id, rel_type,
             t_valid, t_invalid,          -- Novel here: bi-temporal validity (Zep/Graphiti)
             superseded_by_event_id, source_event_id)
memory_summaries(id, level, content, confidence,
                 source_event_ids_json, ts)   -- multi-resolution (episode/day/week/profile)
```

Layer 2 (search indices, un-synced `lifeos-derived.db`, reuse `memvec.py`):
```
entities_fts  USING fts5(...)          -- BM25 (attrs flattened to attrs_text by trigger)
entity_vec    USING vec0(...)          -- MiniLM-384 (all-MiniLM-L6-v2) semantic ANN
```

The derived DB stays a physically separate, never-synced file (libSQL has no table-level sync flag) - and now it is also *safe to blow away*, because §2 makes it recomputable from `events`.

---

## 4. Retrieval - activation-scored, not flat top-k

**Novel:** retrieval is an **activation score** over a hybrid candidate set, not a single vector top-k.

```
A(m) = relevance(m,q) · recency(m) · importance(m) · frequency(m)
relevance = RRF( vector_rank , bm25_rank , entity_match_rank , like_rank )  -- hybrid, not vector-only
recency   = decay_per_hour ^ Δhours                              -- ACT-R decay (decay_per_hour ≈ 0.995/hr, Δhours from now or as_of)
frequency = 1.0 + ln(1 + access_count)                           -- ACT-R base-level activation, +1.0 offset so a never-accessed memory still scores
importance= max(0.05, salience scored once at write time)        -- not per-read; a 0.05 floor so a low-salience memory can never zero out A(m)
```

The exact parameterization the implementation uses (`retrieval.rs::score_candidates`):
`recency` is `decay_per_hour.powf(Δhours)` where `Δhours = max(0, reference - ts) / 3600` and `reference` is `now` for current-truth recall or `as_of` for point-in-time recall.
`frequency` carries a `+1.0` offset (`1.0 + ln(1 + access_count)`) so a memory that has never been recalled still contributes a factor of `1.0` rather than `0.0`.
`importance` is floored at `0.05` (`importance.max(0.05)`) so even the least-salient memory keeps a non-zero activation and can still surface when relevance and recency are strong.

**Vector lane wiring (env-gated):** the semantic ANN lane is a `VectorSearcher` the API injects, because vec0/sqlite-vec is owned by `memvec.py` and is not loadable from the Rust libSQL build.
When `LIFEOS_MEMVEC` is set, `lifeos-api` wires a subprocess-backed `MemvecSearcher` (the same shared seam `/api/search` uses); when it is unset, or the subprocess is missing/fails/times out, recall degrades to the lexical lanes (`NoopVectorSearcher`) and never errors.
Memory-node vectors are partitioned from entity vectors by a `mem:<ws>` workspace label, so the memory lane only ever returns `mn_*` ids and `/api/search` only ever returns entity ids.
Memory nodes are embedded best-effort at recall/ingest time (`embed_new_nodes`), tracked in the never-synced derived DB (`d.memory_embedded`); because node ids are content-deterministic, an embedding stays valid across a rebuild and never needs invalidating.

Pipeline per query:
1. **Query reformulation** (one cheap LLM call): rewrite the turn into "what would a relevant memory look like?" before scoring.
2. **Self-RAG gate:** skip long-term retrieval entirely on turns that don't need it (latency + noise win).
3. Gather candidates from FTS5 (BM25) + sqlite-vec (ANN) + entity match.
4. Score with `A(m)`; take the top set.
5. **Spreading activation (tiered, optional):** for multi-hop queries, expand 1-2 hops over `memory_edges` where `t_invalid IS NULL`, then re-score.
   Graph expansion is *not* always-on - "Does Memory Need Graphs?" (2026) shows it only pays off for multi-hop at scale, so single-hop queries stay on the cheap FTS5+vector path.
6. **Abstention signal:** if the top activation is below threshold, emit "no reliable memory" so the model says *I don't know* instead of confabulating (the LongMemEval failure mode).

**GraphRAG global lens (issue #139, `communities.rs`):** spreading activation above is local, seeded from one query.
A global view - "which parts of my world touch trading?" - instead clusters the whole `memory_edges` graph with deterministic label propagation, persists a `memory_communities` read model of cluster summaries, and rebuilds it every sleep cycle.
`ask_network` ranks clusters by relevance and returns grounded summaries; it never calls a model itself, so the caller composes the final answer.

This is the SOTA-validated formula (Generative Agents + ACT-R + ENGRAM's typed hybrid), with frequency and BM25 added (both were missing from the naive recency·importance·relevance baseline).

---

## 5. Consolidation - "sleep" jobs that write events, never mutate state

**Novel:** consolidation (Letta calls it sleep-time compute; Generative Agents call it reflection) runs as scheduled background jobs that **emit new provenance-linked events**, rather than overwriting memory.

Jobs (Tokio async on the Mac; triggered on idle + an accumulated-importance threshold):
- `segment_episodes`: cut the raw event stream at **cognitive episode boundaries** (topic shift / task done), not per message - episode-aligned units retrieve better (ES-Mem).
- `consolidate`: episodes -> episode summaries; summaries -> semantic facts; weekly -> reflections (multi-resolution).
- `score_importance` + `score_surprise`: salience at write time; **Novel:** flag low-similarity outliers (surprising/contradicting memories) as high-importance, slow-decay, and trigger reconciliation.
- `decay_sweep`: nightly ranking recompute (no LLM); it also runs the cold-tier sweep (§7) on the same cadence, actually tiering a bounded batch of cold nodes out to storage when a backend is available, not merely counting them.
- `supersede_detector`: turn UPDATE-type events into `t_invalid` on the old edge + a supersede pointer.

The sleep cycle takes the workspace's primary storage backend as an optional argument.
When it is present, the decay sweep tiers cold nodes out on cadence (bounded to `TIER_BATCH_MAX` per cycle, same thresholds as the manual `POST /api/memory/tier`); when it is `None`, the sweep records an honest `memory.tier.skipped` ledger note and only counts, so a deployment with no backend degrades cleanly rather than erroring.

**The unsolved field problem is summarization drift** (rare-but-important details silently dropped) and **reflection self-reinforcement** (a consolidation error propagating with no ground truth).
Life OS's mitigation is structural and not implemented by any current system: **raw events are never deleted, every summary carries `source_event_ids` + a `confidence`/source-count**, so a bad consolidation is auditable and correctable by recompute.
This is a genuine design advantage, again falling out of the append-only spine.

---

## 6. The context compiler - hybrid-deterministic working memory

**Novel:** working memory is assembled by a **deterministic context compiler** with a token budget, not by LLM-driven paging (MemGPT), which adds hot-path latency and cost.

Per turn, deterministic where it should be, scored where it must be:
- **Deterministic:** budget allocation (N tokens recent-turns-verbatim, M tokens retrieved facts, P tokens procedural rules), the recency window (always include the last K turns), and template formatting/injection.
- **Scored:** *what* fills the retrieval budget (the `A(m)` ranking from §4).
- The agent may still request deeper paging on demand, but the default is a compiled, bounded, reproducible, testable working set.

ENGRAM-style results show this kind of budgeted assembly hits full-context-level quality at ~1% of the tokens - the efficiency that makes an unbounded life fit a fixed window.

---

## 7. Forgetting - decay + bi-temporal supersede, never deletion

Auditability is sacred, so nothing is hard-deleted.

- **Ranking decay:** low-activation memories fall out of the compiled context (§4) but remain queryable.
- **Bi-temporal supersede:** "I moved Delhi -> Bangalore" does not edit the old fact - it sets `t_invalid` on the old `memory_edge` and adds a new one, with a `superseded_by_event_id`.
  Retrieval never returns `t_invalid < now` facts as current truth, but a point-in-time query can still ask "what was true in March?".
  This is the mechanism that fixes temporal reasoning - the field's hardest axis (even GPT-4o-class systems score ~49% on it with decay alone).
- **Tiered storage:** cold, low-activation events/blobs migrate to the user's [storage backend](./STORAGE-BACKENDS.md) (warm index stays in the derived DB); promote-on-access.
  Tiering runs automatically on the sleep cadence (§5), not only via the manual endpoint: `tier_out_cold` bundles cold node content to a content-addressed blob and replaces each node's row content with a 64-char stub + `tiered_ref`, keeping the row, its scores, and its provenance intact.
  Crucially it leaves the derived FTS mirror (`d.memory_idx`) holding the FULL text, so a tiered node's distinctive terms - even ones past the stub - still match on the lexical lane and recall still surfaces it; the FTS index is derived and local, so retaining full text there does not defeat tiering (the storage win is moving the canonical blob, not shrinking the disposable index).
  On recall, promote-on-access then restores the full content from the backend and clears `tiered_ref`, and the restored text is reflected in that same recall response, not just persisted for next time.

---

## 8. Procedural memory - self-updating behavioral rules

**Novel for Life OS:** beyond facts, store **how to behave** - "when the user asks about a trade, always include the current market regime", "summaries should lead with the TLDR".
These are a separate procedural store that feeds the system prompt directly and is updated by consolidation, giving policy-learning-without-RL (LangMem's most distinctive idea).
The interface is kept clean (inputs: candidate events; outputs: rule deltas) so the policy can later be replaced by a learned one (Memory-R1) with no architectural change.

---

## 9. Multi-tier, multi-agent, isolated

- **One shared memory across tiers:** the Telegram bot (Haiku), the Mac harness, and the in-app agent all recall from the same canonical DB + derived indices (reusing `~/.claude/bin/memvec.py` and `memory-recall`).
  The shared DB *is* the cross-tier memory.
- **Write-heavy, read-cheap:** importance/entity extraction happen at write time (and in consolidation); the hot retrieval path makes **no LLM call** beyond optional query reformulation.
  This fits a personal OS where reads happen every turn and writes are comparatively rare.
- **Workspace-scoped:** every memory row carries `workspace_id`; a second workspace cannot recall the first's memories (RLS-style, [SECURITY.md](./SECURITY.md) §5).
- **Secrets excluded; writes are events:** secrets never enter memory; agent memory-writes are `events` and therefore appear in the AGENT-CONTROL action ledger and are undoable.
  The agent may read/write memory but cannot touch the protected domains (VCS-rewrite, security config, OAuth/connections, API keys).

---

## 10. Reuse vs net-new

- **Reuse (already specified/built):** `events` (episodic spine), `entities`/`edges` (temporal graph), derived `entities_fts` + `entity_vec`, RRF via `~/.claude/bin/memory-recall`, `memvec.py` (MiniLM-384), the un-synced derived DB, the jobs/drain loop, the storage backends.
- **Net-new (the real work):** the event-sourced projection + replay cache (§2), the `memory_nodes`/`memory_edges`/`memory_summaries` read models with bi-temporal validity (§3), the activation re-ranker + reformulation + abstention (§4), the consolidation jobs with provenance/confidence (§5), the hybrid-deterministic context compiler (§6), supersede + tiering (§7), and the procedural store (§8).
- **Stack:** all of it sits on libSQL/Turso + sqlite-vec + FTS5 + Tokio (+ `petgraph` for spreading activation, `blake3` for the replay cache) - **zero new infrastructure** beyond what the system already plans, fully offline-capable.

---

## 11. Build surface & verification

- **Backend:** the projections + activation re-ranker + context compiler live in `lifeos-api`/`lifeos-pipelines`; consolidation runs as `jobs` drained by `lifeos-drain`.
- **Frontend:** a memory inspector (what the agent recalled and why - score breakdown), surfaced via the AGENT-CONTROL ledger.
- **Benchmark targets (regression-test against public sets):** LongMemEval (temporal reasoning + knowledge-updates + abstention), BEAM at 1M/10M (watch the scaling cliff), LoCoMo (multi-hop).
- **Must-pass checks:**
  - Delete the entire derived store + memory read models, recompute from `events`, and retrieval returns identical results (projection is truly derived).
  - A superseded fact ("moved cities") is never returned as current, but a point-in-time query still recovers the old value.
  - Every recalled fact resolves to its `source_event_ids`; nothing is recalled without provenance.
  - A turn needing no memory triggers the self-RAG gate (no retrieval); a genuinely-unknown fact triggers abstention, not confabulation.
  - No secret ever appears in a memory row; a second workspace recalls nothing of the first's.

---

**TLDR (layman engineer terms):** Build a small brain, not a vector DB.
Keep the append-only event log as the one source of truth and treat every summary/embedding/profile as a **rebuildable cache** of it (event-sourcing) - that buys provenance, safe forgetting, and trivial sync.
Recall by **relevance + recency + importance + how-often-used + a couple of graph hops** (not flat top-k), run nightly **"sleep" jobs** that distill raw events into compact facts (keeping pointers back to the originals so nothing important is silently lost), and assemble each turn's context with a **deterministic, token-budgeted compiler**.
Forgetting = ranking decay + marking old facts "no longer true" (never deletion) + moving cold data to the user's storage.
Most of the substrate already exists; the new parts are the projection/replay layer, the activation re-ranker, the consolidation jobs, and the context compiler - all on the libSQL + sqlite-vec + FTS5 stack you already planned.
