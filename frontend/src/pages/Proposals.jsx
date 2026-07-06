import React, { useCallback, useEffect, useState } from 'react';
import { GitPullRequest, Check, X, AlertTriangle, MessageSquare, RefreshCw } from 'lucide-react';
import { apiCall } from '../lib/api';

// Proposals - the GitHub-analog on the entity graph (issue #148). Lists draft
// changesets, shows a per-attr before/after diff (mirroring GenericDetail's
// attr styling), and lets an editor/owner merge or reject. Merge is refused
// server-side when a target drifted since drafting (needs_rebase). Review
// comments are plain annotations on the proposal entity (no parallel store) -
// GET/POST /api/annotation with entity_id = the proposal id.

// Roles allowed to merge/reject a proposal (server re-enforces regardless).
const CAN_REVIEW = new Set(['owner', 'editor']);

// Render any JSON attr value as compact text for the diff cells.
function fmt(v) {
  if (v === null || v === undefined) return '∅';
  if (typeof v === 'string') return v;
  return JSON.stringify(v);
}

const STATUS_STYLE = {
  open: 'bg-neo-yellow',
  merged: 'bg-neo-mint',
  rejected: 'bg-neo-red text-white',
};

export default function Proposals() {
  const [proposals, setProposals] = useState([]);
  const [yourRole, setYourRole] = useState(null);
  const [selected, setSelected] = useState(null); // proposal id
  const [diff, setDiff] = useState(null);
  const [comments, setComments] = useState([]);
  const [commentBody, setCommentBody] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(false);

  const canReview = CAN_REVIEW.has(yourRole);

  const loadList = useCallback(async () => {
    setLoading(true);
    const res = await apiCall('GET', '/api/proposal');
    if (res.ok) {
      setProposals(Array.isArray(res.data) ? res.data : []);
      setError(null);
    } else {
      setError(res.error || 'Failed to load proposals');
    }
    // Role drives whether the merge/reject controls render (best-effort).
    const me = await apiCall('GET', '/api/members');
    if (me.ok) setYourRole(me.data?.your_role ?? null);
    setLoading(false);
  }, []);

  useEffect(() => { loadList(); }, [loadList]);

  const openProposal = useCallback(async (id) => {
    setSelected(id);
    setDiff(null);
    setComments([]);
    const d = await apiCall('GET', `/api/proposal/${id}/diff`);
    if (d.ok) setDiff(d.data);
    else setError(d.error || 'Failed to load diff');
    const c = await apiCall('GET', `/api/annotation?entity_id=${id}&kind=comment`);
    if (c.ok) setComments(Array.isArray(c.data) ? c.data : []);
  }, []);

  const merge = async (id) => {
    setBusy(true);
    const res = await apiCall('POST', `/api/proposal/${id}/merge`, {});
    if (!res.ok) setError(res.error || 'Merge failed');
    else setError(null);
    setBusy(false);
    await loadList();
    await openProposal(id);
  };

  const reject = async (id) => {
    setBusy(true);
    const res = await apiCall('POST', `/api/proposal/${id}/reject`, {});
    if (!res.ok) setError(res.error || 'Reject failed');
    else setError(null);
    setBusy(false);
    await loadList();
    await openProposal(id);
  };

  const addComment = async (e) => {
    e.preventDefault();
    const body = commentBody.trim();
    if (!body || !selected) return;
    setBusy(true);
    const res = await apiCall('POST', '/api/annotation', {
      entity_id: selected,
      kind: 'comment',
      body,
    });
    if (res.ok) {
      setCommentBody('');
      const c = await apiCall('GET', `/api/annotation?entity_id=${selected}&kind=comment`);
      if (c.ok) setComments(Array.isArray(c.data) ? c.data : []);
    } else {
      setError(res.error || 'Failed to add comment');
    }
    setBusy(false);
  };

  const selectedProposal = proposals.find((p) => p.id === selected);
  const selectedStatus = diff?.status || selectedProposal?.attrs?.status || 'open';
  const isOpen = selectedStatus === 'open';

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h2 className="neo-title-md flex items-center gap-2"><GitPullRequest size={20} /> Proposals</h2>
        <p className="text-xs text-neo-text-muted mt-1">
          Draft a changeset against shared entities, review the before/after diff, then merge or reject.
          Merging applies each change as an event-sourced, reversible update. {yourRole && <>Your role: <strong className="font-mono">{yourRole}</strong>.</>}
        </p>
      </div>

      {error && <p className="text-xs text-neo-red font-bold">{error}</p>}
      {loading && <p className="text-xs text-neo-text-muted">Loading…</p>}

      <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,20rem)_1fr] gap-6">
        {/* List */}
        <div className="flex flex-col gap-2">
          {!loading && proposals.length === 0 && (
            <p className="text-xs text-neo-text-muted">No proposals yet.</p>
          )}
          {proposals.map((p) => {
            const st = p.attrs?.status || 'open';
            return (
              <button
                key={p.id}
                onClick={() => openProposal(p.id)}
                className={`text-left p-3 neo-border bg-neo-surface flex flex-col gap-1 ${
                  selected === p.id ? 'neo-shadow border-neo-blue' : 'neo-shadow-hover'
                }`}
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="text-sm font-bold text-neo-text truncate">{p.attrs?.title || p.title || p.id}</span>
                  <span className={`neo-tag text-[9px] font-mono ${STATUS_STYLE[st] || 'bg-neo-surface-high'}`}>{st}</span>
                </div>
                <span className="text-[10px] text-neo-text-muted font-mono">
                  {(p.attrs?.changes?.length ?? 0)} change(s)
                  {Array.isArray(p.attrs?.reviewers) && p.attrs.reviewers.length > 0 && <> · {p.attrs.reviewers.length} reviewer(s)</>}
                </span>
              </button>
            );
          })}
        </div>

        {/* Detail + diff */}
        <div className="flex flex-col gap-4 min-w-0">
          {!selected && <p className="text-xs text-neo-text-muted">Select a proposal to see its diff.</p>}

          {selected && (
            <>
              <div className="flex items-center justify-between gap-3 flex-wrap">
                <div className="flex items-center gap-2">
                  <h3 className="neo-title-md">{diff?.title || selectedProposal?.attrs?.title || selected}</h3>
                  <span className={`neo-tag text-[10px] font-mono ${STATUS_STYLE[selectedStatus] || 'bg-neo-surface-high'}`}>{selectedStatus}</span>
                  {diff?.needs_rebase && (
                    <span className="neo-tag bg-neo-red text-white text-[10px] font-mono flex items-center gap-1">
                      <AlertTriangle size={11} /> needs rebase
                    </span>
                  )}
                </div>
                {canReview && isOpen && (
                  <div className="flex items-center gap-2">
                    <button
                      onClick={() => merge(selected)}
                      disabled={busy || diff?.needs_rebase}
                      title={diff?.needs_rebase ? 'A target changed since drafting - rebase required' : 'Merge this proposal'}
                      className="neo-btn bg-neo-mint text-neo-text py-1.5 px-3 text-xs font-bold flex items-center gap-1 disabled:opacity-50"
                    >
                      <Check size={13} /> Merge
                    </button>
                    <button
                      onClick={() => reject(selected)}
                      disabled={busy}
                      className="neo-btn bg-neo-red text-white py-1.5 px-3 text-xs font-bold flex items-center gap-1 disabled:opacity-50"
                    >
                      <X size={13} /> Reject
                    </button>
                  </div>
                )}
              </div>

              {/* Per-entity before/after diff */}
              {!diff && <p className="text-xs text-neo-text-muted">Loading diff…</p>}
              {diff && diff.entities?.length === 0 && <p className="text-xs text-neo-text-muted">No changes in this proposal.</p>}
              {diff?.entities?.map((ent) => (
                <div key={ent.entity_id} className="neo-border bg-neo-surface p-3 flex flex-col gap-2">
                  <div className="flex items-center gap-2">
                    <span className="neo-chip py-0.5 text-[10px] font-mono">{ent.entity_id}</span>
                    {!ent.exists && <span className="neo-tag bg-neo-red text-white text-[9px]">deleted</span>}
                    {ent.conflict && <span className="neo-tag bg-neo-red text-white text-[9px] flex items-center gap-1"><AlertTriangle size={10} /> conflict</span>}
                  </div>
                  <div className="flex flex-col gap-1">
                    {ent.attrs?.map((a) => (
                      <div key={a.attr} className="grid grid-cols-[8rem_1fr] gap-2 items-start text-xs">
                        <span className="font-mono text-neo-text-muted truncate">{a.attr}</span>
                        <div className="flex items-center gap-2 flex-wrap min-w-0">
                          <code className="neo-border px-2 py-0.5 bg-neo-red/10 text-neo-red line-through break-all">{fmt(a.before)}</code>
                          <span className="text-neo-text-muted">→</span>
                          <code className="neo-border px-2 py-0.5 bg-neo-mint/20 text-neo-text break-all">{fmt(a.after)}</code>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ))}

              {/* Review comments = annotations on the proposal entity */}
              <div className="flex flex-col gap-2">
                <h4 className="neo-label-md text-neo-text-muted flex items-center gap-1"><MessageSquare size={14} /> Review comments</h4>
                {comments.length === 0 && <p className="text-[11px] text-neo-text-muted">No comments yet.</p>}
                {comments.map((c) => (
                  <div key={c.id} className="p-2 neo-border bg-neo-surface text-xs">
                    <div className="text-[10px] text-neo-text-muted font-mono">{c.created_by || 'user'}</div>
                    <div>{c.body}</div>
                  </div>
                ))}
                <form onSubmit={addComment} className="flex gap-2">
                  <input
                    aria-label="Add a review comment"
                    placeholder="Leave a review comment…"
                    value={commentBody}
                    onChange={(e) => setCommentBody(e.target.value)}
                    className="neo-input text-xs flex-1"
                  />
                  <button type="submit" disabled={busy} className="neo-btn bg-neo-surface-high text-neo-text py-1 px-3 text-xs font-bold disabled:opacity-50">
                    Comment
                  </button>
                </form>
              </div>

              <button
                onClick={() => openProposal(selected)}
                className="self-start neo-btn bg-neo-surface-high text-neo-text py-1 px-2 text-[10px] font-bold flex items-center gap-1"
              >
                <RefreshCw size={11} /> Refresh diff
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
