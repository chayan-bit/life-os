import React, { useCallback, useEffect, useState } from 'react';
import { Inbox, Check, X, ShieldAlert } from 'lucide-react';
import { apiCall } from '../lib/api';

// Approval inbox (issue #142): every outward/irreversible action lands as a
// pending_approval / awaiting_approval entity; this lists them, grouped by kind,
// and resolves each with one tap via /api/approval/:id/{approve,deny}. A T5 gate
// (attrs.requires_typed_confirm) refuses a bare approve - the row renders a typed
// input and only enables approve once the exact node/entity phrase is typed
// (the server re-enforces it regardless). Updates are optimistic with rollback.

// A stable, human-facing group label for an approval entity.
function kindOf(entity) {
  if (entity.module === 'bot' && entity.type === 'draft') return 'Drafts';
  if (entity.module === 'pipelines') return 'Build gates';
  if (entity.module === 'storage') return 'Storage backends';
  return 'Other';
}

// The exact phrase a typed-confirm gate demands (matches the server).
function confirmPhrase(entity) {
  const node = entity.attrs?.node;
  if (typeof node === 'string' && node.length > 0) return node;
  return entity.title || entity.id;
}

function summaryOf(entity) {
  return (
    entity.attrs?.text ||
    entity.attrs?.summary ||
    entity.title ||
    `${entity.module}/${entity.type}`
  );
}

export default function ApprovalInbox() {
  const [items, setItems] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [typed, setTyped] = useState({}); // id -> typed phrase
  const [busyId, setBusyId] = useState(null);

  const load = useCallback(async () => {
    setLoading(true);
    const res = await apiCall('GET', '/api/approvals');
    setItems(res.ok && Array.isArray(res.data) ? res.data : []);
    setError(res.ok ? null : res.error || 'Failed to load approvals');
    setLoading(false);
  }, []);

  useEffect(() => { load(); }, [load]);

  const resolve = async (entity, action) => {
    const needsTyped = action === 'approve' && entity.attrs?.requires_typed_confirm;
    const phrase = confirmPhrase(entity);
    if (needsTyped && (typed[entity.id] || '').trim() !== phrase) {
      setError(`Type "${phrase}" exactly to approve this gate.`);
      return;
    }
    setBusyId(entity.id);
    // Optimistic remove; restore the exact prior list on failure.
    const prior = items;
    setItems((cur) => cur.filter((e) => e.id !== entity.id));
    const body = needsTyped ? { typed: typed[entity.id].trim() } : {};
    const res = await apiCall('POST', `/api/approval/${entity.id}/${action}`, body);
    if (!res.ok) {
      setItems(prior);
      setError(res.error || `Failed to ${action}`);
    } else {
      setError(null);
    }
    setBusyId(null);
  };

  const groups = {};
  for (const e of items) (groups[kindOf(e)] ||= []).push(e);

  // Rendered inline (not a nested component) so a keystroke in the typed-confirm
  // input never remounts the row and steals focus.
  const renderRow = (entity) => {
    const needsTyped = entity.attrs?.requires_typed_confirm;
    const phrase = confirmPhrase(entity);
    const canApprove = !needsTyped || (typed[entity.id] || '').trim() === phrase;
    return (
      <div key={entity.id} className="p-3 neo-border bg-neo-surface flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0 flex-1">
            <div className="font-mono text-xs font-bold truncate">{summaryOf(entity)}</div>
            <div className="text-[10px] text-neo-text-muted">
              {entity.module}/{entity.type}
              {entity.attrs?.tier ? ` · ${entity.attrs.tier}` : ''}
            </div>
          </div>
          <div className="shrink-0 flex items-center gap-2">
            <button
              onClick={() => resolve(entity, 'approve')}
              disabled={busyId === entity.id || !canApprove}
              className="neo-btn bg-neo-mint py-1 px-2 text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
            >
              <Check size={11} /> Approve
            </button>
            <button
              onClick={() => resolve(entity, 'deny')}
              disabled={busyId === entity.id}
              className="neo-btn bg-neo-red text-white py-1 px-2 text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
            >
              <X size={11} /> Deny
            </button>
          </div>
        </div>
        {needsTyped && (
          <label className="flex items-center gap-2 text-[10px] text-neo-text-muted">
            <ShieldAlert size={12} className="text-neo-red shrink-0" />
            <span className="shrink-0">Type <code className="font-bold">{phrase}</code> to confirm:</span>
            <input
              aria-label={`Typed confirmation for ${phrase}`}
              value={typed[entity.id] || ''}
              onChange={(e) => setTyped((t) => ({ ...t, [entity.id]: e.target.value }))}
              className="neo-border bg-neo-bg px-2 py-1 font-mono text-[11px] flex-1 min-w-0"
              placeholder={phrase}
            />
          </label>
        )}
      </div>
    );
  };

  return (
    <div className="flex flex-col gap-6 max-w-4xl">
      <div>
        <h2 className="neo-title-md flex items-center gap-2"><Inbox size={20} /> Approval Inbox</h2>
        <p className="text-xs text-neo-text-muted mt-1">
          Every outward or irreversible action is human-gated. Approve enqueues execution;
          deny records a rejection. High-blast-radius gates require typing the exact phrase.
        </p>
      </div>

      {error && <p className="text-xs text-neo-red font-bold">{error}</p>}
      {loading && <p className="text-xs text-neo-text-muted">Loading…</p>}
      {!loading && !items.length && <p className="text-xs text-neo-text-muted">Nothing awaiting approval.</p>}

      {Object.entries(groups).map(([group, rows]) => (
        <div key={group} className="flex flex-col gap-2">
          <span className="neo-label-sm text-neo-text-muted">{group} ({rows.length})</span>
          <div className="flex flex-col gap-1.5">
            {rows.map((entity) => renderRow(entity))}
          </div>
        </div>
      ))}
    </div>
  );
}
