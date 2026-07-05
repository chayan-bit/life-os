// T2 validator (issue #134, docs/SELF-EXTENSION-V2.md §9 T2 row) - the
// tool-contract test for a self-authored agent tool. Pure and unit-testable
// directly: no worktree boot, no scratch DB, no live server needed.
//
// Given a worktree + params {name}, this proves:
//  1. exactly one new file exists at the allowed path
//     server/agent/tools/generated/<name>.js, and it loads cleanly.
//  2. AST-posture import/usage scan (no new deps): only 'zod' may be
//     imported; no raw child_process/fs/net/http/https/fetch(/process.env/
//     dynamic import(.
//  3. shape check via the same meta-schema the loader uses (index.js), plus
//     the schema round-trip (`inputSchema.safeParse(example)` must succeed).
//  4. dry-run: `request({args: example, workspaceId: 'ws_scratch'})` must
//     return a {method, path} that passes the shared `isRouteAllowed` - a
//     descriptor targeting an order/secret/connection path fails here and is
//     never installed.
import fs from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { isRouteAllowed } from "../lib/routeAllowlist.js";
import { validateToolShape } from "../agent/tools/generated/index.js";

const GENERATED_DIR = ["server", "agent", "tools", "generated"];
const DRY_RUN_WORKSPACE = "ws_scratch";

// Only 'zod' may ever be imported (no new deps, and no reach into node/npm
// internals). Matches both ESM `import ... from '<spec>'` and CJS
// `require('<spec>')`.
const IMPORT_RE = /\b(?:import\s+(?:[^;'"]*?\bfrom\s*)?|require\s*\(\s*)['"]([^'"]+)['"]/g;
const DYNAMIC_IMPORT_RE = /\bimport\s*\(/;

// Raw usage tokens that must never appear in a generated tool's source, even
// outside a static import (e.g. `globalThis.require('fs')`, a template-built
// module id). Matched as whole words so ordinary identifiers are not caught.
const FORBIDDEN_TOKENS = ["child_process", "fs", "net", "http", "https"];

function scanDisallowedImports(source) {
  const disallowed = [];
  let match;
  while ((match = IMPORT_RE.exec(source)) !== null) {
    if (match[1] !== "zod") disallowed.push(match[1]);
  }
  return disallowed;
}

function scanForbiddenUsage(source) {
  const hits = [];
  if (DYNAMIC_IMPORT_RE.test(source)) hits.push("dynamic import(");
  if (/\bfetch\s*\(/.test(source)) hits.push("fetch(");
  if (/\bprocess\.env\b/.test(source)) hits.push("process.env");
  for (const token of FORBIDDEN_TOKENS) {
    if (new RegExp(`\\b${token}\\b`).test(source)) hits.push(token);
  }
  return hits;
}

async function listGeneratedFiles(worktreePath) {
  const dir = path.join(worktreePath, ...GENERATED_DIR);
  try {
    const entries = await fs.readdir(dir);
    return entries.filter((f) => f.endsWith(".js") && f !== "index.js");
  } catch {
    return [];
  }
}

// validateT2Tool({ worktreePath, params: { name } }) -> { valid, errors }
export async function validateT2Tool({ worktreePath, params = {} } = {}) {
  const name = params.name;
  if (!name) return { valid: false, errors: ["T2 validator requires params.name"] };

  const files = await listGeneratedFiles(worktreePath);
  if (files.length !== 1) {
    return {
      valid: false,
      errors: [`expected exactly one new file under server/agent/tools/generated/, found ${files.length}: ${files.join(", ") || "(none)"}`],
    };
  }
  const expectedFile = `${name}.js`;
  if (files[0] !== expectedFile) {
    return { valid: false, errors: [`expected server/agent/tools/generated/${expectedFile}, found ${files[0]}`] };
  }

  const filePath = path.join(worktreePath, ...GENERATED_DIR, expectedFile);
  let source;
  try {
    source = await fs.readFile(filePath, "utf8");
  } catch (error) {
    return { valid: false, errors: [`could not read ${expectedFile}: ${error.message}`] };
  }

  const badImports = scanDisallowedImports(source);
  if (badImports.length > 0) {
    return { valid: false, errors: [`disallowed import(s): ${badImports.join(", ")} - only 'zod' may be imported`] };
  }
  const forbiddenUsage = scanForbiddenUsage(source);
  if (forbiddenUsage.length > 0) {
    return { valid: false, errors: [`disallowed usage found in source: ${forbiddenUsage.join(", ")}`] };
  }

  let mod;
  try {
    mod = await import(pathToFileURL(filePath).href);
  } catch (error) {
    return { valid: false, errors: [`module failed to load: ${error.message}`] };
  }

  const shape = validateToolShape(mod);
  if (!shape.valid) return { valid: false, errors: shape.errors };
  const tool = shape.tool;

  if (tool.name !== name) {
    return { valid: false, errors: [`descriptor name '${tool.name}' does not match file name '${name}'`] };
  }

  const parsedExample = tool.inputSchema.safeParse(tool.example);
  if (!parsedExample.success) {
    return { valid: false, errors: [`example does not satisfy inputSchema: ${parsedExample.error.message}`] };
  }

  let built;
  try {
    built = tool.request({ args: tool.example, workspaceId: DRY_RUN_WORKSPACE });
  } catch (error) {
    return { valid: false, errors: [`request() threw during dry-run: ${error.message}`] };
  }
  if (!built || !isRouteAllowed(built.method, built.path)) {
    return { valid: false, errors: [`dry-run route ${built?.method} ${built?.path} is outside the allowlist`] };
  }

  return { valid: true, errors: [] };
}

export const t2ToolValidator = { name: "t2Tool", run: validateT2Tool };
