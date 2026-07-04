// Tool registry + Tool-RAG (issue #123, docs/AGENT-CORE.md §4). Mounting every
// registered tool every turn is token waste and dulls tool choice, so this
// embeds each tool's `name: description` once into the derived-DB vector index
// (reusing `server/memvec.py` / sqlite-vec - the same infra `entity_vec`
// already uses, keyed by a `tool:<name>` id prefix rather than a separate
// index) and retrieves only the top-K relevant tools per turn plus an
// always-on core set. Any retrieval failure - or an empty index - falls back
// to the full catalog; a turn must never fail because Tool-RAG failed.
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Always offered regardless of retrieval - the reads + core actuators the
// loop leans on for almost every turn.
export const CORE_TOOLS = Object.freeze([
  "search.query",
  "entity.list",
  "entity.get",
  "entity.create",
  "entity.update",
  "event.append",
  "draft.create",
]);

export const TOOL_RAG_K = Number(process.env.TOOL_RAG_K) || 16;

// Tool embeddings are global (not per-workspace), so they live under a
// reserved memvec workspace label rather than any real tenant's workspace.
// `entity_vec`/`entity_vec_meta` live in the un-synced derived DB and are
// written directly by memvec.py, never through the entities API, so this
// label never touches canonical workspace validation.
const TOOL_MEMVEC_WORKSPACE = "_system_tools";
const TOOL_ID_PREFIX = "tool:";

// The digest marker is a real entities row (tenant-neutral choice per
// docs/DATA-MODEL.md §4.3: canonical entities carry a real workspace_id, so
// it is written under whichever workspace triggers the first turn - cheap and
// consistent, versus a Mac-local file that a cloud tier could never see).
const DIGEST_MODULE = "agent";
const DIGEST_TYPE = "tool_index_digest";

const MEMVEC_TIMEOUT_MS = 20_000;

function defaultMemvecPath() {
  return process.env.LIFEOS_MEMVEC || path.join(__dirname, "..", "memvec.py");
}

function defaultDerivedDbPath() {
  return process.env.LIFEOS_DERIVED_DB_PATH || "lifeos-derived.db";
}

function runMemvec(args) {
  return new Promise((resolve, reject) => {
    execFile("python3", args, { timeout: MEMVEC_TIMEOUT_MS }, (error, stdout) => {
      if (error) return reject(error);
      resolve(stdout);
    });
  });
}

// Default embedFn: shells out to `memvec.py embed`. Injectable via opts.embedFn.
async function defaultEmbedFn(name, description, opts = {}) {
  const dbPath = opts.dbPath || defaultDerivedDbPath();
  const memvecPath = opts.memvecPath || defaultMemvecPath();
  await runMemvec([
    memvecPath,
    "--db",
    dbPath,
    "embed",
    "--workspace",
    TOOL_MEMVEC_WORKSPACE,
    "--id",
    `${TOOL_ID_PREFIX}${name}`,
    "--text",
    `${name}: ${description}`,
  ]);
}

// Default retrieveFn: shells out to `memvec.py query`, returns raw ids
// (still `tool:`-prefixed), best-first. Injectable via opts.retrieveFn.
async function defaultRetrieveFn(prompt, k, opts = {}) {
  const dbPath = opts.dbPath || defaultDerivedDbPath();
  const memvecPath = opts.memvecPath || defaultMemvecPath();
  const stdout = await runMemvec([
    memvecPath,
    "--db",
    dbPath,
    "query",
    "--workspace",
    TOOL_MEMVEC_WORKSPACE,
    "--text",
    prompt,
    "--k",
    String(k),
  ]);
  return stdout
    .split("\n")
    .map((line) => line.split("\t")[0]?.trim())
    .filter(Boolean);
}

// Deterministic content digest over [{name, description}], sorted by name so
// registry key order never changes the digest.
function computeDigest(registry) {
  const entries = Object.keys(registry)
    .sort()
    .map((name) => ({ name, description: registry[name].description }));
  return createHash("sha256").update(JSON.stringify(entries)).digest("hex");
}

function readDigestAttr(entity) {
  const raw = entity?.attrs;
  if (raw && typeof raw === "object") return raw.digest ?? null;
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw).digest ?? null;
    } catch {
      return null;
    }
  }
  return null;
}

async function findDigestEntity(httpFn, workspaceId) {
  const res = await httpFn(
    "GET",
    `/api/entity?module=${DIGEST_MODULE}&type=${DIGEST_TYPE}&workspace_id=${workspaceId}&limit=1`,
  );
  if (!res?.ok || !Array.isArray(res.data) || res.data.length === 0) return null;
  return res.data[0];
}

async function writeDigestEntity(httpFn, workspaceId, existing, digest) {
  const attrs = { digest, updated_at: Date.now() };
  if (existing?.id) {
    await httpFn("PATCH", `/api/entity/${existing.id}`, { attrs, workspace_id: workspaceId });
    return;
  }
  await httpFn("POST", "/api/entity", {
    module: DIGEST_MODULE,
    type: DIGEST_TYPE,
    attrs,
    workspace_id: workspaceId,
  });
}

// Embeds every registered tool's `name: description` into the derived-DB
// vector index, skipping re-embedding when the registry's content digest is
// unchanged since the last index. Never throws - a failure here must never
// fail the turn that triggered it (called lazily, on the first turn).
export async function indexTools(registry, opts = {}) {
  const embedFn = opts.embedFn ?? defaultEmbedFn;
  const httpFn = opts.httpFn;
  const workspaceId = opts.workspaceId;
  const digest = computeDigest(registry);

  try {
    const existing = await findDigestEntity(httpFn, workspaceId);
    if (existing && readDigestAttr(existing) === digest) {
      return { reembedded: false, digest };
    }
    for (const name of Object.keys(registry)) {
      await embedFn(name, registry[name].description, opts);
    }
    await writeDigestEntity(httpFn, workspaceId, existing, digest);
    return { reembedded: true, digest };
  } catch (error) {
    return { reembedded: false, digest, error: error.message };
  }
}

const stripToolPrefix = (id) => (id.startsWith(TOOL_ID_PREFIX) ? id.slice(TOOL_ID_PREFIX.length) : id);

const unique = (items) => [...new Set(items)];

// Retrieves the top-K semantically relevant tools for `prompt` plus the
// always-on core set, returned in registry order. On any retrieval error, or
// an empty index, returns the FULL catalog with `fallback: true` - retrieval
// never fails the turn.
export async function retrieveTools(prompt, registry, opts = {}) {
  const registryNames = Object.keys(registry);
  const retrieveFn = opts.retrieveFn ?? defaultRetrieveFn;
  const k = opts.k ?? TOOL_RAG_K;

  let hits;
  try {
    hits = await retrieveFn(prompt, k, opts);
  } catch {
    return { tools: registryNames, fallback: true };
  }

  if (!Array.isArray(hits) || hits.length === 0) {
    return { tools: registryNames, fallback: true };
  }

  const topNames = hits.map(stripToolPrefix).filter((name) => registry[name]);
  const core = CORE_TOOLS.filter((name) => registry[name]);
  const selected = new Set(unique([...core, ...topNames]));
  const tools = registryNames.filter((name) => selected.has(name));
  return { tools, fallback: false };
}
