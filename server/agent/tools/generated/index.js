// Loader for T2 self-authored agent tools (Voyager-style, docs/
// SELF-EXTENSION-V2.md T2 row, docs/AGENT-CORE.md §6, issue #134).
//
// Each file in this directory is ONE generated tool exporting a default PURE
// REQUEST DESCRIPTOR: { name, description, classification, inputSchema,
// example, request }. `request` only builds { method, path, body? } - it never
// performs I/O itself. The executor (server/agent/executor.js) is the only
// chokepoint that ever calls fetch/httpFn, so generated code never owns the
// wire.
//
// Lazy + failure-tolerant: a bad file is skipped with a warning, never crashes
// the loop or the server boot. Defense-in-depth, duplicated in the T2 build
// validator (server/validators/t2Tool.js) and re-enforced at call time
// (server/agent/executor.js): a descriptor whose `request()` result targets a
// route outside `isRouteAllowed` (server/lib/routeAllowlist.js) is rejected
// here at load time, before it is ever offered to the model.
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isRouteAllowed } from "../../../lib/routeAllowlist.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const LOAD_CHECK_WORKSPACE = "ws_loadcheck";

export function defaultGeneratedDir() {
  return __dirname;
}

function isZodType(value) {
  return Boolean(value) && typeof value.safeParse === "function" && typeof value.parse === "function";
}

// Structural (duck-typed) validation of a loaded module's default export
// against the T2 tool contract. Shared by the loader (below) and the T2
// build-time validator so the shape check is defined exactly once.
export function validateToolShape(mod) {
  const tool = mod?.default ?? null;
  const errors = [];

  if (!tool || typeof tool !== "object") {
    return { valid: false, errors: ["module has no default export object"], tool: null };
  }
  if (typeof tool.name !== "string" || tool.name.length === 0) errors.push("missing/invalid 'name' (must be a non-empty string)");
  if (typeof tool.description !== "string" || tool.description.length === 0) errors.push("missing/invalid 'description'");
  if (tool.classification !== "allowed" && tool.classification !== "gated") {
    errors.push("'classification' must be 'allowed' or 'gated'");
  }
  if (!isZodType(tool.inputSchema)) errors.push("'inputSchema' must be a zod schema (e.g. z.object({...}))");
  if (typeof tool.request !== "function") errors.push("'request' must be a function: ({args, workspaceId}) => ({method, path, body?})");
  if (tool.example === undefined) errors.push("missing required 'example' field (args satisfying inputSchema)");

  if (errors.length > 0) return { valid: false, errors, tool: null };
  return { valid: true, errors: [], tool };
}

// Runs the same example-round-trip + route dry-run checks the T2 build
// validator performs, so a bad tool is rejected identically whether it is
// caught at build time or at load time (a hand-edited file, or a tool moved
// in from elsewhere).
function dryRunRoute(tool) {
  const parsedExample = tool.inputSchema.safeParse(tool.example);
  if (!parsedExample.success) {
    return { ok: false, error: `example does not satisfy inputSchema: ${parsedExample.error.message}` };
  }
  let built;
  try {
    built = tool.request({ args: tool.example, workspaceId: LOAD_CHECK_WORKSPACE });
  } catch (error) {
    return { ok: false, error: `request() threw during load-time dry-run: ${error.message}` };
  }
  if (!built || !isRouteAllowed(built.method, built.path)) {
    return { ok: false, error: `route ${built?.method} ${built?.path} is outside the allowlist` };
  }
  return { ok: true };
}

// The registry-entry shape a loaded generated tool maps to, matching the
// static REGISTRY entries in actionRegistry.js so classify()/executor.js treat
// generated and hand-written tools identically. `inputSchema` here is the raw
// zod shape (`.shape`) the SDK's `tool()` helper expects, mirroring the static
// entries; `requestFn` carries the full descriptor's `request` for the
// executor's dynamic-route path.
function toRegistryEntry(tool, sourceFile) {
  return {
    classification: tool.classification,
    description: tool.description,
    inputSchema: tool.inputSchema.shape,
    requestFn: tool.request,
    generated: true,
    sourceFile,
  };
}

// loadGeneratedTools({ dir?, existingNames? }) -> { tools, warnings }
// `existingNames` is the set of names a generated tool may never shadow -
// PROTECTED_TOOLS union the static REGISTRY keys, passed in by
// actionRegistry.js so this module never has to import it back (no cycle).
export async function loadGeneratedTools(opts = {}) {
  const dir = opts.dir ?? defaultGeneratedDir();
  const existingNames = opts.existingNames ?? new Set();
  const tools = {};
  const warnings = [];

  let entries;
  try {
    entries = await fs.readdir(dir);
  } catch (error) {
    return { tools, warnings: [`could not read generated tools dir '${dir}': ${error.message}`] };
  }

  for (const entry of entries) {
    if (entry === "index.js" || !entry.endsWith(".js")) continue;
    const filePath = path.join(dir, entry);

    let mod;
    try {
      mod = await import(pathToFileURL(filePath).href);
    } catch (error) {
      warnings.push(`skipped ${entry}: failed to load: ${error.message}`);
      continue;
    }

    const shape = validateToolShape(mod);
    if (!shape.valid) {
      warnings.push(`skipped ${entry}: ${shape.errors.join("; ")}`);
      continue;
    }
    const tool = shape.tool;

    if (existingNames.has(tool.name) || tools[tool.name]) {
      warnings.push(`skipped ${entry}: name '${tool.name}' collides with an existing or protected tool`);
      continue;
    }

    const dryRun = dryRunRoute(tool);
    if (!dryRun.ok) {
      warnings.push(`skipped ${entry}: ${dryRun.error}`);
      continue;
    }

    tools[tool.name] = toRegistryEntry(tool, entry);
  }

  return { tools, warnings };
}
