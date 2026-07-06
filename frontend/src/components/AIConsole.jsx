import React, { useState, useEffect, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { Sparkles, X, ShieldAlert, Wand2, Lock, CornerDownLeft, Wrench, Clock } from 'lucide-react';
import MarkdownRenderer from './MarkdownRenderer';
import { routeIntent, canAI } from '../lib/capabilities';
import { apiCall, WORKSPACE_ID_KEY } from '../lib/api';
import { executeAction } from '../lib/agentActions';

// The app-wide AI surface. Mounted once in Layout; openable from anywhere via:
//   window.dispatchEvent(new CustomEvent('lifeos:ai', { detail: { prefill, layer } }))
// The primary path routes the request to the backend plan -> execute -> verify
// loop (POST /api/agent, docs/AGENT-CORE.md §14). That loop owns capability
// gating, memory, Tool-RAG and tracing; applied mutations land in the Agent
// Ledger (each undoable) and gated actions become pending_approval drafts.
// Only browser-bound local actions (navigation) stay client-side, since the
// server loop has no browser to drive.

const EXAMPLES = [
  'Find my overdue tasks and tag them urgent',
  'Add a "Spanish" knowledge domain with a starter roadmap',
  'Recommend 3 projects for my Trading domain',
  'Delete the version history', // demonstrates a gated refusal
];

// Browser-only navigation targets. The server agent loop cannot navigate the
// user's browser, so these resolve through the client-side action registry.
const LOCAL_NAV = [
  { key: 'dashboard', label: 'Dashboard', href: '/dashboard' },
  { key: 'knowledge', label: 'Knowledge', href: '/knowledge' },
  { key: 'modules', label: 'Modules', href: '/modules' },
  { key: 'database', label: 'Database', href: '/database' },
  { key: 'graph', label: 'Graph', href: '/graph' },
  { key: 'harness', label: 'Harness', href: '/harness' },
  { key: 'storage', label: 'Storage', href: '/storage' },
  { key: 'integrations', label: 'Integrations', href: '/integrations' },
  { key: 'docs', label: 'Docs', href: '/docs' },
  { key: 'profile', label: 'Profile', href: '/profile' },
  { key: 'ledger', label: 'Agent Ledger', href: '/agent-ledger' },
  { key: 'memory', label: 'Memory', href: '/memory' },
];

const NAV_INTENT = /^(?:go to|open|navigate to|take me to|show me)\s+(.+)/i;

// Recognizes an explicit "go to X" request and resolves it to a route, so a
// pure navigation never pays a network round trip to the server loop.
function matchLocalNav(text) {
  const m = NAV_INTENT.exec(text.trim());
  if (!m) return null;
  const target = m[1].toLowerCase().replace(/\b(page|the)\b/g, '').trim();
  return LOCAL_NAV.find((n) => target.includes(n.key) || target.includes(n.label.toLowerCase())) || null;
}

const isDelete = (t) => /\b(delete|remove|drop|wipe|erase|destroy)\b/i.test(t);

// A human-readable line for a turn that returned no final text of its own.
function outcomeText(result) {
  const o = result?.outcome;
  if (o === 'awaiting_approval') return 'Prepared a draft that needs your approval before it goes out.';
  if (o === 'kill_switch') return 'The agent is paused - the kill switch is on for this workspace.';
  if (o === 'step_budget_exhausted') return 'The agent hit its step budget and stopped before finishing.';
  if (result?.error) return `The agent stopped: ${result.error}.`;
  return 'Done.';
}

// Compact, collapsible list of the tools the loop actually ran this turn.
function StepList({ steps }) {
  return (
    <details className="mt-2">
      <summary className="text-[10px] font-bold uppercase text-neo-text-muted cursor-pointer flex items-center gap-1">
        <Wrench size={11} /> {steps.length} step{steps.length === 1 ? '' : 's'}
      </summary>
      <ul className="mt-1.5 flex flex-col gap-1">
        {steps.map((s, i) => (
          <li key={i} className="flex items-center gap-1.5 font-mono text-[10px] text-neo-text">
            <span className={s.ok ? 'text-emerald-600' : 'text-neo-red'}>{s.ok ? '✓' : '✗'}</span>
            {s.tool}
            <span className="text-neo-text-muted">{s.decision}</span>
          </li>
        ))}
      </ul>
    </details>
  );
}

// Surfaces gated actions the loop turned into pending_approval drafts.
function PendingNotice({ pending }) {
  return (
    <div className="mt-2 p-2 neo-border bg-neo-yellow/20 border-neo-yellow text-[11px] text-neo-text">
      <div className="flex items-center gap-1 font-bold mb-1"><Clock size={12} /> Awaiting approval</div>
      {pending.length} outward/irreversible action{pending.length === 1 ? '' : 's'} were drafted and need human approval before they run:
      <ul className="mt-1 font-mono text-[10px]">
        {pending.map((p, i) => <li key={i}>- {p.tool}</li>)}
      </ul>
    </div>
  );
}

export default function AIConsole() {
  const [open, setOpen] = useState(false);
  const [input, setInput] = useState('');
  const [log, setLog] = useState([]);
  const [busy, setBusy] = useState(false);
  const endRef = useRef(null);
  const inputRef = useRef(null);
  const launcherRef = useRef(null);
  const wasOpenRef = useRef(false);
  const navigate = useNavigate();

  const close = () => setOpen(false);

  useEffect(() => {
    const onOpen = (e) => {
      setOpen(true);
      if (e.detail?.prefill) setInput(e.detail.prefill);
    };
    window.addEventListener('lifeos:ai', onOpen);
    return () => window.removeEventListener('lifeos:ai', onOpen);
  }, []);

  // Focus the composer on open; Escape closes the panel from anywhere inside
  // it (finding 56 - command-palette-style surfaces need both). On close,
  // return focus to the launcher - it only re-enters the DOM once `open`
  // flips back to false, so that happens in its own effect below rather
  // than inside `close()` itself.
  useEffect(() => {
    if (!open) return undefined;
    inputRef.current?.focus?.();
    const onKeyDown = (e) => {
      if (e.key === 'Escape') close();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (wasOpenRef.current && !open) launcherRef.current?.focus?.();
    wasOpenRef.current = open;
  }, [open]);

  useEffect(() => { endRef.current?.scrollIntoView?.({ behavior: 'smooth' }); }, [log, busy]);

  const run = async () => {
    const text = input.trim();
    if (!text || busy) return;
    setInput('');
    setLog((l) => [...l, { role: 'user', text }]);

    // 1. Local-only action: browser navigation goes through the client-side
    // registry (executeAction) and never touches the server loop.
    const nav = matchLocalNav(text);
    if (nav) {
      await executeAction({ tool: 'navigate', args: { to: nav.href } }, {});
      setLog((l) => [...l, { role: 'system', text: `Navigating to **${nav.label}**.` }]);
      navigate(nav.href);
      close();
      return;
    }

    // 2. Fast client-side guardrail pre-filter (UX only - the /api/agent loop's
    // gate is the enforcement authority). Short-circuits an obviously-forbidden
    // ask before a 300s network round trip.
    const layers = routeIntent(text);
    const action = isDelete(text) ? 'delete' : 'modify';
    const blocked = layers
      .map((layer) => ({ layer, ...canAI(action, layer.id) }))
      .filter((v) => !v.allowed);
    if (blocked.length) {
      const reasons = blocked.map((b) => `- **${b.layer.label}** - ${b.reason}`).join('\n');
      setLog((l) => [...l, {
        role: 'ai',
        blocked: true,
        text: `I can't do that - it hits a guardrail:\n\n${reasons}\n\n_These are protected so the app can't be broken. You can make this change yourself._`,
      }]);
      return;
    }

    // 3. Primary path: the backend plan -> execute -> verify loop.
    setBusy(true);
    const workspaceId = localStorage.getItem(WORKSPACE_ID_KEY) || undefined;
    const { ok, data, error } = await apiCall('POST', '/api/agent', {
      prompt: text,
      ...(workspaceId ? { workspace_id: workspaceId } : {}),
    });
    setBusy(false);

    if (!ok) {
      const timedOut = /timed out/i.test(error || '');
      setLog((l) => [...l, {
        role: 'ai',
        error: true,
        text: `The agent couldn't run: ${error || 'unknown error'}.${timedOut ? ' It ran past the time limit - try a smaller, more specific ask.' : ''}`,
      }]);
      return;
    }

    setLog((l) => [...l, {
      role: 'ai',
      text: data?.text || outcomeText(data),
      steps: data?.ledger || [],
      pending: data?.pendingApprovals || [],
      outcome: data?.outcome,
    }]);
  };

  return (
    <>
      {/* Floating launcher - reachable from every page */}
      {!open && (
        <button
          ref={launcherRef}
          onClick={() => setOpen(true)}
          className="fixed bottom-6 right-6 z-[120] neo-btn bg-neo-blue text-white py-3 px-4 flex items-center gap-2 neo-shadow-lg"
          title="Ask AI to change anything"
          aria-label="Open AI console"
        >
          <Sparkles size={18} /> <span className="hidden sm:inline font-bold">AI Console</span>
        </button>
      )}

      {open && (
        <aside className="fixed right-0 top-0 bottom-0 w-full sm:w-[420px] bg-[var(--neo-surface)] border-l-4 border-neo-border neo-shadow-xl z-[130] flex flex-col">
          <div className="p-4 border-b-4 border-neo-border flex justify-between items-center bg-neo-blue text-white">
            <h3 className="neo-title-md text-base flex items-center gap-2"><Wand2 size={18} /> AI Console</h3>
            <button onClick={close} className="neo-icon-btn p-1.5 text-neo-text" aria-label="Close AI console"><X size={16} /></button>
          </div>

          <div className="px-4 py-2 border-b-2 border-neo-border bg-neo-surface-muted text-[11px] text-neo-text-muted flex items-center gap-1.5">
            <Lock size={11} /> The agent plans, executes and verifies. Applied changes land in the Agent Ledger (each undoable); outward actions wait for your approval.
          </div>

          <div className="flex-1 overflow-y-auto p-4 flex flex-col gap-3">
            {log.length === 0 && (
              <div className="flex flex-col gap-2">
                <p className="text-xs text-neo-text-muted">Ask me to change anything in the app. Examples:</p>
                {EXAMPLES.map((ex) => (
                  <button key={ex} onClick={() => setInput(ex)} className="neo-btn bg-neo-surface text-neo-text py-1.5 px-2 text-[11px] text-left">{ex}</button>
                ))}
              </div>
            )}
            {log.map((m, i) => (
              <div
                key={i}
                className={`p-2.5 text-xs neo-border ${
                  m.role === 'user' ? 'bg-neo-blue text-white self-end max-w-[85%]'
                  : m.role === 'system' ? 'bg-neo-mint text-neo-text'
                  : m.blocked ? 'bg-neo-red/10 border-neo-red text-neo-text'
                  : m.error ? 'bg-neo-red/10 border-neo-red text-neo-text'
                  : 'bg-neo-surface-muted text-neo-text'
                }`}
              >
                {m.blocked && <div className="flex items-center gap-1 font-bold text-neo-red mb-1"><ShieldAlert size={13} /> Guardrail</div>}
                <MarkdownRenderer content={m.text} className={m.role === 'user' ? 'text-white' : ''} />
                {m.role === 'ai' && !m.blocked && !m.error && m.steps?.length > 0 && <StepList steps={m.steps} />}
                {m.role === 'ai' && m.pending?.length > 0 && <PendingNotice pending={m.pending} />}
              </div>
            ))}
            {busy && <div className="p-2.5 text-xs neo-border bg-neo-surface-muted text-neo-text animate-pulse">Working through it - planning, executing and verifying…</div>}
            <div ref={endRef} />
          </div>

          <div className="p-3 border-t-2 border-neo-border flex flex-col gap-2">
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); run(); } }}
              placeholder="Change anything… (Enter to send)"
              aria-label="Message to AI console"
              className="neo-input text-sm min-h-[60px] w-full"
            />
            <button onClick={run} disabled={busy} className="neo-btn bg-neo-blue text-white py-2 text-xs font-bold flex items-center justify-center gap-2 disabled:opacity-50">
              <CornerDownLeft size={14} /> Send to AI
            </button>
          </div>
        </aside>
      )}
    </>
  );
}
