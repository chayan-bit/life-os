import React, { useState, useEffect, useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';
import { Database as DbIcon, Share2, Type, ArrowRight, RefreshCw, Plus, CheckCircle, ChevronLeft, ChevronRight } from 'lucide-react';
import { apiCall } from '../lib/api';
import EntityDetailPanel from '../components/EntityDetailPanel';
import GenericTable from '../core/renderers/GenericTable';

const PAGE_SIZE = 20;

// Poll while any job is still queued/running so the queue reflects
// lifeos-drain claims without the user manually refreshing.
const JOBS_POLL_MS = 4000;

export default function DatabaseView() {
  const [selectedEntity, setSelectedEntity] = useState('trade');
  const [jobs, setJobs] = useState([]);
  const [jobsState, setJobsState] = useState('loading'); // 'loading' | 'ready' | 'offline'

  // Live `entities` table browser - GET /api/entity, filtered + paginated.
  const [liveEntities, setLiveEntities] = useState([]);
  const [liveState, setLiveState] = useState('loading'); // 'loading' | 'ready' | 'offline'
  const [liveFilters, setLiveFilters] = useState({ module: '', type: '', status: '' });
  const [livePage, setLivePage] = useState(0);
  const [searchParams, setSearchParams] = useSearchParams();
  const [detailEntityId, setDetailEntityId] = useState(searchParams.get('entity'));

  // Cmd-K's entity search results link here as /database?entity=<id> - open
  // the slide-over directly instead of requiring the user to find the row.
  useEffect(() => {
    const fromUrl = searchParams.get('entity');
    if (fromUrl) setDetailEntityId(fromUrl);
  }, [searchParams]);

  const closeDetail = () => {
    setDetailEntityId(null);
    if (searchParams.get('entity')) {
      const next = new URLSearchParams(searchParams);
      next.delete('entity');
      setSearchParams(next, { replace: true });
    }
  };

  const loadLiveEntities = useCallback(() => {
    setLiveState('loading');
    const qs = new URLSearchParams({ limit: String(PAGE_SIZE), offset: String(livePage * PAGE_SIZE) });
    if (liveFilters.module) qs.set('module', liveFilters.module);
    if (liveFilters.type) qs.set('type', liveFilters.type);
    if (liveFilters.status) qs.set('status', liveFilters.status);
    apiCall('GET', `/api/entity?${qs.toString()}`).then(({ ok, data, offline }) => {
      if (ok && !offline && Array.isArray(data)) {
        setLiveEntities(data);
        setLiveState('ready');
      } else {
        setLiveEntities([]);
        setLiveState('offline');
      }
    });
  }, [liveFilters, livePage]);

  useEffect(() => { loadLiveEntities(); }, [loadLiveEntities]);

  const updateLiveFilter = (key, value) => {
    setLivePage(0);
    setLiveFilters((prev) => ({ ...prev, [key]: value }));
  };

  const [formError, setFormError] = useState('');
  const [formSuccess, setFormSuccess] = useState('');
  const [formModule, setFormModule] = useState('trading');
  const [formType, setFormType] = useState('trade');
  const [formTitle, setFormTitle] = useState('AAPL setup');
  const [formAttrs, setFormAttrs] = useState('{\n  "ticker": "AAPL",\n  "pnl": 120.00,\n  "notes": "Broke out of bull flag"\n}');
  const [apiStatus, setApiStatus] = useState('checking');

  useEffect(() => {
    // Ping lifeos-api Axum server to check if it's awake
    apiCall('GET', '/api/health').then(({ ok, data }) => {
      setApiStatus(ok && data?.status === 'healthy' ? 'online' : 'offline');
    });
  }, []);

  const loadJobs = useCallback(() => {
    apiCall('GET', '/api/jobs?limit=50').then(({ ok, data, offline }) => {
      if (ok && !offline && Array.isArray(data)) {
        setJobs(data);
        setJobsState('ready');
      } else {
        setJobsState('offline');
      }
    });
  }, []);

  useEffect(() => {
    loadJobs();
    // Only poll while something is actually in flight - avoids hammering the
    // API once the queue has settled.
    const hasActive = jobs.some((j) => j.status === 'queued' || j.status === 'running');
    if (!hasActive) return undefined;
    const interval = setInterval(loadJobs, JOBS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadJobs, jobs]);

  const entitiesSchema = [
    { name: 'id', type: 'TEXT (UUID)', desc: 'Primary Key' },
    { name: 'workspace_id', type: 'TEXT', desc: 'Tenant ID for SaaS scaling' },
    { name: 'module', type: 'TEXT', desc: 'e.g. learning, tasks, trading, social, design' },
    { name: 'type', type: 'TEXT', desc: 'e.g. topic, task, project, trade, post, campaign' },
    { name: 'parent_id', type: 'TEXT (Nullable)', desc: 'Self-referencing parent ID for hierarchies' },
    { name: 'status', type: 'TEXT', desc: 'Lifecycle state per module manifest' },
    { name: 'ts', type: 'TIMESTAMP', desc: 'Creation/modification time' },
    { name: 'attrs', type: 'JSON', desc: 'Flexible key-value storage for domain-specific fields' },
  ];

  const entityDefinitions = {
    trade: {
      title: 'Trading Entity',
      module: 'trading',
      type: 'trade',
      status: 'completed',
      attrs: {
        ticker: 'AAPL',
        entry: 172.50,
        exit: 181.20,
        stop_loss: 169.00,
        pnl: 8.70,
        r_multiple: 2.48,
        thesis: 'Double bottom bounce on daily chart backed by volume spike.'
      },
      edges: [
        { label: 'depends_on', target: 'Topic: Technical Analysis' },
        { label: 'uses_asset', target: 'Asset: AAPL screenshot' }
      ]
    },
    task: {
      title: 'Task Entity',
      module: 'tasks',
      type: 'task',
      status: 'review',
      attrs: {
        title: 'Migrate knowledge atlas data',
        due: '2026-06-30',
        priority: 'high',
        assigned_to: 'chayan-aggarwal',
        notes: 'Need to write the atlasAdd wrapper in the shim configuration.'
      },
      edges: [
        { label: 'depends_on', target: 'Project: Life OS Core' }
      ]
    },
    campaign: {
      title: 'Marketing Campaign',
      module: 'marketing',
      type: 'campaign',
      status: 'active',
      attrs: {
        campaign_name: 'Summer Launch 2026',
        channels: ['twitter', 'reddit'],
        budget: 500,
        leads_generated: 48,
        conversion_rate: 0.12
      },
      edges: [
        { label: 'publishes_to', target: 'Social Account: X (@life_os_app)' },
        { label: 'uses_asset', target: 'Asset: Summer Banner SVG' }
      ]
    }
  };

  // Creates the entity via the real API only - no local-only fallback copy.
  // The workspace comes from the app's existing tenant mechanism
  // (lib/api.js::authHeaders reads WORKSPACE_ID_KEY from localStorage and
  // attaches it as X-Workspace-Id on every call apiCall makes), never a
  // hardcoded id constructed here.
  const handleCreateEntity = (e) => {
    e.preventDefault();
    let parsedAttrs = {};
    try {
      parsedAttrs = JSON.parse(formAttrs);
    } catch (err) {
      setFormError('Invalid JSON in attrs field.');
      return;
    }

    setFormError('');
    setFormSuccess('');
    apiCall('POST', '/api/entity', {
      module: formModule,
      type: formType,
      title: formTitle,
      attrs: parsedAttrs,
    }).then(({ ok, offline, error }) => {
      if (offline) {
        setFormError('Cannot reach the Life OS API. Check that lifeos-api is running.');
        return;
      }
      if (!ok) {
        setFormError(error || 'Failed to create entity.');
        return;
      }
      setFormSuccess(`Entity "${formTitle}" created.`);
      setTimeout(() => setFormSuccess(''), 3000);
      loadLiveEntities();
    });
  };

  const selectedData = entityDefinitions[selectedEntity] || null;

  return (
    <div className="flex flex-col gap-8">
      {/* Introduction Banner */}
      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface flex flex-col md:flex-row justify-between items-start md:items-center gap-4">
        <div>
          <h2 className="neo-title-md mb-2 flex items-center gap-2">
            <DbIcon size={24} className="text-neo-blue" />
            The Notion Killer: One Generic Table
          </h2>
          <p className="neo-body-md text-neo-text-muted">
            Life OS stores all domain models in a single, indexable, multi-tenant <strong>entities</strong> table. New domains require <strong>zero database migrations</strong>.
          </p>
        </div>
        <div className={`neo-tag text-xs font-mono font-bold flex items-center gap-1.5 ${
          apiStatus === 'online' ? 'bg-neo-mint' : 'bg-neo-red text-white'
        }`}>
          <div className={`w-2.5 h-2.5 rounded-full ${apiStatus === 'online' ? 'bg-black animate-pulse' : 'bg-neo-surface'}`} />
          <span>Local API: {apiStatus.toUpperCase()}</span>
        </div>
      </div>

      {/* Main interactive grid */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-8">
        
        {/* Core Database Columns */}
        <div className="lg:col-span-4 neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
          <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
            <Type size={18} />
            `entities` Schema
          </h3>
          <div className="flex flex-col gap-3">
            {entitiesSchema.map((col, idx) => (
              <div key={idx} className="p-3 bg-neo-surface-muted neo-border flex justify-between items-center gap-4">
                <div>
                  <span className="neo-label-md font-mono text-neo-blue">{col.name}</span>
                  <p className="text-xs text-neo-text-muted mt-1">{col.desc}</p>
                </div>
                <span className="neo-label-sm bg-neo-surface neo-border px-1.5 py-0.5 text-[10px]">{col.type}</span>
              </div>
            ))}
          </div>
        </div>

        {/* Dynamic Mapping Playground & Custom Entities Form */}
        <div className="lg:col-span-8 flex flex-col gap-6">
          <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
            <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4">
              Row Translator & Live Viewer
            </h3>
            
            {/* Entity Selectors */}
            <div className="flex flex-wrap gap-2 mb-6">
              {['trade', 'task', 'campaign'].map((type) => (
                <button
                  key={type}
                  onClick={() => setSelectedEntity(type)}
                  className={`neo-btn py-1.5 px-3 neo-label-sm ${
                    selectedEntity === type ? 'bg-neo-yellow' : 'bg-neo-surface'
                  }`}
                >
                  {type.toUpperCase()} (SEED)
                </button>
              ))}
            </div>

            {/* Simulated Database Row */}
            {selectedData && (
              <div className="neo-border p-4 bg-gray-950 text-emerald-400 font-mono text-sm neo-radius overflow-x-auto shadow-inner">
                <div className="text-xs text-neo-text-muted mb-2">// Simulated SQL row in entities table</div>
                <div><span className="text-pink-400">id</span>: "d3b07384-d113-4cd4"</div>
                <div><span className="text-pink-400">workspace_id</span>: "personal_workspace"</div>
                <div><span className="text-pink-400">module</span>: "{selectedData.module}"</div>
                <div><span className="text-pink-400">type</span>: "{selectedData.type}"</div>
                <div><span className="text-pink-400">status</span>: "{selectedData.status}"</div>
                <div><span className="text-pink-400">attrs</span>: {'{'}</div>
                {Object.entries(selectedData.attrs).map(([key, val]) => (
                  <div key={key} className="pl-4">
                    <span className="text-sky-400">"{key}"</span>: {typeof val === 'number' ? <span className="text-yellow-400">{val}</span> : <span className="text-orange-300">"{val}"</span>},
                  </div>
                ))}
                <div>{'}'}</div>
              </div>
            )}

            {/* Linked Edges (Graph Layer) */}
            {selectedData && selectedData.edges && selectedData.edges.length > 0 && (
              <div className="mt-6">
                <h4 className="neo-label-md mb-3 text-neo-text-muted">Cross-Domain Graph Edges (`edges` table)</h4>
                <div className="flex flex-col gap-2">
                  {selectedData.edges.map((edge, idx) => (
                    <div key={idx} className="flex items-center gap-3 p-2 bg-neo-surface-muted neo-border text-xs font-semibold">
                      <span className="neo-chip neo-chip--active py-0.5 text-[9px]">{selectedData.title}</span>
                      <div className="flex items-center gap-1 text-neo-red">
                        <Share2 size={12} />
                        <span className="font-mono">{edge.label}</span>
                      </div>
                      <ArrowRight size={14} />
                      <span className="neo-chip neo-chip--draft py-0.5 text-[9px]">{edge.target}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>

          {/* Form to Add Entity */}
          <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
            <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
              <Plus size={18} />
              Insert Live Row Entity
            </h3>

            <form onSubmit={handleCreateEntity} className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="flex flex-col gap-2">
                <label className="neo-label-sm">Module Domain</label>
                <select 
                  value={formModule} 
                  onChange={(e) => setFormModule(e.target.value)}
                  className="p-2 neo-border bg-neo-surface text-xs font-semibold focus:outline-none"
                >
                  <option value="trading">trading</option>
                  <option value="tasks">tasks</option>
                  <option value="marketing">marketing</option>
                  <option value="learning">learning</option>
                </select>
              </div>

              <div className="flex flex-col gap-2">
                <label className="neo-label-sm">Entity Type</label>
                <input 
                  type="text" 
                  value={formType} 
                  onChange={(e) => setFormType(e.target.value)}
                  className="p-2 neo-border bg-neo-surface text-xs font-mono"
                />
              </div>

              <div className="flex flex-col gap-2 md:col-span-2">
                <label className="neo-label-sm">Display Title</label>
                <input 
                  type="text" 
                  value={formTitle} 
                  onChange={(e) => setFormTitle(e.target.value)}
                  className="p-2 neo-border bg-neo-surface text-xs font-semibold"
                />
              </div>

              <div className="flex flex-col gap-2 md:col-span-2">
                <label className="neo-label-sm">Attributes JSON (attrs)</label>
                <textarea 
                  rows={4}
                  value={formAttrs} 
                  onChange={(e) => setFormAttrs(e.target.value)}
                  className="p-2 neo-border bg-neo-surface text-xs font-mono"
                />
              </div>

              {formError && (
                <div className="md:col-span-2 px-3 py-2 bg-neo-red text-white text-xs font-bold neo-border">{formError}</div>
              )}
              {formSuccess && (
                <div className="md:col-span-2 px-3 py-2 bg-neo-mint text-black text-xs font-bold neo-border">{formSuccess}</div>
              )}
              <div className="md:col-span-2">
                <button
                  type="submit"
                  className="neo-btn w-full bg-neo-mint py-2.5 px-4 font-bold uppercase text-xs"
                >
                  Create & Save Entity →
                </button>
              </div>
            </form>
          </div>
        </div>

      </div>

      {/* Live Entities Browser (GET /api/entity) */}
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 border-b-2 border-neo-border pb-4 mb-4">
          <h3 className="neo-title-md">Live `entities` Browser</h3>
          <div className="flex flex-wrap items-center gap-2">
            <input
              placeholder="module"
              value={liveFilters.module}
              onChange={(e) => updateLiveFilter('module', e.target.value)}
              className="p-1.5 neo-border bg-neo-surface text-xs font-mono w-24"
            />
            <input
              placeholder="type"
              value={liveFilters.type}
              onChange={(e) => updateLiveFilter('type', e.target.value)}
              className="p-1.5 neo-border bg-neo-surface text-xs font-mono w-24"
            />
            <input
              placeholder="status"
              value={liveFilters.status}
              onChange={(e) => updateLiveFilter('status', e.target.value)}
              className="p-1.5 neo-border bg-neo-surface text-xs font-mono w-24"
            />
            <button onClick={loadLiveEntities} className="neo-btn py-1.5 px-2 bg-neo-surface" title="Refresh">
              <RefreshCw size={14} className={liveState === 'loading' ? 'animate-spin' : ''} />
            </button>
          </div>
        </div>

        {liveState === 'offline' && (
          <div className="px-3 py-2 bg-neo-red text-white text-xs font-bold neo-border mb-3">
            Backend unreachable - showing no rows.
          </div>
        )}
        {liveState === 'ready' && liveEntities.length === 0 && (
          <div className="px-3 py-2 bg-neo-surface-muted text-xs neo-border mb-3">
            No entities match these filters.
          </div>
        )}

        {liveEntities.length > 0 && (
          <div className="overflow-x-auto">
            <GenericTable
              entities={liveEntities}
              setEntities={setLiveEntities}
              onRowClick={(ent) => setDetailEntityId(ent.id)}
              columns={[
                { key: 'id', label: 'id' },
                { key: 'module', label: 'module' },
                { key: 'type', label: 'type' },
                { key: 'status', label: 'status', editable: true },
                { key: 'title', label: 'title', truncate: true },
              ]}
            />
          </div>
        )}

        <div className="flex items-center justify-between mt-4">
          <span className="text-xs text-neo-text-muted">Page {livePage + 1}</span>
          <div className="flex gap-2">
            <button
              onClick={() => setLivePage((p) => Math.max(0, p - 1))}
              disabled={livePage === 0}
              className="neo-btn py-1 px-2 bg-neo-surface disabled:opacity-40"
            >
              <ChevronLeft size={14} />
            </button>
            <button
              onClick={() => setLivePage((p) => p + 1)}
              disabled={liveEntities.length < PAGE_SIZE}
              className="neo-btn py-1 px-2 bg-neo-surface disabled:opacity-40"
            >
              <ChevronRight size={14} />
            </button>
          </div>
        </div>
      </div>

      {/* Jobs Queue Section - GET /api/jobs, claimed/run by lifeos-drain */}
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        <div className="flex justify-between items-center border-b-2 border-neo-border pb-3 mb-4">
          <h3 className="neo-title-md">Jobs Queue Manager (`jobs` table cloud ↔ Mac)</h3>
          <button onClick={loadJobs} className="neo-btn py-1.5 px-2 bg-neo-surface" title="Refresh">
            <RefreshCw size={14} className={jobsState === 'loading' ? 'animate-spin' : ''} />
          </button>
        </div>

        {jobsState === 'offline' && (
          <div className="px-3 py-2 bg-neo-red text-white text-xs font-bold neo-border mb-3">Backend unreachable.</div>
        )}
        {jobsState === 'ready' && jobs.length === 0 && (
          <div className="px-3 py-2 bg-neo-surface-muted text-xs neo-border mb-3">Queue is empty.</div>
        )}

        <div className="flex flex-col gap-3">
          {jobs.map((job) => (
            <div key={job.id} className="p-4 bg-neo-bg neo-border flex flex-col md:flex-row md:items-center justify-between gap-4">
              <div>
                <div className="flex items-center gap-2 mb-1.5">
                  <span className="neo-label-md font-mono text-xs text-neo-blue">{job.id}</span>
                  <span className="neo-chip py-0.5 text-[9px]">PRIORITY: {job.priority}</span>
                  <span className="neo-tag text-[9px]">kind: {job.kind}</span>
                </div>
                <code className="text-[10px] bg-neo-surface p-1 border font-mono block text-neo-text-muted truncate max-w-lg">
                  {JSON.stringify(job.payload)}
                </code>
              </div>
              <span className={`text-[10px] px-1.5 py-0.5 neo-border font-bold uppercase ${
                job.status === 'done' ? 'bg-neo-mint' :
                job.status === 'running' ? 'bg-neo-yellow' :
                job.status === 'failed' ? 'bg-neo-red text-white' : 'bg-neo-surface'
              }`}>
                {job.status}
              </span>
            </div>
          ))}
        </div>
      </div>

      <EntityDetailPanel entityId={detailEntityId} onClose={closeDetail} />
    </div>
  );
}
