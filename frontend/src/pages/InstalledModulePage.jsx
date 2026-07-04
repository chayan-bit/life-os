import React, { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { Sparkles } from 'lucide-react';
import { apiCall } from '../lib/api';
import { fetchInstalledManifest } from '../lib/manifestApi';
import { getModule, registerModule } from '../lib/moduleRegistry';
import { getManifest } from '../lib/moduleManifests';
import GenericList from '../core/renderers/GenericList';
import ModuleManifestPage from '../core/ModuleManifestPage';

// A registry entry only supports the full ModuleManifestPage renderer once
// it actually carries entityTypes + views - a minimal {id,name,version,icon}
// object (or an old localStorage snapshot from before issue #121) doesn't.
function hasFullManifest(manifest) {
  return Boolean(manifest?.entityTypes && Object.keys(manifest.entityTypes).length && manifest?.views?.length);
}

// Landing page for a module. Day-1 modules (lib/moduleManifests.js, e.g.
// 'learning') have a static manifest and always render through
// ModuleManifestPage's generic view system (issue #39). A module hot-
// installed via the self-extension stream (issue #29) now also gets the
// same treatment once its full manifest arrives (issue #121,
// docs/SELF-EXTENSION-V2.md §6) - only a genuinely manifest-less module
// falls back to an honest flat list of its entities.
export default function InstalledModulePage() {
  const { id } = useParams();
  const staticManifest = getManifest(id);
  const manifest = getModule(id);
  const fullManifest = hasFullManifest(manifest);
  const [entities, setEntities] = useState([]);
  const [state, setState] = useState('loading');
  const [, forceUpdate] = useState(0);

  // Hooks must run unconditionally even when a static/full manifest takes
  // over rendering below - each effect no-ops in that case.
  useEffect(() => {
    if (staticManifest) return;
    const onMounted = () => forceUpdate((n) => n + 1);
    window.addEventListener('lifeos:module-mounted', onMounted);
    return () => window.removeEventListener('lifeos:module-mounted', onMounted);
  }, [staticManifest]);

  // A registry entry hydrated from an old minimal localStorage snapshot has
  // no views - fetch the full manifest once so it upgrades to
  // ModuleManifestPage without waiting for a fresh install event.
  useEffect(() => {
    if (staticManifest || fullManifest) return;
    let cancelled = false;
    fetchInstalledManifest(id).then((full) => {
      if (!cancelled && full) registerModule(full);
    });
    return () => { cancelled = true; };
  }, [id, staticManifest, fullManifest]);

  useEffect(() => {
    if (staticManifest || fullManifest) return;
    apiCall('GET', `/api/entity?module=${encodeURIComponent(id)}`).then(({ ok, data, offline }) => {
      if (offline) { setState('offline'); return; }
      setEntities(ok ? data || [] : []);
      setState('ready');
    });
  }, [id, staticManifest, fullManifest]);

  if (staticManifest) return <ModuleManifestPage manifest={staticManifest} />;
  if (fullManifest) return <ModuleManifestPage manifest={manifest} />;

  return (
    <div className="flex flex-col gap-6">
      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface">
        <h2 className="neo-title-md mb-2 flex items-center gap-2">
          <Sparkles size={22} /> {manifest?.name || id}
        </h2>
        <p className="neo-body-md text-neo-text-muted">
          Hot-installed via self-extension - this generic list renders its entities directly
          ({'module: ' + id}) until a full manifest (views/board/calendar/...) ships for it.
        </p>
      </div>
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        {state === 'offline' && <p className="text-xs text-neo-red font-bold">Backend unreachable.</p>}
        {state === 'ready' && <GenericList entities={entities} display={{ title: 'title', badge: 'type' }} />}
      </div>
    </div>
  );
}
