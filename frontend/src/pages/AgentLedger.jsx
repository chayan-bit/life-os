import React, { useEffect, useState, useCallback } from 'react';
import { History, Undo2, Layers, Check, X, Ban } from 'lucide-react';
import { apiCall } from '../lib/api';
import { undoAction, undoPlan } from '../lib/agentActions';

// "Agent did X" ledger - reads the action.applied / action.undone events
// straight from the append-only events log (issue #36). Per-action and
// per-plan undo both re-invoke the stored reverse_action as a NEW forward
// action; the original event is never rewritten or deleted.
export default function AgentLedger() {
  const [events, setEvents] = useState([]);
  // loading | ready | error - a failed fetch used to silently fall back to
  // an empty events array and render "No agent actions yet.", masking an
  // outage as emptiness (finding 52).
  const [status, setStatus] = useState('loading');
  const [busyId, setBusyId] = useState(null);

  const load = useCallback(async () => {
    setStatus('loading');
    const [applied, undone] = await Promise.all([
      apiCall('GET', '/api/event?type=action.applied&limit=500'),
      apiCall('GET', '/api/event?type=action.undone&limit=500'),
    ]);
    if (!applied.ok || !undone.ok) {
      setStatus('error');
      return;
    }
    const undoneOf = new Set((undone.data || []).map((e) => e.attrs?.undoes_event_id).filter(Boolean));
    setEvents((applied.data || []).map((e) => ({ ...e, undone: undoneOf.has(e.id) })));
    setStatus('ready');
  }, []);

  useEffect(() => { load(); }, [load]);

  const plans = {};
  const standalone = [];
  for (const ev of events) {
    const planId = ev.attrs?.plan_id;
    if (planId) {
      (plans[planId] ||= []).push(ev);
    } else {
      standalone.push(ev);
    }
  }

  const runUndo = async (ev) => {
    setBusyId(ev.id);
    await undoAction(ev);
    await load();
    setBusyId(null);
  };

  const runUndoPlan = async (planId) => {
    setBusyId(planId);
    await undoPlan(plans[planId]);
    await load();
    setBusyId(null);
  };

  const reversibleLabel = (ev) => (ev.attrs?.reverse_action ? null : 'not reversible');

  const ActionRow = ({ ev }) => (
    <div className="p-3 neo-border bg-neo-surface flex items-center justify-between gap-3">
      <div className="min-w-0 flex-1">
        <div className="font-mono text-xs font-bold truncate">{ev.attrs?.tool}</div>
        <div className="text-[10px] text-neo-text-muted">{new Date(ev.ts * 1000).toLocaleString()}</div>
        <pre className="text-[10px] text-neo-text-muted font-mono mt-1 overflow-x-auto max-w-full">{JSON.stringify(ev.attrs?.args, null, 0)}</pre>
      </div>
      <div className="shrink-0 flex items-center gap-2">
        {ev.undone && <span className="neo-tag bg-neo-surface-high text-neo-text-muted text-[9px]"><Check size={9} /> undone</span>}
        {!ev.undone && reversibleLabel(ev) && (
          <span className="neo-tag bg-neo-surface-high text-neo-text-muted text-[9px]"><Ban size={9} /> not reversible</span>
        )}
        {!ev.undone && !reversibleLabel(ev) && (
          <button
            onClick={() => runUndo(ev)}
            disabled={busyId === ev.id}
            className="neo-btn bg-neo-yellow py-1 px-2 text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
          >
            <Undo2 size={11} /> {busyId === ev.id ? 'Undoing…' : 'Undo'}
          </button>
        )}
      </div>
    </div>
  );

  return (
    <div className="flex flex-col gap-6 max-w-4xl">
      <div>
        <h2 className="neo-title-md flex items-center gap-2"><History size={20} /> Agent Ledger</h2>
        <p className="text-xs text-neo-text-muted mt-1">
          Every mutation the agent has applied, read straight from the append-only <code>events</code> log.
          Undo re-applies the stored reverse action as a new event - history is never rewritten.
        </p>
      </div>

      {status === 'loading' && <p className="text-xs text-neo-text-muted">Loading…</p>}
      {status === 'error' && (
        <div className="text-xs text-neo-red font-bold flex items-center gap-2">
          Failed to load the agent ledger.
          <button onClick={load} className="neo-btn bg-neo-surface-high py-1 px-2 text-[10px] font-bold">Retry</button>
        </div>
      )}
      {status === 'ready' && !events.length && <p className="text-xs text-neo-text-muted">No agent actions yet.</p>}

      {Object.entries(plans).map(([planId, evs]) => {
        const allUndone = evs.every((e) => e.undone);
        return (
          <div key={planId} className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <span className="neo-label-sm flex items-center gap-1.5"><Layers size={12} /> Plan {planId.slice(0, 14)}… ({evs.length} steps)</span>
              {!allUndone && (
                <button
                  onClick={() => runUndoPlan(planId)}
                  disabled={busyId === planId}
                  className="neo-btn bg-neo-yellow py-1 px-2 text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
                >
                  <Undo2 size={11} /> {busyId === planId ? 'Undoing batch…' : 'Undo entire plan'}
                </button>
              )}
              {allUndone && <span className="neo-tag bg-neo-surface-high text-neo-text-muted text-[9px]">all undone</span>}
            </div>
            <div className="flex flex-col gap-1.5 pl-3 border-l-2 border-neo-border">
              {evs.map((ev) => <ActionRow key={ev.id} ev={ev} />)}
            </div>
          </div>
        );
      })}

      {standalone.length > 0 && (
        <div className="flex flex-col gap-2">
          {Object.keys(plans).length > 0 && <span className="neo-label-sm">Standalone actions</span>}
          <div className="flex flex-col gap-1.5">
            {standalone.map((ev) => <ActionRow key={ev.id} ev={ev} />)}
          </div>
        </div>
      )}
    </div>
  );
}
