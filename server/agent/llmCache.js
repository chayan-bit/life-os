// Two-layer LLM cache (exact + semantic), API-key mode only (issue #127,
// docs/AGENT-CORE.md §10, §3 step 1a). BOTH layers live in the un-synced
// derived DB via `server/memvec.py` - not an `entities` row like the issue
// text suggests, because the issue's own hard constraint ("never a
// sync-reconciliation source") cannot be met by canonical `entities` rows:
// libSQL has no table-level no-sync flag (docs/DATA-MODEL.md §4.3). This
// mirrors Tool-RAG's (#123) `tool:`-prefixed id convention in the same
// `entity_vec` index, under an `llmcache:<key>` id instead.
//
// On the keyless CLI path (no ANTHROPIC_API_KEY) every function here is a
// no-op - that path is already free/local, so caching buys nothing and would
// only add latency + a python subprocess for no benefit.
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const DEFAULT_CACHE_DISTANCE_THRESHOLD = 0.08;
const CACHE_MEMVEC_TIMEOUT_MS = 20_000;

// Conservative denylist of imperative mutation verbs. When in doubt this
// returns true (don't probe) - a false positive just skips a cache hit; a
// false negative could serve a stale answer for a request that should have
// mutated state.
const ACTIONY_VERB_RE =
  /\b(create|update|delete|post|send|draft|run|schedule|publish|place|cancel|modify|remove|add|execute|approve|invite|share|upload|pay|transfer|order|buy|sell)\b/i;

export function looksActiony(prompt) {
  return ACTIONY_VERB_RE.test(String(prompt || ""));
}

// True only in API-key mode. The keyless CLI path (local Claude Code / Gemini
// CLI already on $PATH) is free, so the cache is bypassed entirely there.
export function isCacheMode() {
  return Boolean(process.env.ANTHROPIC_API_KEY);
}

// Off by default is NOT the semantic-cache default here - the issue's
// "off by default on keyless path" is already covered by isCacheMode() gating
// everything; SEMANTIC_CACHE additionally lets API-key-mode operators disable
// just the fuzzy layer while keeping the free exact-hash layer.
function isSemanticCacheEnabled() {
  return process.env.SEMANTIC_CACHE !== "off";
}

function distanceThreshold() {
  const envValue = Number(process.env.CACHE_DISTANCE_THRESHOLD);
  return Number.isFinite(envValue) ? envValue : DEFAULT_CACHE_DISTANCE_THRESHOLD;
}

// node:crypto ships blake2b in stdlib on both node and python - chosen over
// BLAKE3 (used by the eval_gate.rs precedent) because a blake3 dependency
// would violate the machine's nix-only package policy for this JS runtime.
// The hash algorithm is opaque to correctness; any stable hash works.
export function cacheKey({ model, system, prompt, params } = {}) {
  const canonical = [model ?? "", system ?? "", prompt ?? "", JSON.stringify(params ?? {})].join("||");
  return createHash("blake2b512").update(canonical).digest("hex");
}

function defaultMemvecPath() {
  return process.env.LIFEOS_MEMVEC || path.join(__dirname, "..", "memvec.py");
}

function defaultDerivedDbPath() {
  return process.env.LIFEOS_DERIVED_DB_PATH || "lifeos-derived.db";
}

function runMemvec(args) {
  return new Promise((resolve, reject) => {
    execFile("python3", args, { timeout: CACHE_MEMVEC_TIMEOUT_MS }, (error, stdout) => {
      if (error) return reject(error);
      resolve(stdout);
    });
  });
}

// Default cacheGetFn: shells out to `memvec.py cache-get`. Injectable via
// opts.cacheGetFn so tests never touch python/sqlite-vec.
async function defaultCacheGetFn({ workspace, key, prompt, threshold, semantic }, opts = {}) {
  const dbPath = opts.dbPath || defaultDerivedDbPath();
  const memvecPath = opts.memvecPath || defaultMemvecPath();
  const args = [
    memvecPath,
    "--db",
    dbPath,
    "cache-get",
    "--workspace",
    workspace,
    "--key",
    key,
    "--prompt",
    prompt,
    "--threshold",
    String(threshold),
  ];
  if (!semantic) args.push("--no-semantic");
  const stdout = await runMemvec(args);
  return JSON.parse(stdout.trim());
}

// Default cachePutFn: shells out to `memvec.py cache-put`. Injectable via
// opts.cachePutFn.
async function defaultCachePutFn({ workspace, key, prompt, completion, model }, opts = {}) {
  const dbPath = opts.dbPath || defaultDerivedDbPath();
  const memvecPath = opts.memvecPath || defaultMemvecPath();
  const args = [
    memvecPath,
    "--db",
    dbPath,
    "cache-put",
    "--workspace",
    workspace,
    "--key",
    key,
    "--prompt",
    prompt,
    "--completion",
    completion,
  ];
  if (model) args.push("--model", model);
  await runMemvec(args);
}

// Probes both cache layers for `request` ({ workspace, model, system, prompt,
// params }). ANY error (bad deps, subprocess failure, malformed JSON) or the
// keyless path resolves to a plain miss - a cache failure must never fail the
// turn. Returns { hit: null } | { hit: 'exact', completion } |
// { hit: 'semantic', completion, distance }.
export async function probe(request, opts = {}) {
  if (!isCacheMode()) return { hit: null };

  const cacheGetFn = opts.cacheGetFn ?? defaultCacheGetFn;
  const key = cacheKey(request);

  try {
    const result = await cacheGetFn(
      {
        workspace: request.workspace,
        key,
        prompt: request.prompt,
        threshold: opts.threshold ?? distanceThreshold(),
        semantic: isSemanticCacheEnabled(),
      },
      opts,
    );
    return result?.hit ? result : { hit: null };
  } catch {
    return { hit: null };
  }
}

// Stores `completion` for `request` in both cache layers. Best-effort only -
// never throws, and a no-op on the keyless path.
export async function store(request, completion, opts = {}) {
  if (!isCacheMode()) return;

  const cachePutFn = opts.cachePutFn ?? defaultCachePutFn;
  const key = cacheKey(request);

  try {
    await cachePutFn(
      {
        workspace: request.workspace,
        key,
        prompt: request.prompt,
        completion,
        model: request.model,
      },
      opts,
    );
  } catch {
    // Best-effort write - a store failure must never fail an already-completed turn.
  }
}
