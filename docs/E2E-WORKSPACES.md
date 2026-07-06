# End-to-end-encrypted workspaces - threat model (spike, issue #155)

> Status: **spike / design**. This is a threat model plus an isolated,
> measured prototype (`prototypes/e2e-workspace/`), not a shipped feature.
> Nothing here is wired into `services/`. It builds conceptually on the
> envelope machinery that already exists (`workspaces.envelope_key_enc`,
> migration `0011`; `crypto.rs`; `EncryptedBackend` in `lifeos-vcs`).

## Headline conclusion

An end-to-end-encrypted (E2E) workspace - one where the **server operator
cannot read workspace content** - is achievable as an **opt-in trust tier**,
reusing the AES-256-GCM envelope format Life OS already ships, by moving key
custody from the server's master key to **member-derived keys**.

The price is not encryption overhead (that is microseconds).
The price is that **server-side search, AI, and cognitive memory cannot run on
ciphertext**, so an E2E workspace must either give those up or move them
**client-side / onto a trusted `lifeos-node`**.
Because Life OS's whole value proposition is the server-side cognitive-memory
stack ([AI-MEMORY.md](./AI-MEMORY.md)), E2E is best modeled as a **distinct,
explicit trust tier a workspace opts into** - trading server intelligence for
zero-knowledge - **not** as the default for every workspace.

The prototype proves the security property (server never holds plaintext or the
key) and measures the real cost: full-text search still works, but it inverts
from a free O(1) server query into an O(N) client-side fetch-decrypt-index
bootstrap. See §11.

---

## 1. Goal and non-goals

**Goal.** For a workspace flagged E2E, a party who controls the server - the
honest-but-curious operator, an insider with application + database access, or
an attacker who exfiltrates the DB - **cannot recover the plaintext of workspace
content** (entity `attrs`, `title`, annotation bodies, event payloads, blobs).
Content is readable only by workspace **members**, on their own devices, using
keys the server never holds.

**Non-goals (explicitly out of scope for E2E, spike or eventual build).**
- Hiding **structural metadata**. The server still routes, scopes, and syncs
  rows, so it necessarily sees `workspace_id`, `module`, `type`, timestamps,
  row counts, and the graph shape. §4 owns this leakage honestly.
- **Integrity against a malicious server** beyond per-blob authentication.
  GCM's tag detects tampering of any single blob; a server that drops, reorders,
  or rolls back rows is a separate availability/consistency problem (mitigated
  by `events` + client-side verification, not by this design).
- **Metadata-private search** (hiding *which* terms a user searches). Out of
  scope; §8 rejects encrypted-search schemes for this spike.
- Protecting a **compromised member device**. If an attacker owns a member's
  unlocked device, they are that member. E2E defends the server, not the
  endpoint.

---

## 2. What exists today, and the exact gap

Life OS already does client-side AES-256-GCM in two places, both with the wire
format `nonce(12) || ciphertext` (ciphertext carries GCM's 16-byte tag):

- **`services/lifeos-api/src/crypto.rs`** - envelope-encrypts the handful of
  non-Nango secrets in `connections.secret_enc`, and mints/stores a random
  per-workspace key in `workspaces.envelope_key_enc` (`ensure_envelope_key`,
  migration `0011`).
- **`services/lifeos-vcs/src/encrypted.rs`** - `EncryptedBackend` wraps any
  storage backend so the provider (Drive/Dropbox/R2) stores ciphertext only;
  the content hash is over **plaintext**, so identity stays stable.

This is real client-of-the-provider encryption, but it is **not end-to-end**,
because of **who holds the key**:

> Today `workspaces.envelope_key_enc` is the per-workspace key **encrypted under
> the server's master `LIFEOS_SECRET_ENCRYPTION_KEY`** (`crypto.rs`,
> `decrypt_envelope_key`). The Life OS server can therefore decrypt any
> workspace key and read everything. That is correct and intended for the
> default trust model (the operator is you), and it is *required* for the
> server-side memory/search stack to function. But it means the operator is
> fully trusted with content.

**The gap E2E closes is exactly one link in the key chain:** stop wrapping the
per-workspace content key under a server-held master key, and wrap it under
**member keys the server never holds** instead. Everything else - the GCM
envelope, the plaintext-hash blob identity, fail-closed decryption - carries
over unchanged.

---

## 3. Key hierarchy that closes the gap

```
member secret (passphrase OR passkey)          [never leaves the device]
        │  Argon2id(passphrase, per-member salt)   ── or ──  WebAuthn PRF(passkey)
        ▼
member KEK  (32B, Key-Encrypting-Key)          [derived per session, never stored]
        │  AES-256-GCM unwrap
        ▼
CWK  (Content Workspace Key, 32B, random)      [memory-only on the client]
        │  AES-256-GCM
        ▼
per-field ciphertext: attrs, title, annotation body, event payloads, blobs
```

- **CWK** is minted once, by the first member, with a CSPRNG. It is the single
  key that encrypts all content in the workspace. It is **never** sent to the
  server in the clear and **never** wrapped under the server master key.
- Each member has a **KEK** derived from their own secret:
  - **Passphrase path** - `Argon2id(passphrase, salt)` (the prototype uses
    scrypt, a zero-dependency Node built-in, with the same shape). The salt is
    public and stored server-side; salts are not secrets.
  - **Passkey path** - a WebAuthn credential's **PRF/hmac-secret** extension
    yields a stable 32-byte secret bound to the authenticator, giving a
    hardware-backed KEK with no memorized password. This is the preferred path
    for the PWA and is why the issue says "passkey-derived".
- The server stores, per member, only: the **salt** (public) and the
  **wrapped CWK** = `AES-256-GCM(CWK, KEK)`. Both are opaque to the server.

**New storage this implies (eventual build, not this spike):** a
`workspace_key_wraps(workspace_id, member_id, kdf, salt, wrapped_cwk, created_at)`
table, plus a `workspaces.e2e` flag. This is additive and migration-only,
consistent with the no-migration-by-default data model. It is **not** created in
this spike (see §13).

---

## 4. What is ciphertext, what stays plaintext (residual leakage)

An E2E workspace encrypts the **content** columns but must leave the
**structural** columns readable, because the server routes, scopes (RLS by
`workspace_id`), syncs, and reconciles on them.

| Column / field | E2E state | Why |
| --- | --- | --- |
| `entities.attrs` | **ciphertext** | the per-domain content (note body, trade thesis, email text) |
| `entities.title` | **ciphertext** | denormalized display content |
| `annotations.body`, `annotations.anchor` | **ciphertext** | user notes/highlights |
| `events.attrs` (payload) | **ciphertext** | domain payloads may carry content |
| `blob_ref` bytes (via CAS) | **ciphertext** | already handled by `EncryptedBackend` |
| `entities.id`, `workspace_id`, `parent_id` | plaintext | routing, hierarchy, RLS |
| `entities.module`, `type`, `status`, `tier` | plaintext | indexing/routing decisions, generic views |
| `created_at`, `updated_at`, `events.ts` | plaintext | sync ordering, reconciliation |
| `edges` (`src_id`, `dst_id`, `rel`) | plaintext (refs) / ciphertext (`dst_ref`) | graph shape is visible; external targets can be encrypted |

**Residual metadata leakage - stated honestly.** Even a perfectly E2E
workspace lets the operator learn:
- **volume and cadence** - how many entities/events, and *when* (timestamps),
  per module/type;
- **graph shape** - the edge topology (what links to what), even without the
  content of either endpoint;
- **module mix** - that a workspace is heavy on `trading` vs `learning`, etc.

This is the standard metadata-leakage cost of any practical E2E system (Signal
leaks who-messages-whom-and-when; iMessage leaks similar). Hiding it needs
padding, oblivious storage, or constant-rate cover traffic - disproportionate
for this threat model and out of scope. If a workspace needs *content*
confidentiality from the operator, E2E delivers it; if it needs *metadata*
confidentiality, that is a different (and much more expensive) system.

---

## 5. Threat model

| Actor / capability | Sees | Cannot get |
| --- | --- | --- |
| **Honest server (normal operation)** | ciphertext, wrapped CWKs, salts, all structural metadata | any content plaintext; the CWK; any member KEK/passphrase |
| **Insider / malicious operator** (full app + DB + logs, live process) | everything the honest server sees, plus request patterns and in-flight *ciphertext* | content plaintext - the CWK is never in server memory or logs (unlike today's master-key model, where it is derivable) |
| **Attacker with a DB dump at rest** | every stored row, wrapped CWKs, salts, metadata | content plaintext; CWK - must brute-force a member's KDF (Argon2id) offline, so passphrase strength is the wall |
| **Attacker with DB + a memory snapshot of the running server** | same as insider | content plaintext - the server process never holds the CWK, so a core dump yields no key (this is the sharp win over the master-key model) |
| **Network attacker** | TLS-protected transport; at worst ciphertext | content plaintext (TLS + payload already encrypted) |
| **Malicious member** | all content (they are authorized) and can leak it | nothing new - E2E never defended against an authorized member; revocation limits *future* access (§6) |
| **Compromised member device (unlocked)** | that member's content and KEK | out of scope - endpoint security, not server-trust |
| **Lost/locked member device** | nothing without the secret | content - unless a recovery path exists (§7) |

**The security property in one line:** the CWK's confidentiality reduces to the
weakest member's passphrase/passkey strength against an offline Argon2id
attack - and to *nothing the server holds*. Compromising the server yields
metadata, never content.

---

## 6. Membership lifecycle and key rotation

Because the server cannot touch the CWK, membership operations are **client-side
re-wrapping** operations performed by an already-authorized member.

- **Add member.** An existing member (who can unwrap the CWK) derives the new
  member's KEK from an invite secret (or the new member's passkey PRF during a
  live handshake) and writes a new `wrapped_cwk` row for them. The server only
  stores the new wrap. The prototype demonstrates this (`addMember`): a second
  member decrypts the same content with their own passphrase. This ties into
  the existing invite flow (`invites`, migration `0020`), with the extra step
  that the CWK must be wrapped to the invitee - so an invite acceptance is not
  complete until an online member (or a pre-provisioned wrap) supplies it.
- **Remove member / revoke.** Deleting their `wrapped_cwk` row stops *future*
  reads, but that member may already hold the CWK. True revocation requires
  **re-keying**: mint a fresh CWK', re-encrypt content forward under CWK', and
  re-wrap CWK' to the remaining members. This is O(content) work done
  client-side / on a trusted node. **Honest limit:** you cannot claw back what
  the removed member already decrypted and cached; E2E gives forward secrecy
  from the revocation point, not retroactive erasure.
- **Key rotation.** Same mechanism as revocation without removing anyone -
  mint CWK', re-encrypt forward, re-wrap to all members. Old ciphertext stays
  readable under the old CWK (kept wrapped) until a full re-encrypt migrates it;
  a `key_epoch` per row lets old and new coexist during migration.
- **Interaction with `events` (append-only).** `events` cannot be rewritten, so
  a re-key does **not** rewrite historical event payloads; instead new events
  are written under the new epoch and old ones stay under the old (still-wrapped)
  CWK. This is consistent with the append-only invariant - re-keying is additive.

---

## 7. Recovery - the hard human problem

Member-derived keys create the classic zero-knowledge dilemma: **if the only
custodian of a secret forgets it, the data is unrecoverable**, because the
server - by design - cannot help. Options, with their trust costs:

| Recovery option | How | Trust cost |
| --- | --- | --- |
| **None (pure zero-knowledge)** | lost passphrase = lost workspace | maximal security, maximal footgun; unacceptable as the only option for most users |
| **Recovery key** | at bootstrap, generate a high-entropy code that wraps the CWK; user stores it offline (print/password manager) | strong; shifts custody to the user's offline copy; standard (this is how iCloud Advanced Data Protection, 1Password, etc. do it) |
| **Social recovery** | wrap CWK to *k-of-n* trustee members (Shamir split of the KEK) | strong, no single point; operational complexity |
| **Server escrow** | wrap CWK under a server-held key too | **defeats E2E** - the operator can now decrypt; only acceptable as an explicit, per-workspace opt-out, clearly labeled "not zero-knowledge" |

**Recommendation:** default an E2E workspace to **recovery key + optional social
recovery**, never silent server escrow. The prototype does not implement
recovery (lost passphrase = lost data there); this is design-only (see §13).

---

## 8. The hard problems the issue names: search and memory on ciphertext

**Server-side FTS and cognitive-memory consolidation cannot run on ciphertext.
Full stop.** BM25 needs to tokenize text; embeddings need the text; GraphRAG
community summaries and sleep-time consolidation ([AI-MEMORY.md](./AI-MEMORY.md)
§4, §7) need to *read and reason over* content. A server that holds only
ciphertext can do none of this. There are exactly three honest strategies:

### Strategy A - give up server-side AI/search for E2E workspaces
The E2E workspace simply has **no** server FTS, no server vector recall, no
sleep-job consolidation, no server-side Haiku-bot content reasoning. Search and
memory run **client-side**, over decrypted data, on the member's device. This is
what the prototype demonstrates. Cost: the client must fetch+decrypt the working
set to build its own index (§11); thin clients pay an O(N) bootstrap.

### Strategy B - run them on a trusted node (`lifeos-node` as trusted compute)
Move the FTS5 + sqlite-vec + consolidation pipeline off the untrusted server and
onto a node the workspace **owns and trusts** with the CWK - the Mac harness
today, a self-hosted `lifeos-node` tomorrow. The node holds the CWK, decrypts,
builds the derived indices *locally* (which already must live in the un-synced
`lifeos-derived.db`, [DATA-MODEL.md](./DATA-MODEL.md) §5), and answers
search/memory queries. **This does not weaken E2E** - it *relocates the trust
boundary* from "whoever runs the Life OS server" to "the node you run yourself",
which is exactly the boundary a self-hoster already trusts. This is the
strategic answer: E2E workspaces keep the full cognitive stack, but it executes
on trusted compute, never on the SaaS operator's server.

### Strategy C - encrypted search (rejected for this spike)
Searchable Symmetric Encryption (SSE), deterministic/order-preserving
encryption, or homomorphic approaches let a server "search" ciphertext. All leak
access/search patterns (well-published attacks recover plaintext from
deterministic and OPE schemes), add heavy complexity, and still cannot do the
*reasoning* memory consolidation needs. **Rejected.** A narrow exception worth
keeping in mind: a **blind index** (keyed HMAC of exact tokens) enables
server-side *exact-match equality lookup* (e.g. "find the entity whose external
id == X") without full FTS, at the cost of leaking equality patterns - useful
only for specific keyed lookups, never for ranked search.

---

## 9. Feature-degradation matrix

For an E2E workspace, using Strategy A (pure client-side) vs Strategy B (trusted
`lifeos-node`):

| Life OS feature | Server-readable ws (today) | E2E, Strategy A (client only) | E2E, Strategy B (trusted node) |
| --- | --- | --- | --- |
| Full-text search (`entities_fts`) | server FTS5 | **client-side local index (O(N) bootstrap)** | node-side FTS5, full |
| Semantic / vector recall (`entity_vec`) | server sqlite-vec | client-side (needs local embeddings) or **unavailable** on a thin client | node-side, full |
| Memory consolidation / sleep jobs | server | **unavailable** (no server plaintext) | node-side, full |
| GraphRAG community summaries | server | **unavailable** | node-side, full |
| Activation-scored context compiler | server | client-side only | node-side, full |
| Telegram/Haiku bot content RW | full (bot reads content) | **bot sees ciphertext only** - can route/queue by metadata, cannot read/summarize content | bot delegates content work to the node |
| Cross-entity server analytics | server | metadata-only | node-side, full |
| Media ingest (transcribe/caption) | server pipeline | must run on a trusted node (raw media is content) | node-side, full |
| Gating / approvals (draft->approve->execute) | unaffected | unaffected (structural) | unaffected |
| Marketplace / module install | unaffected | unaffected (code, not content) | unaffected |

**The one that stings most: the cloud Haiku bot.** Its entire value is reading
your content to answer/route. In an E2E workspace it sees only ciphertext +
metadata, so it degrades to a **metadata router** (it can see "a new `trading`
entity arrived at 09:31" and enqueue heavy work, but cannot read or summarize
it). Content reasoning must move to the trusted node. This is the sharpest
concrete example of "E2E trades away server AI".

---

## 10. Interaction with sync, events, and the derived DB

- **Sync (last-push-wins, [DATA-MODEL.md](./DATA-MODEL.md) §4).** Unchanged.
  libSQL syncs opaque ciphertext blobs exactly as it syncs plaintext; the
  reconciliation logic operates on ids/timestamps/events, which stay plaintext.
  Row-level last-push-wins over the whole (now-encrypted) `attrs` blob still
  holds; single-writer-per-row discipline still applies.
- **`events` append-only.** Payloads become ciphertext; the log stays
  append-only and remains the reconciliation source of truth. Re-keying is
  additive (§6), never a rewrite.
- **Derived DB ([DATA-MODEL.md](./DATA-MODEL.md) §5).** Already un-synced and
  local. Under E2E it is simply **built from decrypted data on the trusted
  node** and, as today, never syncs - so ciphertext never has to round-trip
  through a derived index. E2E and the "derived state lives in a separate
  un-synced DB" invariant are naturally compatible: the derived DB is exactly
  the trusted-compute artifact Strategy B relies on.

---

## 11. What the prototype demonstrates + measured tradeoffs

`prototypes/e2e-workspace/` is a self-contained, zero-dependency Node prototype
(runs under `nix-shell -p nodejs`, reuses the repo's `nonce || ciphertext`
AES-256-GCM wire format). It builds one E2E workspace where the mock server
holds only ciphertext + wrapped keys, and search runs locally over decrypted
data. It **asserts** (not just prints) each property; see its README to run it.

Measured on Apple Silicon / Node 22 (shape matters more than absolute ms):

| Metric | N = 2,000 | N = 10,000 |
| --- | --- | --- |
| Encrypt attrs, per entity | ~8-10 us | ~7 us |
| Ciphertext size overhead | +43% | +43% |
| **Server-side FTS on the E2E ws** | **impossible - 0 of N indexable** | **impossible - 0 of N indexable** |
| Server-side FTS on an identical plaintext ws | ~15 ms | ~96 ms |
| Local-index bootstrap (fetch+decrypt+index) - the E2E tax | ~27 ms | ~112 ms |
| Local index memory footprint (client holds it) | ~1.0 MB | ~5.2 MB |
| Local search, per query (warm) | ~0.7 ms | ~3.3 ms |
| Local vs server ranking parity | 100% | 100% |

**What the numbers say:**
- **Encryption cost is negligible** (microseconds/entity). It is never the
  reason to hesitate.
- **The +43% size is mostly base64 on the wire** (+33%) plus a fixed 28
  bytes/field of nonce+tag; storing raw ciphertext in a `BLOB` column drops it
  to low single digits for realistic payloads.
- **The real cost is the search-model inversion.** Server FTS is O(1) download
  for the client (send query, get 10 rows). E2E makes it an **O(N) bootstrap**:
  the client must pull and decrypt the working set to build its own index.
  Fine for a device that already holds the data (the Mac, a synced PWA);
  expensive for a thin/new client, and it does not amortize across users the way
  a shared server index does.
- **Ranking quality does not degrade** - identical BM25 over the same text; you
  lose *where* the work runs and *who* can run it, not *how well* it ranks.

---

## 12. Recommendation

1. **Model E2E as an opt-in trust tier, per workspace, not the default.** The
   default workspace stays server-readable so the full cognitive-memory stack
   works. A workspace flips `e2e = true` at creation (re-keying an existing
   populated workspace into E2E is possible but is a bulk re-encrypt migration).
2. **For E2E workspaces, adopt Strategy B (trusted `lifeos-node`) as the target,
   with Strategy A (pure client-side) as the floor.** This keeps search/memory
   working while honoring "the operator cannot read content" - the trust
   boundary moves to the node you run, which a self-hoster already trusts.
3. **Reuse the shipped envelope format unchanged** - the only new crypto is
   member-KEK derivation (Argon2id/passkey-PRF) and CWK wrapping; the GCM
   envelope, plaintext-hash blob identity, and fail-closed decryption all carry
   over from `crypto.rs` / `encrypted.rs`.
4. **Default recovery to a recovery key (plus optional social recovery); never
   silent server escrow.**
5. **Be explicit in-product about what degrades** (the §9 matrix), especially
   that the cloud Haiku bot becomes a metadata router in an E2E workspace.

---

## 13. Open questions and follow-ups (deferred, not done in this spike)

- **Doc index.** When this graduates from spike to build, add a row for this doc
  to `docs/ARCHITECTURE.md` §0 "Document map" and a cross-reference from
  `docs/SECURITY.md` §5 (tenancy isolation). *Not edited here* - this spike
  touches only new files to stay collision-safe with concurrent work.
- **Schema.** Add `workspace_key_wraps` (per-member salt + wrapped CWK) and a
  `workspaces.e2e` flag as an additive migration; add a `key_epoch` to
  content-bearing rows for rotation. Not created in this spike.
- **Passkey PRF.** Validate WebAuthn `prf`/`hmac-secret` support across the PWA's
  target browsers/authenticators before committing to the passkey path as
  primary; keep Argon2id passphrase as the portable fallback.
- **`lifeos-node`.** Strategy B assumes a trusted-compute node beyond the Mac
  harness; its provisioning, attestation, and CWK-custody model are their own
  design (referenced in the mental model as "lifeos-node as trusted compute").
- **Revocation UX.** Re-keying is O(content); define when it runs (immediate vs
  batched sleep-job) and how members are notified their index must rebuild.
- **Metadata-leakage tolerance.** Decide per-tier whether graph-shape/timing
  leakage is acceptable, or whether a future high-security tier needs padding.
