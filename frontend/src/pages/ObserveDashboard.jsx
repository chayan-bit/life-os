import React, { useEffect, useState, useCallback } from 'react';
import { Activity, RefreshCw } from 'lucide-react';
import { apiCall } from '../lib/api';
import ErrorBoundary from '../components/ErrorBoundary';
import {
  ResponsiveContainer, LineChart, Line, BarChart, Bar,
  XAxis, YAxis, Tooltip, CartesianGrid,
} from 'recharts';

// Observe dashboard (issue #145): every card below reads fields already
// stamped on `events` rows by the harness - `server/agent/loop.js::persistTurn`
// (tokens/gated/cache/recoveries on `agent.turn`) and
// `server/build/commit.js::emitBuildEvent` (per-node build outcomes) - and
// aggregated server-side by `GET /api/metrics` (services/lifeos-api/src/routes/metrics.rs).
// No client-side event scraping, no new endpoint, no new chart library
// (recharts is already a dependency - see core/metrics/GenericMetricChart.jsx).
const COLORS = ['#3b82f6', '#10b981', '#f59e0b', '#ef4444', '#8b5cf6', '#14b8a6'];

// `{ a: 1, b: 2 }` -> `[{ label: 'a', value: 1 }, ...]`, the shape every bar
// chart below renders. Exported for the data-mapping tests.
export function objectToBars(obj) {
  return Object.entries(obj || {}).map(([label, value]) => ({ label, value: Number(value) || 0 }));
}

// Cache hit rate = (exact + semantic) / all agent.turn rows the cache
// breakdown covers. `null` (not 0) when there's no data yet, so the UI can
// tell "no turns" apart from "0% hit rate".
export function cacheHitRate(cacheByResult) {
  const c = cacheByResult || {};
  const hits = (c.exact || 0) + (c.semantic || 0);
  const total = hits + (c.none || 0);
  return total > 0 ? hits / total : null;
}

function Card({ title, children }) {
  return (
    <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface flex flex-col gap-3">
      <h3 className="neo-label-md">{title}</h3>
      {children}
    </div>
  );
}

function Empty({ children }) {
  return <p className="text-xs text-neo-text-muted">{children}</p>;
}

function Stat({ label, value }) {
  return (
    <div className="flex flex-col">
      <span className="text-lg font-bold font-mono">{value}</span>
      <span className="text-[10px] text-neo-text-muted uppercase tracking-wide">{label}</span>
    </div>
  );
}

export default function ObserveDashboard() {
  const [metrics, setMetrics] = useState(null);
  const [status, setStatus] = useState('loading'); // loading | ready | offline | error

  // A non-offline API error (e.g. a 500) used to leave `metrics` null while
  // still flipping status to 'ready', so the render below dereferenced
  // `metrics.*` on null and crashed the whole page (finding 22). Now an
  // error gets its own status and metrics is only ever read once it's a
  // real object.
  const load = useCallback(() => {
    setStatus('loading');
    apiCall('GET', '/api/metrics')
      .then(({ ok, data, offline }) => {
        if (offline) { setMetrics(null); setStatus('offline'); return; }
        if (!ok || !data) { setMetrics(null); setStatus('error'); return; }
        setMetrics(data);
        setStatus('ready');
      })
      .catch(() => { setMetrics(null); setStatus('error'); });
  }, []);

  useEffect(() => { load(); }, [load]);

  const turnsByDay = metrics?.turns_by_day || [];
  const cacheByResult = metrics?.cache_by_result || {};
  const hitRate = cacheHitRate(cacheByResult);
  const gatedVsAllowed = [
    { label: 'Allowed', value: metrics?.agent_turns_allowed || 0 },
    { label: 'Gated', value: metrics?.agent_turns_gated || 0 },
  ];
  const recoveryByKind = objectToBars(metrics?.recovery_by_kind);
  const strategyLeaderboard = metrics?.strategy_leaderboard || [];
  const recentBuildNodes = metrics?.recent_build_nodes || [];
  const buildRunsByOutcome = metrics?.build_runs_by_outcome || {};

  // ErrorBoundary wraps just this page's own content for now - a global
  // wrap around the whole router tree in App.jsx is the natural follow-up
  // but that file is owned by another worker, so it's left out of scope.
  return (
    <ErrorBoundary>
    <div className="flex flex-col gap-6">
      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface">
        <h2 className="neo-title-md mb-2 flex items-center gap-2">
          <Activity size={22} /> Observe
        </h2>
        <p className="neo-body-md text-neo-text-muted">
          Live rollups over the append-only <code>events</code> log: agent-turn tokens/latency,
          cache hit rate, gated-vs-allowed actions, self-healing recoveries, and recent build runs.
        </p>
      </div>

      <div className="flex items-center gap-3">
        <button onClick={load} className="neo-btn bg-neo-surface-high py-2 px-3 flex items-center gap-1.5 text-xs font-bold">
          <RefreshCw size={14} /> Refresh
        </button>
        {status === 'loading' && <span className="text-xs text-neo-text-muted">Loading metrics...</span>}
        {status === 'offline' && <span className="text-xs text-neo-red font-bold">Backend unreachable.</span>}
        {status === 'error' && <span className="text-xs text-neo-red font-bold">Failed to load metrics. Try refresh.</span>}
      </div>

      {status === 'ready' && metrics && (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          <Card title="Agent turns & tokens">
            <div className="flex gap-6 mb-2">
              <Stat label="Total turns" value={metrics.agent_turns_total ?? 0} />
              <Stat label="Tokens in" value={metrics.tokens_in ?? 0} />
              <Stat label="Tokens out" value={metrics.tokens_out ?? 0} />
              <Stat label="Avg latency (ms)" value={Math.round(metrics.avg_latency_ms ?? 0)} />
            </div>
            {turnsByDay.length === 0 ? (
              <Empty>No agent turns recorded yet.</Empty>
            ) : (
              <ResponsiveContainer width="100%" height={200}>
                <LineChart data={turnsByDay}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="day" tick={{ fontSize: 10 }} />
                  <YAxis tick={{ fontSize: 10 }} />
                  <Tooltip />
                  <Line type="monotone" dataKey="turns" name="Turns" stroke={COLORS[0]} strokeWidth={2} dot={false} />
                  <Line type="monotone" dataKey="tokens_in" name="Tokens in" stroke={COLORS[1]} strokeWidth={2} dot={false} />
                  <Line type="monotone" dataKey="tokens_out" name="Tokens out" stroke={COLORS[2]} strokeWidth={2} dot={false} />
                </LineChart>
              </ResponsiveContainer>
            )}
          </Card>

          <Card title="Cache hit rate">
            {Object.keys(cacheByResult).length === 0 ? (
              <Empty>No cache-eligible turns yet (API-key mode only, see server/agent/llmCache.js).</Empty>
            ) : (
              <>
                <Stat
                  label="Hit rate"
                  value={hitRate === null ? 'n/a' : `${Math.round(hitRate * 100)}%`}
                />
                <ResponsiveContainer width="100%" height={160}>
                  <BarChart data={objectToBars(cacheByResult)}>
                    <CartesianGrid strokeDasharray="3 3" />
                    <XAxis dataKey="label" tick={{ fontSize: 10 }} />
                    <YAxis tick={{ fontSize: 10 }} />
                    <Tooltip />
                    <Bar dataKey="value" fill={COLORS[1]} />
                  </BarChart>
                </ResponsiveContainer>
              </>
            )}
          </Card>

          <Card title="Gated vs allowed actions">
            {gatedVsAllowed.every((d) => d.value === 0) ? (
              <Empty>No agent turns recorded yet.</Empty>
            ) : (
              <ResponsiveContainer width="100%" height={160}>
                <BarChart data={gatedVsAllowed}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="label" tick={{ fontSize: 10 }} />
                  <YAxis tick={{ fontSize: 10 }} />
                  <Tooltip />
                  <Bar dataKey="value" fill={COLORS[3]} />
                </BarChart>
              </ResponsiveContainer>
            )}
          </Card>

          <Card title="Self-heal recoveries">
            <div className="flex gap-6 mb-2">
              <Stat label="Recovery actions" value={metrics.recovery_action_count ?? 0} />
              <Stat label="Turns recovered" value={metrics.recovery_turns_count ?? 0} />
            </div>
            {recoveryByKind.length === 0 ? (
              <Empty>No recovery actions recorded yet.</Empty>
            ) : (
              <ResponsiveContainer width="100%" height={160}>
                <BarChart data={recoveryByKind}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="label" tick={{ fontSize: 10 }} />
                  <YAxis tick={{ fontSize: 10 }} />
                  <Tooltip />
                  <Bar dataKey="value" fill={COLORS[4]} />
                </BarChart>
              </ResponsiveContainer>
            )}
          </Card>

          <Card title="Strategy optimizer leaderboard">
            {strategyLeaderboard.length === 0 ? (
              <Empty>
                No strategy decisions recorded yet (rag.rewrite / planner.prompt decision groups,
                see server/agent/strategy.js - issue #156).
              </Empty>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-xs font-mono">
                  <thead>
                    <tr className="text-left text-neo-text-muted">
                      <th className="pr-3 py-1">Group</th>
                      <th className="pr-3 py-1">Variant</th>
                      <th className="pr-3 py-1">Plays</th>
                      <th className="pr-3 py-1">Wins</th>
                      <th className="pr-3 py-1">Rate</th>
                    </tr>
                  </thead>
                  <tbody>
                    {strategyLeaderboard.map((row, i) => (
                      <tr key={`${row.group}-${row.variant}-${i}`} className="border-t border-neo-border/30">
                        <td className="pr-3 py-1">{row.group}</td>
                        <td className="pr-3 py-1 font-bold">{row.variant}</td>
                        <td className="pr-3 py-1">{row.plays}</td>
                        <td className="pr-3 py-1">{row.successes}</td>
                        <td className="pr-3 py-1">{Math.round((row.rate || 0) * 100)}%</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>

          <Card title="Recent build runs">
            <div className="flex gap-4 mb-2 flex-wrap">
              {objectToBars(buildRunsByOutcome).map((b) => (
                <span key={b.label} className="neo-tag text-[10px]">{b.label}: {b.value}</span>
              ))}
            </div>
            {recentBuildNodes.length === 0 ? (
              <Empty>No build runs recorded yet.</Empty>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-xs font-mono">
                  <thead>
                    <tr className="text-left text-neo-text-muted">
                      <th className="pr-3 py-1">Run</th>
                      <th className="pr-3 py-1">Node</th>
                      <th className="pr-3 py-1">Tier</th>
                      <th className="pr-3 py-1">Outcome</th>
                    </tr>
                  </thead>
                  <tbody>
                    {recentBuildNodes.map((n, i) => (
                      <tr key={`${n.run_id}-${n.node}-${i}`} className="border-t border-neo-border/30">
                        <td className="pr-3 py-1">{n.run_id}</td>
                        <td className="pr-3 py-1">{n.node}</td>
                        <td className="pr-3 py-1">{n.tier}</td>
                        <td className={`pr-3 py-1 font-bold ${n.outcome === 'failed' ? 'text-neo-red' : ''}`}>
                          {n.outcome}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>

          <Card title="Eval pass rate">
            <Empty>
              Eval runs (server/evals/run.js) currently write only to the local
              server/evals/history.jsonl file, never to the events log - there's no API
              surface to render a pass-rate chart from yet. This card will populate once
              eval results are stamped as events.
            </Empty>
          </Card>
        </div>
      )}
    </div>
    </ErrorBoundary>
  );
}
