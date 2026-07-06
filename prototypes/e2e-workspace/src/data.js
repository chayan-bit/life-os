// Deterministic synthetic entities so the benchmark is reproducible. A small
// seeded PRNG draws from a fixed vocabulary to build note-like `attrs.body`
// text - enough lexical variety that BM25 ranking is meaningful, no external
// dependency, identical output on every run.

const VOCAB = [
  "trade", "swing", "breakout", "volume", "nifty", "banknifty", "gap", "order",
  "flow", "microstructure", "learning", "topic", "review", "spaced", "recall",
  "task", "deadline", "sprint", "refactor", "rust", "libsql", "turso", "sync",
  "envelope", "encryption", "workspace", "tenant", "memory", "consolidation",
  "embedding", "vector", "search", "index", "annotation", "highlight", "note",
  "campaign", "post", "engagement", "design", "asset", "figma", "render",
  "meeting", "calendar", "email", "thread", "draft", "approve", "publish",
];

function mulberry32(seed) {
  let a = seed >>> 0;
  return function () {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const MODULES = ["trading", "learning", "tasks", "social", "design"];
const TYPES = ["trade", "topic", "task", "post", "asset", "note"];

/** Generate `n` deterministic entities. `seed` fixes the whole corpus. */
export function generateEntities(n, seed = 42) {
  const rand = mulberry32(seed);
  const pick = (arr) => arr[Math.floor(rand() * arr.length)];
  const entities = [];
  for (let i = 0; i < n; i++) {
    const wordCount = 20 + Math.floor(rand() * 60);
    const words = [];
    for (let w = 0; w < wordCount; w++) words.push(pick(VOCAB));
    entities.push({
      id: `ent_${i.toString().padStart(6, "0")}`,
      module: pick(MODULES),
      type: pick(TYPES),
      createdAt: 1_700_000_000 + i,
      title: `${pick(VOCAB)} ${pick(VOCAB)} ${i}`,
      attrs: { body: words.join(" "), i },
    });
  }
  return entities;
}

/** A fixed set of queries for latency measurement. */
export const QUERIES = Object.freeze([
  "envelope encryption workspace",
  "swing trade breakout volume",
  "spaced recall review learning",
  "memory consolidation embedding",
  "refactor rust libsql sync",
]);
