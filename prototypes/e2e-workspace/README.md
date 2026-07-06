# E2E-encrypted workspace - isolated spike (issue #155)

A self-contained, **zero-dependency** prototype of one Life OS workspace where
**the server never holds plaintext entity content or the content key**, and
full-text search still works by running **locally over decrypted data** instead
of server-side.

This is a spike, not an integration. It lives entirely under
`prototypes/e2e-workspace/` and touches no `services/` source. The threat model
it demonstrates is written up in [`docs/E2E-WORKSPACES.md`](../../docs/E2E-WORKSPACES.md).

It deliberately reuses the **exact wire format** of the shipped envelope
machinery so a real build can share blobs byte-for-byte:

- `services/lifeos-api/src/crypto.rs` - `base64(nonce(12) || ciphertext)` AES-256-GCM.
- `services/lifeos-vcs/src/encrypted.rs` - `EncryptedBackend`, same format for blobs.

The **one** conceptual change that turns "server-custodied" into "end-to-end":
the per-workspace content key is wrapped under a **member key derived from a
passphrase** (scrypt here; Argon2id / passkey-PRF in production), **not** under
the server's master `LIFEOS_SECRET_ENCRYPTION_KEY`. The server stores only
wrapped keys and ciphertext, so it can never decrypt.

## Run it

No global installs. Node 22's built-in `crypto` and `node:test` are all it uses.

```sh
cd prototypes/e2e-workspace

# the demonstration + measured tradeoffs (default 2000 entities)
nix-shell -p nodejs --run 'node src/bench.js'
nix-shell -p nodejs --run 'node src/bench.js 10000'   # custom N

# the invariants, as tests
nix-shell -p nodejs --run 'node --test'
```

## What it proves (each step is asserted, not just printed)

1. **Correctness** - a fresh client on a new device, holding only the
   passphrase, re-derives the member key, unwraps the content key, and reads
   back exactly what was written. A wrong passphrase **fails closed** (throws,
   no key, no data) rather than returning garbage.
2. **The security property** - a full server-side DB dump of the E2E workspace
   is scanned for every distinctive content word, across both the stored
   ciphertext and its decoded bytes: **0 leaks**. The server sees only
   structural metadata (`module`, `type`, timestamps) - the residual leak the
   threat model owns up to.
3. **The honest degradation** - server-side FTS can index **0** documents in
   the E2E workspace (there is nothing to tokenize) while indexing all *N* in
   an identical plaintext workspace. Server-side search/AI/memory genuinely
   cannot run here.
4. **The replacement and its price** - a local BM25 index built over decrypted
   data returns the **same top-10 hits** as the plaintext server FTS (100%
   parity in the demo), and we measure the O(*N*) fetch-decrypt-index bootstrap
   that local search costs in place of a free server query.

## Measured tradeoffs

Measured on this Mac (Apple Silicon, Node 22), deterministic corpus. Absolute
times vary by machine; the **shape** is the finding.

| Metric | N = 2,000 | N = 10,000 |
| --- | --- | --- |
| Encrypt attrs, per entity | ~8-10 us | ~7 us |
| Ciphertext size overhead | +43% | +43% |
| **Server-side FTS on E2E ws** | **impossible (0 indexable)** | **impossible (0 indexable)** |
| Server-side FTS on plaintext ws | 15 ms | 96 ms |
| Local-index bootstrap (the E2E tax) | 27 ms | 112 ms |
| Local index memory footprint (client holds it) | ~1.0 MB | ~5.2 MB |
| Local search, per query (warm) | 0.7 ms | 3.3 ms |
| Local vs server ranking parity | 100% | 100% |

Reading the numbers:

- **Encryption is cheap.** A few microseconds per entity of AES-256-GCM is
  noise next to a network round-trip. Encryption overhead is not the reason to
  hesitate.
- **The +43% size overhead is mostly a wire artifact.** It is 12-byte nonce +
  16-byte GCM tag per field (fixed) **plus base64's +33%**. Life OS attrs are
  small JSON, so the fixed 28 bytes dominates the percentage. Storing raw
  ciphertext in a `BLOB` column (no base64) collapses this to ~+28 bytes/field,
  a low single-digit percentage for realistic payloads.
- **The real cost is the search-model inversion.** Server FTS is an O(1)
  download for the client: send a query, get 10 rows. E2E turns that into an
  **O(N) bootstrap** - the client must fetch and decrypt the whole workspace to
  build its own index before the first search. On a device that already holds
  the data (the Mac harness, a synced PWA) this is a one-time ~100 ms for 10k
  entities and then sub-4 ms per query. On a **thin/new client it is the whole
  workspace over the wire**, and it does not amortize the way a server index
  does across many users/devices.
- **Ranking does not degrade** - local BM25 matches server BM25 exactly,
  because it is the same algorithm over the same (now-decrypted) text. What you
  lose is not quality; it is *where the work runs* and *who can run it*.

## Files

| File | Role |
| --- | --- |
| `src/crypto.js` | AES-256-GCM envelope (repo wire format) + scrypt KDF + content-key wrap/unwrap |
| `src/localIndex.js` | Dependency-free BM25 local index (build + query + size estimate) |
| `src/server.js` | Mock honest-but-curious server: ciphertext-only store, plaintext baseline, server FTS |
| `src/client.js` | The trusted client: key custody, encrypt-before-store, decrypt-after-fetch, local search |
| `src/data.js` | Deterministic synthetic corpus + fixed query set |
| `src/bench.js` | Runnable demonstration + measured tradeoffs (asserts every property) |
| `test/e2e.test.js` | The invariants as `node --test` cases |

## What this spike does NOT do (honest scope)

- No membership **revocation / re-keying** implementation (add-member is shown;
  remove-member's forward-secrecy re-encryption is specified in the doc only).
- No **recovery** mechanism (lost passphrase = lost data here; the doc lays out
  recovery-key / escrow / social-recovery options and their trust costs).
- No **semantic/vector** recall or **memory consolidation** - the doc explains
  why those degrade identically to FTS and must move client-side or onto a
  trusted `lifeos-node`.
- No real HTTP/DB wiring. `MockServer` is an in-process stand-in for
  lifeos-api + libSQL, sufficient to prove the trust boundary and measure the
  costs, and nothing more.
