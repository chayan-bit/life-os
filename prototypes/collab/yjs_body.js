#!/usr/bin/env node
// yjs_body.js - REAL Yjs, validating the "Yjs for long-form content bodies" half
// of the spike hypothesis. Requires one pure-JS dependency: `npm install`.
//
// This uses the actual Yjs library (not a re-implementation) so the convergence
// guarantee shown is Yjs's own YATA sequence CRDT, exactly what would back a
// long-form body attribute (e.g. a Learning topic write-up or a trade thesis).
//
// What it proves: two clients start from the same body, GO OFFLINE, make
// concurrent character-level edits, then reconnect and exchange only the deltas
// each is missing (state-vector diff). Both converge to the same text, no lost
// writes, no character-level clobbering (unlike cr-sqlite's whole-cell LWW).

'use strict';

let Y;
try {
  Y = require('yjs');
} catch (e) {
  console.error('This demo needs Yjs. Run:  npm install   (in prototypes/collab/)');
  process.exit(2);
}

function assert(cond, msg) {
  if (!cond) { console.error('FAIL:', msg); process.exitCode = 1; throw new Error(msg); }
  console.log('  ok:', msg);
}

function main() {
  console.log('=== Yjs long-form body convergence (real library) ===\n');

  // 1. Shared base body, synced to both clients.
  const base = new Y.Doc();
  base.getText('body').insert(0, 'The trade thesis: long NIFTY on breakout.');
  const baseUpdate = Y.encodeStateAsUpdate(base);

  const A = new Y.Doc(); Y.applyUpdate(A, baseUpdate); // e.g. the Mac
  const B = new Y.Doc(); Y.applyUpdate(B, baseUpdate); // e.g. the browser/bot
  const svA0 = Y.encodeStateVector(A);
  const svB0 = Y.encodeStateVector(B);
  console.log('base:', JSON.stringify(A.getText('body').toString()), '\n');

  // 2. GO OFFLINE. Concurrent character-level edits to the same body.
  console.log('-- offline concurrent edits --');
  A.getText('body').insert(A.getText('body').length, ' Add stop at 22800.'); // A appends
  B.getText('body').insert(0, '[DRAFT] ');                                    // B prepends
  console.log('  A offline:', JSON.stringify(A.getText('body').toString()));
  console.log('  B offline:', JSON.stringify(B.getText('body').toString()), '\n');

  // 3. RECONNECT. Exchange only the missing deltas (state-vector diff transport).
  const deltaA = Y.encodeStateAsUpdate(A, svB0); // what B is missing from A
  const deltaB = Y.encodeStateAsUpdate(B, svA0); // what A is missing from B
  console.log('-- reconnect: exchange state-vector deltas --');
  console.log(`  A->B delta: ${deltaA.length} bytes | B->A delta: ${deltaB.length} bytes`);
  console.log(`  (full-doc update would be ${Y.encodeStateAsUpdate(A).length} bytes)`);
  Y.applyUpdate(A, deltaB);
  Y.applyUpdate(B, deltaA);

  const finalA = A.getText('body').toString();
  const finalB = B.getText('body').toString();
  console.log('\n  A final:', JSON.stringify(finalA));
  console.log('  B final:', JSON.stringify(finalB), '\n');

  console.log('-- acceptance checks --');
  assert(finalA === finalB, 'both clients CONVERGE to identical body text');
  assert(finalA.includes('[DRAFT]'), "B's prepend survived - no lost write");
  assert(finalA.includes('22800'), "A's append survived - no lost write");

  // 4. Idempotence + commutativity: applying a delta twice, or in either order,
  //    yields the same document (Yjs guarantees this).
  const C = new Y.Doc(); Y.applyUpdate(C, baseUpdate);
  Y.applyUpdate(C, deltaA); Y.applyUpdate(C, deltaB);
  Y.applyUpdate(C, deltaA); // re-apply: no effect
  assert(C.getText('body').toString() === finalA, 'delta application is idempotent + commutative');

  console.log('\n=== RESULT: converged, character-level merge, no lost writes ===');
}

if (require.main === module) main();
