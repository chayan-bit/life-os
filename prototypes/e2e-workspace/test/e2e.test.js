// Zero-dependency tests using Node's built-in runner:
//   nix-shell -p nodejs --run 'node --test'
//
// These lock the two properties the spike must prove, plus the fail-closed
// crypto contract, so a future real build can port the invariants directly.

import { test } from "node:test";
import assert from "node:assert/strict";

import { seal, open, deriveMemberKey, randomKey, randomSalt, wrapKey, unwrapKey } from "../src/crypto.js";
import { MockServer } from "../src/server.js";
import { E2EClient } from "../src/client.js";
import { buildIndex, search } from "../src/localIndex.js";
import { generateEntities } from "../src/data.js";

test("seal/open round-trips and produces unlinkable ciphertexts", () => {
  const key = randomKey();
  const a = seal("private note", key);
  const b = seal("private note", key);
  assert.notEqual(a, b, "random nonce must make ciphertexts differ");
  assert.equal(open(a, key).toString("utf8"), "private note");
});

test("open fails closed on the wrong key", () => {
  const blob = seal("secret", randomKey());
  assert.throws(() => open(blob, randomKey()));
});

test("open fails closed on tampered ciphertext", () => {
  const key = randomKey();
  const raw = Buffer.from(seal("integrity matters", key), "base64");
  raw[raw.length - 1] ^= 0xff; // flip a tag byte
  assert.throws(() => open(raw.toString("base64"), key));
});

test("content key wrap/unwrap works only with the right member KEK", () => {
  const cwk = randomKey();
  const salt = randomSalt();
  const kek = deriveMemberKey("pass-1", salt);
  const wrapped = wrapKey(cwk, kek);
  assert.deepEqual(unwrapKey(wrapped, kek), cwk);
  const wrongKek = deriveMemberKey("pass-2", salt);
  assert.throws(() => unwrapKey(wrapped, wrongKek));
});

test("server never stores plaintext attrs for an E2E workspace", () => {
  const server = new MockServer();
  server.createWorkspace("ws", "e2e");
  const client = new E2EClient(server, "ws", "alice");
  client.bootstrapWorkspace("pw");
  client.put({ id: "e1", module: "trading", type: "trade", createdAt: 1, title: "banknifty breakout", attrs: { body: "swing long banknifty" } });

  const row = server.fetchRow("ws", "e1");
  const decoded = Buffer.from(row.attrs, "base64").toString("latin1");
  assert.ok(!row.attrs.includes("banknifty"));
  assert.ok(!decoded.includes("banknifty"));
  assert.equal(row.module, "trading", "structural metadata stays readable by design");
});

test("a second device reconstructs data from passphrase alone", () => {
  const server = new MockServer();
  server.createWorkspace("ws", "e2e");
  const a = new E2EClient(server, "ws", "alice");
  a.bootstrapWorkspace("pw");
  a.put({ id: "e1", module: "learning", type: "note", createdAt: 1, title: "t", attrs: { body: "hello world" } });

  const b = new E2EClient(server, "ws", "alice");
  b.openSession("pw");
  assert.equal(b.get("e1").attrs.body, "hello world");
});

test("added member can decrypt the same content", () => {
  const server = new MockServer();
  server.createWorkspace("ws", "e2e");
  const owner = new E2EClient(server, "ws", "alice");
  owner.bootstrapWorkspace("alice-pw");
  owner.put({ id: "e1", module: "tasks", type: "task", createdAt: 1, title: "t", attrs: { body: "shared secret" } });
  owner.addMember("bob", "bob-pw");

  const bob = new E2EClient(server, "ws", "bob");
  bob.openSession("bob-pw");
  assert.equal(bob.get("e1").attrs.body, "shared secret");
});

test("server-side FTS is impossible on E2E but works on plaintext", () => {
  const server = new MockServer();
  const entities = generateEntities(200);

  server.createWorkspace("plain", "plain");
  for (const e of entities) server.storeRow("plain", { ...e, attrs: JSON.stringify(e.attrs) });

  server.createWorkspace("e2e", "e2e");
  const client = new E2EClient(server, "e2e", "alice");
  client.bootstrapWorkspace("pw");
  for (const e of entities) client.put(e);

  assert.ok(server.serverSearch("plain", "envelope encryption", 5).indexable > 0);
  assert.equal(server.serverSearch("e2e", "envelope encryption", 5).indexable, 0);
});

test("local search over decrypted data closely matches server FTS ranking", () => {
  const server = new MockServer();
  const entities = generateEntities(300);
  server.createWorkspace("plain", "plain");
  for (const e of entities) server.storeRow("plain", { ...e, attrs: JSON.stringify(e.attrs) });
  server.createWorkspace("e2e", "e2e");
  const client = new E2EClient(server, "e2e", "alice");
  client.bootstrapWorkspace("pw");
  for (const e of entities) client.put(e);
  client.buildLocalIndex();

  const q = "swing trade breakout volume";
  const serverTop = new Set(server.serverSearch("plain", q, 10).results.map((r) => r.id));
  const localTop = client.localSearch(q, 10).map((r) => r.id);
  const overlap = localTop.filter((id) => serverTop.has(id)).length;
  assert.ok(overlap / serverTop.size > 0.8, `parity too low: ${overlap}/${serverTop.size}`);
});

test("buildIndex + search rank an exact term match first", () => {
  const idx = buildIndex([
    { id: "a", text: "alpha alpha alpha" },
    { id: "b", text: "beta gamma" },
  ]);
  assert.equal(search(idx, "alpha", 1)[0].id, "a");
});
