// A tiny local search index (BM25 lexical ranking) that runs entirely on the
// client over DECRYPTED entity text. This is the piece that replaces
// server-side FTS5 for an E2E workspace: the server cannot index ciphertext,
// so the index is built and queried on the trusted device instead.
//
// It is deliberately hand-rolled and dependency-free - the point of the spike
// is to MEASURE the client-side cost of local search, not to ship a search
// engine. A real build would use the same substrate the rest of Life OS uses
// (FTS5 + sqlite-vec) running locally / on lifeos-node; the cost profile
// (O(N) fetch+decrypt to build, in-memory postings) is what transfers.

const TOKEN_RE = /[a-z0-9]+/g;

export function tokenize(text) {
  const out = [];
  let m;
  const lower = text.toLowerCase();
  while ((m = TOKEN_RE.exec(lower)) !== null) {
    if (m[0].length > 1) out.push(m[0]);
  }
  return out;
}

/**
 * Build an immutable BM25 index from `[{ id, text }]` docs.
 * Returns a frozen object; the internal Maps are populated once here and never
 * mutated afterwards (immutability at the public boundary).
 */
export function buildIndex(docs) {
  const postings = new Map(); // term -> Map(docId -> termFreq)
  const docLen = new Map(); // docId -> token count
  let totalLen = 0;

  for (const { id, text } of docs) {
    const terms = tokenize(text);
    docLen.set(id, terms.length);
    totalLen += terms.length;
    for (const t of terms) {
      let bucket = postings.get(t);
      if (bucket === undefined) {
        bucket = new Map();
        postings.set(t, bucket);
      }
      bucket.set(id, (bucket.get(id) ?? 0) + 1);
    }
  }

  const n = docs.length;
  const avgLen = n === 0 ? 0 : totalLen / n;
  return Object.freeze({ postings, docLen, n, avgLen });
}

const K1 = 1.5;
const B = 0.75;

/** BM25 search. Returns `[{ id, score }]` sorted by descending relevance. */
export function search(index, query, limit = 10) {
  const { postings, docLen, n, avgLen } = index;
  const scores = new Map();

  for (const term of new Set(tokenize(query))) {
    const bucket = postings.get(term);
    if (bucket === undefined) continue;
    const df = bucket.size;
    const idf = Math.log(1 + (n - df + 0.5) / (df + 0.5));
    for (const [docId, tf] of bucket) {
      const len = docLen.get(docId);
      const denom = tf + K1 * (1 - B + (B * len) / avgLen);
      const contribution = idf * ((tf * (K1 + 1)) / denom);
      scores.set(docId, (scores.get(docId) ?? 0) + contribution);
    }
  }

  return [...scores.entries()]
    .map(([id, score]) => ({ id, score }))
    .sort((a, b) => b.score - a.score)
    .slice(0, limit);
}

/** Rough in-memory footprint of an index, in bytes (for the tradeoff table). */
export function estimateIndexBytes(index) {
  let bytes = 0;
  for (const [term, bucket] of index.postings) {
    bytes += term.length + 8; // term string + map overhead (approx)
    bytes += bucket.size * 16; // docId ref + int freq per posting (approx)
  }
  bytes += index.docLen.size * 16;
  return bytes;
}
