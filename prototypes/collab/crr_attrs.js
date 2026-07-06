#!/usr/bin/env node
// crr_attrs.js - cr-sqlite-faithful column-LWW CRDT over one entity's `attrs`.
//
// Zero dependencies. Reproduces exactly the merge algorithm cr-sqlite applies to
// a CRR table's `crsql_changes` virtual table, restricted to the one shape Life OS
// needs: last-write-wins registers, one per attribute key, tie-broken by site_id.
//
// This is the honest "minimal convergence demo" the spike allows: rather than
// vendor cr-sqlite's native loadable extension into nix-shell (impractical, and
// opaque), we implement its published merge rule so the semantics are auditable.
//
// cr-sqlite semantics reproduced (see vlcn.io/docs/cr-sqlite):
//   * db_version : per-database Lamport clock, bumped on every local write.
//   * col_version: per-cell Lamport clock, bumped on every local write to that cell.
//   * site_id    : origin replica id; deterministic tiebreak when col_versions tie.
//   * a `crsql_changes` row = { pk, cid, val, col_version, db_version, site_id }.
//   * a peer tracks a version vector {site_id -> max db_version seen from that site};
//     pull = all change rows whose db_version exceeds the asker's vector for that site.
//   * merge a cell: incoming wins iff  (col_version, site_id) > (local col_version, local site_id).

'use strict';

// ---- pure merge core ---------------------------------------------------------

// A cell wins over another by Lamport col_version, site_id as deterministic tiebreak.
function cellWins(incoming, local) {
  if (!local) return true;
  if (incoming.col_version !== local.col_version) {
    return incoming.col_version > local.col_version;
  }
  return incoming.site_id > local.site_id; // memcmp-style deterministic tiebreak
}

// Apply a single crsql_changes row into a store, returning a NEW store (immutable).
function applyChange(store, change) {
  const row = store[change.pk] || {};
  const local = row[change.cid];
  if (!cellWins(change, local)) return store; // keep local, no-op
  const nextCell = {
    val: change.val,
    col_version: change.col_version,
    site_id: change.site_id,
    db_version: change.db_version,
  };
  return {
    ...store,
    [change.pk]: { ...row, [change.cid]: nextCell },
  };
}

// ---- a replica (a "client": Mac heavy-lane or bot light-lane) ----------------

class Replica {
  constructor(siteId) {
    this.siteId = siteId;
    this.dbVersion = 0;       // local Lamport clock
    this.store = {};          // pk -> cid -> cell
    this.events = [];         // append-only log (the Life OS `events` analog)
    this.seen = {};           // tracked-peers table: site_id -> highest db_version ever applied
  }

  _track(siteId, dbVersion) {
    this.seen[siteId] = Math.max(this.seen[siteId] || 0, dbVersion);
  }

  // Version vector {site_id -> highest db_version ever applied from that site}.
  // Persisted (not derived from live cells) so an overwritten cell's history is
  // still known-seen - this is cr-sqlite's `crsql_tracked_peers` table.
  versionVector() {
    return { ...this.seen };
  }

  // Local write: set attrs[cid] = val on entity pk. Bumps clocks, appends an event.
  set(pk, cid, val) {
    this.dbVersion += 1;
    const prev = (this.store[pk] || {})[cid];
    const col_version = (prev ? prev.col_version : 0) + 1;
    const change = { pk, cid, val, col_version, db_version: this.dbVersion, site_id: this.siteId };
    this.store = applyChange(this.store, change);
    this._track(this.siteId, this.dbVersion);
    // events is append-only and conflict-free: it records the change, not final state.
    this.events.push({ type: 'entity.updated', ...change });
    return change;
  }

  // crsql_changes pull: only cells the asking peer has not seen, per its version vector.
  changesFor(peerVV) {
    const out = [];
    for (const pk of Object.keys(this.store)) {
      for (const cid of Object.keys(this.store[pk])) {
        const c = this.store[pk][cid];
        if (c.db_version > (peerVV[c.site_id] || 0)) {
          out.push({ pk, cid, val: c.val, col_version: c.col_version, db_version: c.db_version, site_id: c.site_id });
        }
      }
    }
    return out.sort((a, b) => a.db_version - b.db_version);
  }

  // Merge a peer's changeset (idempotent, commutative, associative).
  merge(changeset) {
    let store = this.store;
    for (const c of changeset) {
      store = applyChange(store, c);
      // keep our Lamport clock monotonic past anything we absorb (causality)
      if (c.db_version > this.dbVersion) this.dbVersion = c.db_version;
      this._track(c.site_id, c.db_version); // record we have now seen this site's history
      // absorbed peer changes are also facts in our append-only log
      this.events.push({ type: 'entity.updated', ...c });
    }
    this.store = store;
  }

  attrs(pk) {
    const row = this.store[pk] || {};
    return Object.fromEntries(Object.keys(row).map((cid) => [cid, row[cid].val]));
  }
}

// ---- event-sourcing coexistence: reconcile purely from the union event log ----
// Mirrors services/lifeos-api/src/reconcile.rs: replay events in causal order and
// take the winning snapshot per cell. Proves the CRDT outcome == the event-log
// outcome, so `events` stays the append-only source of truth (Life OS invariant).
function reconcileFromEvents(...logs) {
  const all = [].concat(...logs);
  const winners = {}; // pk -> cid -> event
  for (const e of all) {
    const key = e.pk;
    winners[key] = winners[key] || {};
    const cur = winners[key][e.cid];
    if (cellWins(e, cur)) winners[key][e.cid] = e;
  }
  const out = {};
  for (const pk of Object.keys(winners)) {
    out[pk] = Object.fromEntries(Object.keys(winners[pk]).map((cid) => [cid, winners[pk][cid].val]));
  }
  return out;
}

// ---- demo / acceptance harness ----------------------------------------------

function assert(cond, msg) {
  if (!cond) { console.error('FAIL:', msg); process.exitCode = 1; throw new Error(msg); }
  console.log('  ok:', msg);
}

function jsonBytes(obj) { return Buffer.byteLength(JSON.stringify(obj), 'utf8'); }

function main() {
  const PK = 'ent_01HXTRADE'; // one entity, e.g. a Trading module `trade` row
  console.log('=== cr-sqlite-faithful attrs convergence (Life OS entity.attrs) ===\n');

  // 1. Seed a shared base on an origin, sync it to both replicas so col_versions match.
  const origin = new Replica('site_0origin');
  origin.set(PK, 'status', 'open');
  origin.set(PK, 'notes', 'initial thesis');
  origin.set(PK, 'priority', 1);
  const base = origin.changesFor({}); // empty vector -> full seed

  const mac = new Replica('site_2_mac');   // heavy lane (higher site_id)
  const bot = new Replica('site_1_bot');   // light lane (lower site_id)
  mac.merge(base);
  bot.merge(base);
  console.log('base synced to both:', JSON.stringify(mac.attrs(PK)), '\n');

  // 2. GO OFFLINE. Concurrent edits by two clients on the SAME entity's attrs.
  //    - disjoint columns must all survive (no lost writes)
  //    - the ONE genuinely-concurrent column (notes) resolves deterministically
  console.log('-- offline concurrent edits --');
  mac.set(PK, 'notes', 'refined analysis after chart review'); // conflicts with bot.notes
  mac.set(PK, 'priority', 3);                                   // disjoint -> must survive
  bot.set(PK, 'status', 'in_review');                          // disjoint -> must survive
  bot.set(PK, 'notes', 'quick note from phone');               // conflicts with mac.notes
  console.log('  mac offline attrs:', JSON.stringify(mac.attrs(PK)));
  console.log('  bot offline attrs:', JSON.stringify(bot.attrs(PK)), '\n');

  // 3. RECONNECT. Each side sends its version vector; the peer returns only the
  //    cells that vector has not seen (this is cr-sqlite's whole-CRR pull).
  const macToBot = mac.changesFor(bot.versionVector());
  const botToMac = bot.changesFor(mac.versionVector());
  console.log('-- reconnect: exchange incremental changesets --');
  console.log(`  mac->bot: ${macToBot.length} cell(s), ${jsonBytes(macToBot)} bytes -> ${macToBot.map((c) => c.cid).join(', ')}`);
  console.log(`  bot->mac: ${botToMac.length} cell(s), ${jsonBytes(botToMac)} bytes -> ${botToMac.map((c) => c.cid).join(', ')}`);

  mac.merge(botToMac);
  bot.merge(macToBot);

  const macFinal = mac.attrs(PK);
  const botFinal = bot.attrs(PK);
  console.log('\n  mac final:', JSON.stringify(macFinal));
  console.log('  bot final:', JSON.stringify(botFinal), '\n');

  // 4. ASSERTIONS
  console.log('-- acceptance checks --');
  assert(JSON.stringify(macFinal) === JSON.stringify(botFinal), 'both replicas CONVERGE to identical attrs');
  assert(macFinal.status === 'in_review', 'bot-only edit (status) survived - no lost write');
  assert(macFinal.priority === 3, 'mac-only edit (priority) survived - no lost write');
  // notes: both bumped col_version 1->2, tie -> higher site_id wins. mac=site_2 > bot=site_1.
  assert(macFinal.notes === 'refined analysis after chart review',
    'concurrent notes conflict resolved deterministically by (col_version, site_id)');

  // 5. Commutativity: reverse the merge order, must reach the same state.
  const mac2 = new Replica('site_2_mac'); mac2.merge(base);
  const bot2 = new Replica('site_1_bot'); bot2.merge(base);
  mac2.set(PK, 'notes', 'refined analysis after chart review'); mac2.set(PK, 'priority', 3);
  bot2.set(PK, 'status', 'in_review'); bot2.set(PK, 'notes', 'quick note from phone');
  bot2.merge(mac2.changesFor(bot2.versionVector())); // reversed order vs step 3
  mac2.merge(bot2.changesFor(mac2.versionVector()));
  assert(JSON.stringify(mac2.attrs(PK)) === JSON.stringify(macFinal), 'merge is order-independent (commutative)');

  // 6. Event-sourcing fit: reconcile purely from the union of both append-only logs.
  const fromEvents = reconcileFromEvents(mac.events, bot.events)[PK];
  assert(JSON.stringify(fromEvents) === JSON.stringify(macFinal),
    'event-log replay (reconcile.rs analog) == CRDT merge outcome');

  console.log('\n=== RESULT: converged, zero lost writes, events log agrees ===');
}

if (require.main === module) main();
module.exports = { Replica, applyChange, cellWins, reconcileFromEvents };
