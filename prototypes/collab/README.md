# Co-editing convergence prototype (spike #152)

Isolated, self-contained prototype for the real-time co-editing spike.
It is **not wired into the Life OS app** - it is a standalone runnable that proves
the merge semantics behind the decision in [`docs/COLLAB.md`](../../docs/COLLAB.md).

Two demos, one per half of the recommended split:

| File | Layer | CRDT | Dependency |
|---|---|---|---|
| `crr_attrs.js` | entity `attrs` (structured fields) | cr-sqlite-faithful column LWW register | none (pure Node) |
| `yjs_body.js` | long-form content body | **real Yjs** (YATA sequence CRDT) | `yjs` (pure JS, no native build) |

## How to run

```bash
# entity.attrs convergence - zero dependencies, runs as-is:
node crr_attrs.js

# long-form body convergence - needs the one pure-JS dep first:
npm install        # installs yjs only; no native/C build step
node yjs_body.js

# both at once:
npm install && npm test
```

Both scripts exit non-zero if any convergence assertion fails, so they double as tests.
(If your shell prints an unrelated `zoxide` warning on stderr, ignore it - it comes from
the machine's shell profile, not the demo.)

## What each proves

### `crr_attrs.js` - the acceptance criterion

This is the exact acceptance scenario from the issue: **two clients concurrently edit one
entity's `attrs` while offline and converge on reconnect without lost writes.**

It does not vendor cr-sqlite's native loadable extension (impractical to build under
nix-shell, and opaque). Instead it re-implements cr-sqlite's *published* merge algorithm so
the semantics are auditable:

- each `attrs` key is a **last-write-wins register** with a per-cell Lamport `col_version`,
  a `db_version` (per-database Lamport clock), and an origin `site_id`;
- a change row mirrors cr-sqlite's `crsql_changes` virtual table
  `{ pk, cid, val, col_version, db_version, site_id }`;
- a replica keeps a **tracked-peers version vector** (cr-sqlite's `crsql_tracked_peers`) so a
  reconnect pulls only the cells the peer has not seen;
- merge rule: an incoming cell wins iff `(col_version, site_id) > (local col_version, local site_id)`.

The scenario: a shared base `{status, notes, priority}` is synced to a "mac" (heavy lane) and
a "bot" (light lane); both go offline; mac edits `notes` + `priority`, bot edits `status` +
`notes`. On reconnect they exchange incremental changesets. It then asserts:

1. both replicas **converge** to identical attrs;
2. the disjoint edits (`status`, `priority`) **both survive** - no lost writes;
3. the one genuinely-concurrent edit (`notes`) resolves **deterministically** and identically
   on both sides by `(col_version, site_id)`;
4. merge is **order-independent** (apply A-then-B == B-then-A);
5. **event-sourcing fit:** replaying the union of both append-only event logs (the
   `reconcile.rs` analog) yields the *same* state as the CRDT merge - so `events` can stay the
   append-only source of truth.

### `yjs_body.js` - the long-form half, with the real library

Uses the actual `yjs` package (not a re-implementation). Two clients start from the same body,
go offline, make concurrent **character-level** edits (one prepends, one appends), then
reconnect and exchange only the state-vector deltas each is missing. It asserts convergence,
that both edits survive (character-level merge, no whole-field clobber), and idempotence.

## Actual output / measurements (captured this run)

`node crr_attrs.js` (Node v22.22.3):

```
=== cr-sqlite-faithful attrs convergence (Life OS entity.attrs) ===

base synced to both: {"status":"open","notes":"initial thesis","priority":1}

-- offline concurrent edits --
  mac offline attrs: {"status":"open","notes":"refined analysis after chart review","priority":3}
  bot offline attrs: {"status":"in_review","notes":"quick note from phone","priority":1}

-- reconnect: exchange incremental changesets --
  mac->bot: 2 cell(s), 238 bytes -> notes, priority
  bot->mac: 2 cell(s), 232 bytes -> status, notes

  mac final: {"status":"in_review","notes":"refined analysis after chart review","priority":3}
  bot final: {"status":"in_review","notes":"refined analysis after chart review","priority":3}

-- acceptance checks --
  ok: both replicas CONVERGE to identical attrs
  ok: bot-only edit (status) survived - no lost write
  ok: mac-only edit (priority) survived - no lost write
  ok: concurrent notes conflict resolved deterministically by (col_version, site_id)
  ok: merge is order-independent (commutative)
  ok: event-log replay (reconcile.rs analog) == CRDT merge outcome

=== RESULT: converged, zero lost writes, events log agrees ===
```

`node yjs_body.js` (yjs 13.6.31):

```
=== Yjs long-form body convergence (real library) ===

base: "The trade thesis: long NIFTY on breakout."

-- offline concurrent edits --
  A offline: "The trade thesis: long NIFTY on breakout. Add stop at 22800."
  B offline: "[DRAFT] The trade thesis: long NIFTY on breakout."

-- reconnect: exchange state-vector deltas --
  A->B delta: 36 bytes | B->A delta: 25 bytes
  (full-doc update would be 92 bytes)

  A final: "[DRAFT] The trade thesis: long NIFTY on breakout. Add stop at 22800."
  B final: "[DRAFT] The trade thesis: long NIFTY on breakout. Add stop at 22800."

-- acceptance checks --
  ok: both clients CONVERGE to identical body text
  ok: B's prepend survived - no lost write
  ok: A's append survived - no lost write
  ok: delta application is idempotent + commutative

=== RESULT: converged, character-level merge, no lost writes ===
```

### Measured takeaways

- **Convergence holds** for both layers, offline-concurrent, no lost writes.
- **Sync cost is delta-sized, not doc-sized.** cr-sqlite ships only the changed cells
  (~2 cells / ~235 bytes here), Yjs ships only the missing ops (25-36 bytes vs a 92-byte
  full doc). Both ride an incremental changeset, which is what the SSE/WebSocket channel from
  #150 would carry.
- **The two CRDTs answer different questions.** cr-sqlite's column LWW is right for structured
  `attrs` (whole-value fields where "last edit to this field wins" is the desired semantics);
  Yjs's sequence CRDT is right for a prose body where two people typing in different spots must
  both keep their characters. Using column LWW on a prose body would clobber one person's whole
  paragraph; using Yjs for a numeric `priority` field is overkill. Hence the split.

## Honest scope / what this does NOT do

- It does not load the cr-sqlite native extension; it reproduces the documented merge rule.
  A production integration would load `crsqlite` (or port the rule into `lifeos-api`) - see
  `docs/COLLAB.md` for the libSQL-compatibility analysis that drives that choice.
- It does not open a real socket; "reconnect" is an in-process changeset exchange. The
  transport is exactly the delta shown, which #150's stream would carry.
- It does not touch `lifeos.db`, the derived DB, or any app code. Nothing here is wired in.
