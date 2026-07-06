import React, { useCallback, useEffect, useState } from 'react';
import {
  Brain, RefreshCw, Moon, Search, ShieldQuestion, SkipForward, GitBranch, ScrollText,
} from 'lucide-react';
import { apiCall } from '../lib/api';

// Memory inspector (issue #119, docs/AI-MEMORY.md §11): what the agent
// recalled and WHY. Everything shown here comes off the append-only events
// ledger (memory.* entries) plus the read-model counts - recall is legible,
// never magic. The probe panel runs a real recall (which itself lands on the
// ledger) and renders the activation-score breakdown + provenance per memory.

const fmtTs = (ts) => (ts ? new Date(ts * 1000).toLocaleString() : '');
const pct = (v) => Math.max(2, Math.min(100, Math.round(v * 100)));

function ScoreBar({ label, value, max = 1 }) {
  return (
    <div className="flex items-center gap-2 text-[11px]">
      <span className="w-20 text-neo-text-muted shrink-0">{label}</span>
      <div className="flex-1 h-2 bg-neo-surface-high neo-border overflow-hidden">
        <div className="h-full bg-neo-accent" style={{ width: `${pct(value / max)}%` }} />
      </div>
      <span className="w-14 text-right font-mono">{value.toFixed(3)}</span>
    </div>
  );
}

function MemoryCard({ mem }) {
  const b = mem.breakdown || {};
  return (
    <div className="neo-border bg-neo-surface p-3 space-y-2">
      <div className="text-sm">{mem.content}</div>
      <div className="space-y-1">
        <ScoreBar label="activation" value={b.activation ?? 0} />
        <ScoreBar label="relevance" value={b.relevance ?? 0} max={3} />
        <ScoreBar label="recency" value={b.recency ?? 0} />
        <ScoreBar label="importance" value={b.importance ?? 0} />
        <ScoreBar label="frequency" value={b.frequency ?? 0} max={4} />
      </div>
      <div className="flex flex-wrap items-center gap-2 text-[11px] text-neo-text-muted">
        {b.via_graph_hops != null && (
          <span className="neo-tag bg-neo-accent text-white flex items-center gap-1">
            <GitBranch size={10} /> via {b.via_graph_hops}-hop graph
          </span>
        )}
        <span>provenance:</span>
        {(mem.source_event_ids || []).map((id) => (
          <span key={id} className="font-mono neo-tag bg-neo-surface-high">{id}</span>
        ))}
      </div>
    </div>
  );
}

function LedgerEntry({ entry }) {
  const icon = {
    'memory.recalled': <Search size={13} />,
    'memory.recall.abstained': <ShieldQuestion size={13} />,
    'memory.recall.skipped': <SkipForward size={13} />,
  }[entry.type] || <ScrollText size={13} />;
  const summary = () => {
    const a = entry.attrs || {};
    if (entry.type === 'memory.recalled') {
      return `"${a.query}" -> ${(a.memories || []).length} memories${a.expanded_graph ? ' (graph-expanded)' : ''}`;
    }
    if (entry.type === 'memory.recall.abstained') {
      return `"${a.query}" -> no reliable memory (top ${Number(a.top_activation ?? 0).toFixed(3)} < ${a.threshold})`;
    }
    if (entry.type === 'memory.recall.skipped') return `"${a.query}" -> gate: no memory needed`;
    if (entry.type === 'memory.summary.created') return `${a.level} summary from ${(a.source_event_ids || []).length} events`;
    if (entry.type === 'memory.rule.added') return `rule: ${a.rule}`;
    return JSON.stringify(a).slice(0, 140);
  };
  return (
    <div className="neo-border bg-neo-surface p-2.5 flex items-start gap-2">
      <span className="mt-0.5 text-neo-text-muted shrink-0">{icon}</span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between gap-2">
          <span className="font-mono text-[11px] font-bold">{entry.type}</span>
          <span className="text-[10px] text-neo-text-muted shrink-0">{fmtTs(entry.ts)}</span>
        </div>
        <div className="text-xs text-neo-text truncate">{summary()}</div>
      </div>
    </div>
  );
}

export default function MemoryInspector() {
  const [query, setQuery] = useState('');
  const [probing, setProbing] = useState(false);
  const [result, setResult] = useState(null);
  const [entries, setEntries] = useState([]);
  const [stats, setStats] = useState(null);
  const [rules, setRules] = useState([]);
  const [busy, setBusy] = useState('');
  // Ledger and rules come off two independent endpoints, so each gets its
  // own loading/error/ready status (finding 52) - a failed fetch used to
  // fall through to the same "nothing yet" empty state as a genuinely empty
  // response, masking a real outage as emptiness.
  const [entriesState, setEntriesState] = useState('loading'); // loading | ready | error
  const [rulesState, setRulesState] = useState('loading');

  const load = useCallback(async () => {
    setEntriesState('loading');
    setRulesState('loading');
    const [inspect, rulesRes] = await Promise.all([
      apiCall('GET', '/api/memory/inspect?limit=100'),
      apiCall('GET', '/api/memory/rules'),
    ]);
    if (inspect.ok) {
      setEntries(inspect.data?.entries || []);
      setStats(inspect.data?.stats || null);
      setEntriesState('ready');
    } else {
      setEntriesState('error');
    }
    if (rulesRes.ok) {
      setRules(rulesRes.data?.rules || []);
      setRulesState('ready');
    } else {
      setRulesState('error');
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const probe = async (e) => {
    e.preventDefault();
    if (!query.trim()) return;
    setProbing(true);
    const res = await apiCall('POST', '/api/memory/recall', { query });
    setResult(res.ok ? res.data : { outcome: 'error', error: res.error });
    setProbing(false);
    load(); // the probe itself is now on the ledger
  };

  const maintenance = async (label, path) => {
    setBusy(label);
    await apiCall('POST', path, {});
    setBusy('');
    load();
  };

  const statChips = stats && [
    ['current memories', stats.current_nodes],
    ['superseded', stats.superseded_nodes],
    ['summaries', stats.summaries],
    ['active rules', stats.active_rules],
  ];

  return (
    <div className="p-6 space-y-6 max-w-5xl">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-bold flex items-center gap-2">
          <Brain size={20} /> Memory Inspector
        </h1>
        <div className="flex gap-2">
          <button
            onClick={() => maintenance('sleep', '/api/memory/sleep')}
            disabled={!!busy}
            className="neo-btn bg-neo-surface-high py-1.5 px-3 text-xs flex items-center gap-1"
          >
            <Moon size={13} /> {busy === 'sleep' ? 'consolidating…' : 'Run sleep cycle'}
          </button>
          <button
            onClick={() => maintenance('rebuild', '/api/memory/rebuild')}
            disabled={!!busy}
            className="neo-btn bg-neo-surface-high py-1.5 px-3 text-xs flex items-center gap-1"
          >
            <RefreshCw size={13} /> {busy === 'rebuild' ? 'replaying…' : 'Rebuild from events'}
          </button>
        </div>
      </div>

      {statChips && (
        <div className="flex flex-wrap gap-2">
          {statChips.map(([label, value]) => (
            <span key={label} className="neo-tag bg-neo-surface font-mono text-xs">
              {value ?? 0} {label}
            </span>
          ))}
        </div>
      )}

      <form onSubmit={probe} className="flex gap-2">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder='Probe recall, e.g. "what did I decide about the swing trade?"'
          className="neo-input flex-1 text-sm"
        />
        <button type="submit" disabled={probing} className="neo-btn bg-neo-accent text-white py-1.5 px-4 text-sm">
          {probing ? 'recalling…' : 'Recall'}
        </button>
      </form>

      {result?.outcome === 'skipped' && (
        <div className="neo-border bg-neo-surface p-3 text-sm flex items-center gap-2">
          <SkipForward size={14} /> Self-RAG gate: this turn needs no long-term memory.
        </div>
      )}
      {result?.outcome === 'abstained' && (
        <div className="neo-border bg-neo-yellow p-3 text-sm flex items-center gap-2">
          <ShieldQuestion size={14} />
          No reliable memory (top activation {Number(result.top_activation ?? 0).toFixed(3)} below{' '}
          {result.threshold}) - the agent should say "I don't know" instead of confabulating.
        </div>
      )}
      {result?.outcome === 'error' && (
        <div className="neo-border bg-neo-red text-white p-3 text-sm">{result.error}</div>
      )}
      {result?.outcome === 'recalled' && (
        <div className="space-y-2">
          <div className="text-xs text-neo-text-muted">
            {result.memories.length} memories{result.expanded_graph ? ' - graph expansion ran (multi-hop query)' : ''}
          </div>
          {result.memories.map((m) => <MemoryCard key={m.id} mem={m} />)}
        </div>
      )}

      <div className="grid md:grid-cols-2 gap-6">
        <div className="space-y-2">
          <h2 className="text-sm font-bold">Recall ledger</h2>
          <p className="text-[11px] text-neo-text-muted">
            Every recall/skip/abstention/consolidation is an append-only event - nothing here can be rewritten.
          </p>
          <div className="space-y-2 max-h-[32rem] overflow-y-auto pr-1">
            {entriesState === 'error' && (
              <div className="text-xs text-neo-red font-bold flex items-center gap-2">
                Failed to load the recall ledger.
                <button onClick={load} className="neo-btn bg-neo-surface-high py-0.5 px-2 text-[10px] font-bold">Retry</button>
              </div>
            )}
            {entriesState === 'ready' && entries.length === 0 && (
              <div className="text-xs text-neo-text-muted">No memory activity yet.</div>
            )}
            {entries.map((e) => <LedgerEntry key={e.id} entry={e} />)}
          </div>
        </div>
        <div className="space-y-2">
          <h2 className="text-sm font-bold">Procedural rules</h2>
          <p className="text-[11px] text-neo-text-muted">
            Learned from feedback by consolidation; injected into every compiled context.
          </p>
          {rulesState === 'error' && (
            <div className="text-xs text-neo-red font-bold flex items-center gap-2">
              Failed to load procedural rules.
              <button onClick={load} className="neo-btn bg-neo-surface-high py-0.5 px-2 text-[10px] font-bold">Retry</button>
            </div>
          )}
          {rulesState === 'ready' && rules.length === 0 && (
            <div className="text-xs text-neo-text-muted">No learned rules yet.</div>
          )}
          {rules.map((r) => (
            <div key={r} className="neo-border bg-neo-surface p-2.5 text-sm">{r}</div>
          ))}
        </div>
      </div>
    </div>
  );
}
