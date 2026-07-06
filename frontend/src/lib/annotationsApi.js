// Annotation storage layer for the Knowledge Atlas. Replaces the old
// localStorage note store with the workspace-scoped `/api/annotation` CRUD
// surface (services/lifeos-api). The UI keeps its own rich annotation shape;
// this module is the only place that translates to/from the API row shape,
// so KnowledgeAtlas.jsx never touches storage directly.

import { apiCall } from './api';

// Legacy localStorage key the atlas used before the API existed, plus a flag
// so the one-time import can never double-run.
const LEGACY_KEY = 'KA_ANNOTATIONS_V1';
const MIGRATED_FLAG = 'KA_ANNOTATIONS_MIGRATED_V1';

function safeParse(value) {
  if (value == null) return null;
  if (typeof value !== 'string') return value;
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

// An API annotation row -> the UI annotation shape KnowledgeAtlas renders.
export function fromApi(row) {
  const anchor = safeParse(row.anchor) || {};
  const attrs = row.attrs || {};
  return {
    id: row.id,
    kind: row.kind,
    topicId: row.entity_id ?? null,
    anchorType: anchor.type || 'selection',
    quote: anchor.quote ?? null,
    text: row.body || '',
    link: attrs.link || null,
    createdAt: attrs.createdAt || new Date((row.created_at || 0) * 1000).toISOString(),
    answer: attrs.answer || '',
    answeredAt: attrs.answeredAt || null,
  };
}

// A UI annotation -> the create/update request body. The subject lives in
// `entity_id`, the selection in `anchor`, and everything the UI needs but the
// schema has no column for (link/answer/createdAt) rides in `attrs`.
export function toApi(ann) {
  return {
    entity_id: ann.topicId || null,
    kind: ann.kind,
    body: ann.text || '',
    anchor: { type: ann.anchorType || 'selection', quote: ann.quote ?? null },
    attrs: {
      link: ann.link || null,
      answer: ann.answer || '',
      answeredAt: ann.answeredAt || null,
      createdAt: ann.createdAt || new Date().toISOString(),
    },
  };
}

export async function listAnnotations() {
  const { ok, data } = await apiCall('GET', '/api/annotation');
  if (!ok || !Array.isArray(data)) return [];
  return data.map(fromApi);
}

export async function createAnnotation(ann) {
  const { ok, data } = await apiCall('POST', '/api/annotation', toApi(ann));
  return ok && data ? fromApi(data) : null;
}

export async function updateAnnotation(id, ann) {
  const { ok, data } = await apiCall('PATCH', `/api/annotation/${id}`, toApi(ann));
  return ok && data ? fromApi(data) : null;
}

export async function deleteAnnotation(id) {
  const { ok } = await apiCall('DELETE', `/api/annotation/${id}`);
  return ok;
}

// One-time, best-effort import of legacy localStorage notes into the API.
// Only marks itself done (and clears the legacy key) once every row imported
// cleanly - so an offline first load retries next time instead of dropping
// notes, while a successful import can never run twice.
export async function migrateLegacyAnnotations() {
  if (localStorage.getItem(MIGRATED_FLAG)) return 0;
  const legacy = safeParse(localStorage.getItem(LEGACY_KEY));
  if (!Array.isArray(legacy) || legacy.length === 0) {
    localStorage.setItem(MIGRATED_FLAG, '1');
    localStorage.removeItem(LEGACY_KEY);
    return 0;
  }
  let imported = 0;
  let allOk = true;
  for (const ann of legacy) {
    const saved = await createAnnotation(ann);
    if (saved) imported += 1;
    else allOk = false;
  }
  if (allOk) {
    localStorage.setItem(MIGRATED_FLAG, '1');
    localStorage.removeItem(LEGACY_KEY);
  }
  return imported;
}
