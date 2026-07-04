import React, { useEffect, useState } from 'react';
import { RefreshCw, Loader2 } from 'lucide-react';
import { apiCall } from '../lib/api';
import GenericList from './renderers/GenericList';
import GenericTable from './renderers/GenericTable';
import GenericBoard from './renderers/GenericBoard';
import GenericCalendar from './renderers/GenericCalendar';
import GenericGallery from './renderers/GenericGallery';
import GenericDetail from './renderers/GenericDetail';
import GenericTimeline from './renderers/GenericTimeline';
import GenericMap from './renderers/GenericMap';
import EntityDetailPanel from '../components/EntityDetailPanel';
import GenericMetricChart from './metrics/GenericMetricChart';
import { resolveField } from './renderers/displayHelpers';

const KIND_RENDERERS = {
  list: GenericList,
  table: GenericTable,
  board: GenericBoard,
  calendar: GenericCalendar,
  gallery: GenericGallery,
  timeline: GenericTimeline,
  map: GenericMap,
};

// A view's optional `filter: { field, onOrBefore: 'today' }` narrows the
// fetched entities client-side (the API has no date-range query params) -
// e.g. the Tasks manifest's "Today" view (issue #40).
function applyFilter(entities, filter) {
  if (!filter) return entities;
  if (filter.onOrBefore === 'today') {
    const today = new Date().toISOString().slice(0, 10);
    return entities.filter((e) => {
      const raw = resolveField(e, filter.field);
      return raw && String(raw).slice(0, 10) <= today;
    });
  }
  return entities;
}

// Renders a day-1 module manifest (lib/moduleManifests.js) with zero bespoke
// view code: a view picker over `manifest.views`, each backed by
// `GET /api/entity?module=<id>&type=<view.type>` and rendered through the
// Generic* component matching `view.kind`, driven by `entityTypes.<type>.display`
// (issue #39 - the manifest-driven module system docs/MODULES.md §1 describes).
export default function ModuleManifestPage({ manifest }) {
  const [activeViewId, setActiveViewId] = useState(manifest.views[0]?.id);
  const [entities, setEntities] = useState([]);
  const [events, setEvents] = useState([]);
  const [state, setState] = useState('loading');
  const [selectedId, setSelectedId] = useState(null);
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState(null);
  const [reloadKey, setReloadKey] = useState(0);

  const view = manifest.views.find((v) => v.id === activeViewId) || manifest.views[0];
  const entityType = manifest.entityTypes[view?.type] || {};
  // `kind: 'metric'` views chart raw events (manifest.metrics[view.metric])
  // through GenericMetricChart instead of listing entities - e.g. an equity
  // curve computed from trade.closed events (docs/MODULES.md §2.4).
  const metric = view?.kind === 'metric' ? (manifest.metrics || []).find((m) => m.id === view.metric) : null;

  useEffect(() => {
    if (!view) return;
    setState('loading');
    if (view.kind === 'metric') {
      apiCall('GET', '/api/event?limit=2000').then(({ ok, data, offline }) => {
        if (offline) { setState('offline'); return; }
        setEvents(ok ? data || [] : []);
        setState('ready');
      });
      return;
    }
    apiCall('GET', `/api/entity?module=${encodeURIComponent(manifest.id)}&type=${encodeURIComponent(view.type)}&limit=500`)
      .then(({ ok, data, offline }) => {
        if (offline) { setState('offline'); return; }
        setEntities(ok ? data || [] : []);
        setState('ready');
      });
  }, [manifest.id, view?.type, view?.kind, reloadKey]);

  const runSync = async () => {
    if (!manifest.sync) return;
    setSyncing(true);
    setSyncResult(null);
    const { ok, data, error, status } = await apiCall('POST', manifest.sync.path, {});
    setSyncing(false);
    setSyncResult(
      ok
        ? `Synced ${data?.synced ?? 0} new (${data?.skipped ?? 0} already up to date).`
        : status === 501
          ? 'Not configured yet - see docs/MANUAL-SETUP.md.'
          : error || 'Sync failed.'
    );
    if (ok) setReloadKey((k) => k + 1);
  };

  const Renderer = KIND_RENDERERS[view?.kind] || GenericList;
  const viewEntities = applyFilter(entities, view?.filter);

  return (
    <div className="flex flex-col gap-6">
      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 className="neo-title-md mb-1 flex items-center gap-2">
              <span>{manifest.icon}</span> {manifest.name}
            </h2>
            <p className="neo-body-md text-neo-text-muted">
              {Object.values(manifest.entityTypes).length} entity types, {manifest.views.length} views - rendered entirely
              through the generic renderer system, no bespoke UI code per module.
            </p>
          </div>
          {manifest.sync && (
            <button
              onClick={runSync}
              disabled={syncing}
              className="neo-btn py-1.5 px-3 bg-neo-yellow text-xs font-bold flex items-center gap-1.5 shrink-0 disabled:opacity-50"
            >
              {syncing ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
              {syncing ? 'Syncing…' : manifest.sync.label}
            </button>
          )}
        </div>
        {syncResult && <p className="text-xs text-neo-text-muted font-mono mt-2">{syncResult}</p>}
      </div>

      <div className="flex gap-2 flex-wrap">
        {manifest.views.map((v) => (
          <button
            key={v.id}
            data-view-tab={v.id}
            onClick={() => setActiveViewId(v.id)}
            className={`neo-btn py-1.5 px-3 text-xs font-bold ${view?.id === v.id ? 'bg-neo-yellow' : 'bg-neo-surface'}`}
          >
            {v.label}
          </button>
        ))}
      </div>

      <div data-view-id={view?.id} className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        {state === 'offline' && <p className="text-xs text-neo-red font-bold">Backend unreachable.</p>}
        {state === 'loading' && <p className="text-xs text-neo-text-muted">Loading…</p>}
        {state === 'ready' && view?.kind === 'metric' && metric && (
          <GenericMetricChart metric={metric} events={events} />
        )}
        {state === 'ready' && view?.kind === 'metric' && !metric && (
          <p className="text-xs text-neo-text-muted">No metric named "{view.metric}" declared on this manifest.</p>
        )}
        {state === 'ready' && view?.kind === 'table' && (
          <Renderer entities={viewEntities} setEntities={setEntities} columns={view.columns} onRowClick={(e) => setSelectedId(e.id)} />
        )}
        {state === 'ready' && view?.kind !== 'table' && view?.kind !== 'metric' && (
          <Renderer
            entities={viewEntities}
            setEntities={setEntities}
            display={entityType.display}
            dateField={view?.dateField}
            groupByField={view?.groupBy}
            columns={view?.columns}
            mediaField={view?.mediaField}
            latField={view?.latField}
            lngField={view?.lngField}
            onSelect={(e) => setSelectedId(e.id)}
          />
        )}
      </div>

      {selectedId && <EntityDetailPanel entityId={selectedId} onClose={() => setSelectedId(null)} />}
    </div>
  );
}
