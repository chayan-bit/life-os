// Subscribes to GET /api/stream/modules (SSE) so a freshly self-extension-
// built module hot-adds its tab with zero manual refresh. Falls back to
// polling GET /api/event?type=module.installed when EventSource is
// unavailable (older browsers, some embedded webviews) or the connection
// errors out - the UI behavior is identical either way, just less instant.
import { useEffect, useRef } from 'react';
import { API_BASE, WORKSPACE_ID_KEY, apiCall } from './api';
import { fetchInstalledManifest } from './manifestApi';
import { registerModule } from './moduleRegistry';

const POLL_FALLBACK_MS = 10000;

function manifestFromEvent(ev) {
  // module.installed events are emitted by the self-extension builder
  // (docs/SELF-EXTENSION.md) with attrs carrying at least an id/name; until
  // that builder exists, this also accepts module.requested so the dev flow
  // (issue #22) is visibly wired end-to-end rather than silently inert.
  const id = ev.attrs?.id || ev.entity_id || ev.id;
  return { id, name: ev.attrs?.name || ev.attrs?.prompt || id, version: '0.0.0-dev', icon: '+' };
}

// Registers the minimal manifest immediately (so the tab appears with zero
// delay), then swaps in the persisted full manifest (issue #121) once it
// resolves - server/lib/manifestEntity.js's row can lag the SSE event by a
// beat since it's written earlier in the same install but over a separate
// request. Falls back to the minimal manifest (already registered) if the
// entity isn't found or the fetch fails - no error surfaced to the user.
function registerFromEvent(ev) {
  const minimal = manifestFromEvent(ev);
  registerModule(minimal);
  fetchInstalledManifest(minimal.id)
    .then((full) => {
      if (full) registerModule(full);
    })
    .catch(() => {
      // Minimal manifest above still renders via the GenericList fallback.
    });
}

export function useModuleStream() {
  const pollRef = useRef(null);
  const lastSeenRef = useRef('');

  useEffect(() => {
    let es = null;
    let cancelled = false;

    const startPolling = () => {
      if (pollRef.current) return;
      const poll = async () => {
        const { ok, data } = await apiCall('GET', '/api/event?type=module.installed&limit=20');
        if (ok && Array.isArray(data)) {
          for (const ev of [...data].reverse()) {
            if (ev.id > lastSeenRef.current) {
              lastSeenRef.current = ev.id;
              registerFromEvent(ev);
            }
          }
        }
        if (!cancelled) pollRef.current = setTimeout(poll, POLL_FALLBACK_MS);
      };
      poll();
    };

    if (typeof window.EventSource === 'function') {
      const workspaceId = localStorage.getItem(WORKSPACE_ID_KEY) || '';
      es = new EventSource(`${API_BASE}/api/stream/modules?workspace_id=${encodeURIComponent(workspaceId)}`);
      es.addEventListener('module.installed', (e) => {
        try { registerFromEvent(JSON.parse(e.data)); } catch { /* malformed payload - skip */ }
      });
      es.onerror = () => {
        // SSE dropped (backend down, proxy issue, etc.) - degrade to polling
        // instead of leaving the user with a permanently stale module list.
        es.close();
        es = null;
        startPolling();
      };
    } else {
      startPolling();
    }

    return () => {
      cancelled = true;
      es?.close();
      if (pollRef.current) clearTimeout(pollRef.current);
    };
  }, []);
}
