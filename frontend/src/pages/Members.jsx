import React, { useCallback, useEffect, useState } from 'react';
import { Users, UserPlus, Trash2, ShieldCheck, Copy, Check, Mail } from 'lucide-react';
import { apiCall } from '../lib/api';

// Workspace members, roles, and invites (issue #146). Lists members with their
// roles, lets an owner change a role or remove a member, create an invite (the
// accept URL is shown exactly once, since only its hash is stored server-side),
// and revoke pending invites. Owner-only controls render only when the caller's
// own role (from GET /api/members) is owner - the server re-enforces regardless.

const ROLES = ['owner', 'editor', 'agent', 'viewer'];

const ROLE_HINT = {
  owner: 'Full control, including security config, connections, storage, and membership.',
  editor: 'Reads plus ordinary writes and gated drafting; not security-sensitive routes.',
  agent: 'Reversible, internal writes only - no gated/outward drafting.',
  viewer: 'Read-only.',
};

export default function Members() {
  const [members, setMembers] = useState([]);
  const [yourRole, setYourRole] = useState(null);
  const [invites, setInvites] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(null);

  // Invite form + the one-time accept URL surfaced after creation.
  const [inviteEmail, setInviteEmail] = useState('');
  const [inviteRole, setInviteRole] = useState('viewer');
  const [freshInvite, setFreshInvite] = useState(null); // { email, role, accept_url }
  const [copied, setCopied] = useState(false);

  const isOwner = yourRole === 'owner';

  const load = useCallback(async () => {
    setLoading(true);
    const res = await apiCall('GET', '/api/members');
    if (res.ok) {
      setMembers(Array.isArray(res.data?.members) ? res.data.members : []);
      setYourRole(res.data?.your_role ?? null);
      setError(null);
      // Pending invites are owner-only; ignore a 403 for non-owners.
      if (res.data?.your_role === 'owner') {
        const inv = await apiCall('GET', '/api/invites');
        setInvites(inv.ok && Array.isArray(inv.data?.invites) ? inv.data.invites : []);
      } else {
        setInvites([]);
      }
    } else {
      setError(res.error || 'Failed to load members');
    }
    setLoading(false);
  }, []);

  useEffect(() => { load(); }, [load]);

  const changeRole = async (userId, role) => {
    setBusy(userId);
    const res = await apiCall('POST', `/api/member/${userId}/role`, { role });
    if (!res.ok) setError(res.error || 'Failed to change role');
    else setError(null);
    setBusy(null);
    load();
  };

  const removeMember = async (userId) => {
    setBusy(userId);
    const res = await apiCall('DELETE', `/api/member/${userId}`);
    if (!res.ok) setError(res.error || 'Failed to remove member');
    else setError(null);
    setBusy(null);
    load();
  };

  const createInvite = async (e) => {
    e.preventDefault();
    const email = inviteEmail.trim();
    if (!email) return;
    setBusy('invite');
    const res = await apiCall('POST', '/api/invite', {
      email,
      role: inviteRole,
      base_url: window.location.origin,
    });
    if (res.ok) {
      setFreshInvite({ email: res.data.email, role: res.data.role, accept_url: res.data.accept_url });
      setInviteEmail('');
      setCopied(false);
      setError(null);
      load();
    } else {
      setError(res.error || 'Failed to create invite');
    }
    setBusy(null);
  };

  const revokeInvite = async (id) => {
    setBusy(id);
    const res = await apiCall('DELETE', `/api/invite/${id}`);
    if (!res.ok) setError(res.error || 'Failed to revoke invite');
    else setError(null);
    setBusy(null);
    load();
  };

  const copyUrl = async () => {
    if (!freshInvite?.accept_url) return;
    try {
      await navigator.clipboard.writeText(freshInvite.accept_url);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div className="flex flex-col gap-6 max-w-4xl">
      <div>
        <h2 className="neo-title-md flex items-center gap-2"><Users size={20} /> Members</h2>
        <p className="text-xs text-neo-text-muted mt-1">
          Everyone with access to this workspace and their role. Owners manage roles and invites;
          reads are free for every member. {yourRole && <>Your role: <strong className="font-mono">{yourRole}</strong>.</>}
        </p>
      </div>

      {error && <p className="text-xs text-neo-red font-bold">{error}</p>}
      {loading && <p className="text-xs text-neo-text-muted">Loading…</p>}

      {/* Members list */}
      {!loading && (
        <div className="flex flex-col gap-2">
          {members.length === 0 && (
            <p className="text-xs text-neo-text-muted">
              No explicit members yet - this workspace is in single-user mode (you are the owner).
            </p>
          )}
          {members.map((m) => (
            <div key={m.user_id} className="p-3 neo-border bg-neo-surface flex items-center justify-between gap-3">
              <div className="min-w-0">
                <div className="text-sm font-bold text-neo-text truncate">{m.name || m.email || m.user_id}</div>
                <div className="text-[10px] text-neo-text-muted font-mono truncate">{m.email || m.user_id}</div>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {isOwner ? (
                  <select
                    aria-label={`Role for ${m.email || m.user_id}`}
                    value={m.role}
                    disabled={busy === m.user_id}
                    onChange={(e) => changeRole(m.user_id, e.target.value)}
                    className="neo-input text-xs py-1 px-2"
                  >
                    {ROLES.map((r) => <option key={r} value={r}>{r}</option>)}
                  </select>
                ) : (
                  <span className="neo-tag bg-neo-surface-high text-neo-text text-[10px] font-mono">{m.role}</span>
                )}
                {isOwner && (
                  <button
                    onClick={() => removeMember(m.user_id)}
                    disabled={busy === m.user_id}
                    title="Remove member"
                    className="neo-btn bg-neo-red text-white py-1 px-2 text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
                  >
                    <Trash2 size={11} /> Remove
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Owner-only: invite + pending invites */}
      {isOwner && (
        <div className="neo-surface neo-border-thick neo-shadow p-5 flex flex-col gap-4">
          <h3 className="neo-title-md flex items-center gap-2"><UserPlus size={18} /> Invite a member</h3>
          <p className="text-[11px] text-neo-text-muted">
            The invite link is shown once and cannot be recovered - only its hash is stored. It expires in 7 days.
          </p>
          <form onSubmit={createInvite} className="flex flex-col sm:flex-row gap-2">
            <input
              type="email"
              required
              aria-label="Invite email"
              placeholder="teammate@example.com"
              value={inviteEmail}
              onChange={(e) => setInviteEmail(e.target.value)}
              className="neo-input text-sm flex-1"
            />
            <select
              aria-label="Invite role"
              value={inviteRole}
              onChange={(e) => setInviteRole(e.target.value)}
              className="neo-input text-sm sm:w-32"
            >
              {ROLES.filter((r) => r !== 'owner').map((r) => <option key={r} value={r}>{r}</option>)}
            </select>
            <button
              type="submit"
              disabled={busy === 'invite'}
              className="neo-btn bg-neo-mint text-neo-text py-2 px-4 flex items-center justify-center gap-2 disabled:opacity-50"
            >
              <Mail size={15} /> Send invite
            </button>
          </form>
          <p className="text-[10px] text-neo-text-muted">{ROLE_HINT[inviteRole]}</p>

          {freshInvite && (
            <div className="p-3 neo-border bg-neo-yellow/30 flex flex-col gap-2">
              <div className="text-xs font-bold flex items-center gap-1"><ShieldCheck size={13} /> Invite created for {freshInvite.email} ({freshInvite.role})</div>
              <div className="flex items-center gap-2">
                <code className="text-[10px] font-mono bg-neo-surface neo-border px-2 py-1 flex-1 min-w-0 truncate">{freshInvite.accept_url}</code>
                <button
                  onClick={copyUrl}
                  className="neo-btn bg-neo-surface-high text-neo-text py-1 px-2 text-[10px] font-bold flex items-center gap-1 shrink-0"
                >
                  {copied ? <><Check size={11} /> Copied</> : <><Copy size={11} /> Copy</>}
                </button>
              </div>
              <span className="text-[10px] text-neo-text-muted">Share this link with the invitee - it will not be shown again.</span>
            </div>
          )}

          {invites.length > 0 && (
            <div className="flex flex-col gap-2">
              <span className="neo-label-sm text-neo-text-muted">Pending invites ({invites.length})</span>
              {invites.map((inv) => (
                <div key={inv.id} className="p-2 neo-border bg-neo-surface flex items-center justify-between gap-2">
                  <div className="min-w-0">
                    <div className="text-xs font-mono truncate">{inv.email}</div>
                    <div className="text-[10px] text-neo-text-muted">role: {inv.role}</div>
                  </div>
                  <button
                    onClick={() => revokeInvite(inv.id)}
                    disabled={busy === inv.id}
                    className="neo-btn bg-neo-red text-white py-1 px-2 text-[10px] font-bold flex items-center gap-1 shrink-0 disabled:opacity-50"
                  >
                    <Trash2 size={11} /> Revoke
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
