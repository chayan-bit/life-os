// Persists/reads the full module manifest as a generic `entities` row so a
// hot-installed module can render through the real multi-view
// ModuleManifestPage instead of degrading to a flat GenericList (issue #121,
// docs/SELF-EXTENSION-V2.md §6). Uses the plain generic entity write route
// (services/lifeos-api/src/routes/entity.rs) - no bespoke Rust route needed.
//
// `POST /api/entity` always server-generates its own row id (`new_id("ent")`)
// - there is no client-supplied-id create path - so the lookup key that
// mirrors the manifest's logical identity is `title: module_manifest_<id>`,
// not the row's own `id` column. Upsert is done by listing
// `module='system'&type='module_manifest'` and matching on `title`, then
// PATCH-ing that row's real id (or POST-ing a new row if none matched).
const ENTITY_MODULE = "system";
const ENTITY_TYPE = "module_manifest";

export function manifestEntityTitle(moduleId) {
  return `module_manifest_${moduleId}`;
}

async function findExistingRow(apiBase, moduleId, workspaceId) {
  const qs = new URLSearchParams({ module: ENTITY_MODULE, type: ENTITY_TYPE, limit: "2000" });
  if (workspaceId) qs.set("workspace_id", workspaceId);
  const res = await fetch(`${apiBase}/api/entity?${qs.toString()}`);
  if (!res.ok) throw new Error(`GET /api/entity failed: HTTP ${res.status}`);
  const rows = await res.json();
  const title = manifestEntityTitle(moduleId);
  return rows.find((row) => row.title === title);
}

/// Upserts the manifest entity row. Throws on failure - callers that want
/// "never block the install" semantics (scaffold.js) catch and warn instead
/// of letting this propagate.
export async function persistManifestEntity(apiBase, moduleId, manifest, { workspaceId } = {}) {
  const existing = await findExistingRow(apiBase, moduleId, workspaceId);
  if (existing) {
    const res = await fetch(`${apiBase}/api/entity/${existing.id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ attrs: manifest, workspace_id: workspaceId }),
    });
    if (!res.ok) throw new Error(`PATCH /api/entity/${existing.id} failed: HTTP ${res.status}`);
    return;
  }
  const res = await fetch(`${apiBase}/api/entity`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      module: ENTITY_MODULE,
      type: ENTITY_TYPE,
      title: manifestEntityTitle(moduleId),
      attrs: manifest,
      workspace_id: workspaceId,
    }),
  });
  if (!res.ok) throw new Error(`POST /api/entity failed: HTTP ${res.status}`);
}
