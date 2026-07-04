// Fetches a hot-installed module's full manifest, persisted server-side as a
// generic `module='system'`, `type='module_manifest'` entity (issue #121,
// docs/SELF-EXTENSION-V2.md §6, server/lib/manifestEntity.js). Lets
// useModuleStream.js and InstalledModulePage.jsx mount the real multi-view
// ModuleManifestPage instead of degrading to a flat GenericList.
//
// `POST /api/entity` always server-generates its own row id, so there is no
// `GET /api/entity/module_manifest_<id>` shortcut - the row is found by
// listing module='system'/type='module_manifest' and matching on the
// `title` the write side sets (mirroring manifestEntity.js's naming).
import { apiCall } from './api';

function manifestEntityTitle(id) {
  return `module_manifest_${id}`;
}

export async function fetchInstalledManifest(id) {
  const { ok, data } = await apiCall('GET', '/api/entity?module=system&type=module_manifest&limit=2000');
  if (!ok || !Array.isArray(data)) return null;
  const title = manifestEntityTitle(id);
  const row = data.find((entity) => entity.title === title || entity.attrs?.id === id);
  return row?.attrs ?? null;
}
