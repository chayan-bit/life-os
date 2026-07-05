// Validator 1 - structural, pure Node, no LLM (issue #74,
// docs/SELF-EXTENSION.md §4). Loads module.js in a vm context (§74's own
// checklist calls this "vm/worker load" - vm.Script is the built-in, no
// worker_threads needed since this runs synchronously inline already),
// ajv-checks it against module.schema.json, then two structural cross-checks
// that ajv alone can't express: no entity-type id collides with an existing
// module's, and every view's `type`/`metric` ref resolves within the
// manifest itself. Replaces the earlier fake `content.includes('id:')`-style
// stub left over from an earlier prototype commit.
import fs from "node:fs/promises";
import path from "node:path";
import Ajv2020 from "ajv/dist/2020.js";
import { loadManifestFromFile } from "../lib/loadManifest.js";
// rendererKinds.js (frontend/src/core/, issue #133) is the single source of
// truth for every kind a Generic<Kind>.jsx renderer exists for - plain,
// dependency-free JS (no JSX/React), so this cross-package relative ESM
// import resolves under plain Node with no bundler and no frontend deps
// added to server/package.json. `metric` is not a renderer kind (it charts
// events through GenericMetricChart instead of listing entities,
// ModuleManifestPage.jsx), so it's unioned in here rather than added to
// RENDERER_KINDS.
import { RENDERER_KINDS } from "../../frontend/src/core/rendererKinds.js";

const SCHEMA_PATH = path.resolve(import.meta.dirname, "module.schema.json");
const DEFAULT_MODULES_DIR = path.resolve(import.meta.dirname, "..", "..", "modules");

const KNOWN_VIEW_KINDS = new Set([...RENDERER_KINDS, "metric"]);

let compiledSchema = null;
async function getValidator() {
  if (compiledSchema) return compiledSchema;
  const schema = JSON.parse(await fs.readFile(SCHEMA_PATH, "utf8"));
  // The schema file itself ships a static enum as a readable baseline, but
  // the live known-kinds set (KNOWN_VIEW_KINDS, derived from rendererKinds.js
  // + "metric") is the actual source of truth - patch it in here so a new
  // T1 renderer kind never needs a second hand-edit to this JSON file.
  schema.$defs.view.properties.kind.enum = [...KNOWN_VIEW_KINDS];
  compiledSchema = new Ajv2020({ allErrors: true, allowUnionTypes: true }).compile(schema);
  return compiledSchema;
}

async function findDuplicateTypeIds(manifest, modulesDir, ownDirName) {
  const errors = [];
  const ownTypeIds = new Set(Object.keys(manifest.entityTypes ?? {}));
  if (ownTypeIds.size === 0) return errors;

  let siblingDirs;
  try {
    siblingDirs = await fs.readdir(modulesDir, { withFileTypes: true });
  } catch {
    return errors; // no modules dir (e.g. fresh scratch repo) - nothing to collide with
  }

  // Excluded by directory name, not manifest.id - the id/dirname consistency
  // check below is a separate, explicit assertion; if they disagree, this
  // loop must still not compare the module against itself.
  for (const entry of siblingDirs) {
    if (!entry.isDirectory() || entry.name === ownDirName || entry.name === "_template") continue;

    const siblingModulePath = path.join(modulesDir, entry.name, "module.js");
    let siblingManifest;
    try {
      siblingManifest = await loadManifestFromFile(siblingModulePath);
    } catch {
      continue; // not a module we can introspect - not this validator's job to flag
    }
    if (!siblingManifest?.entityTypes) continue;

    for (const typeId of Object.keys(siblingManifest.entityTypes)) {
      if (ownTypeIds.has(typeId)) {
        errors.push(`entity type "${typeId}" is already used by module "${entry.name}"`);
      }
    }
  }

  return errors;
}

function findDanglingViewRefs(manifest) {
  const errors = [];
  const typeIds = new Set(Object.keys(manifest.entityTypes ?? {}));
  const metricIds = new Set((manifest.metrics ?? []).map((m) => m.id));

  for (const view of manifest.views ?? []) {
    if (!KNOWN_VIEW_KINDS.has(view.kind)) {
      errors.push(`view "${view.id}" has unknown kind "${view.kind}"`);
      continue;
    }
    if (view.kind === "metric") {
      if (!view.metric || !metricIds.has(view.metric)) {
        errors.push(`view "${view.id}" references unknown metric "${view.metric}"`);
      }
    } else if (!view.type || !typeIds.has(view.type)) {
      errors.push(`view "${view.id}" references unknown entity type "${view.type}"`);
    }
  }

  return errors;
}

export async function validateStructural(modulePath, opts = {}) {
  const modulesDir = opts.modulesDir ?? DEFAULT_MODULES_DIR;

  let manifest;
  try {
    manifest = await loadManifestFromFile(modulePath);
  } catch (error) {
    return { valid: false, errors: [`Failed to load module.js: ${error.message}`] };
  }
  if (!manifest || typeof manifest !== "object") {
    return { valid: false, errors: ["module.js did not call osRegisterModule({...})"] };
  }

  const validate = await getValidator();
  const schemaOk = validate(manifest);
  const errors = schemaOk ? [] : validate.errors.map((e) => `${e.instancePath || "(root)"} ${e.message}`);

  const ownDirName = path.basename(path.dirname(modulePath));
  if (manifest.id !== ownDirName) {
    errors.push(`manifest id "${manifest.id}" does not match its own directory "${ownDirName}"`);
  }

  errors.push(...(await findDuplicateTypeIds(manifest, modulesDir, ownDirName)));
  errors.push(...findDanglingViewRefs(manifest));

  return { valid: errors.length === 0, errors, manifest };
}
