import React, { useState, useEffect } from 'react';
import { Link } from 'react-router-dom';
import {
  Database,
  Terminal,
  Cpu,
  Zap,
  Smartphone,
  Laptop,
  ArrowRight,
  Clock,
  CheckCircle,
  FileCode,
  ShieldCheck,
  Play,
  RefreshCw,
  AlertTriangle
} from 'lucide-react';
import { apiCall } from '../lib/api';
import { usePipelineRun } from '../lib/usePipelineRun';

export default function Dashboard() {
  const [activeTab, setActiveTab] = useState('architecture');
  const [metrics, setMetrics] = useState(null);
  const [metricsState, setMetricsState] = useState('loading'); // 'loading' | 'ready' | 'offline'

  useEffect(() => {
    let cancelled = false;
    apiCall('GET', '/api/metrics').then(({ ok, data, offline }) => {
      if (cancelled) return;
      if (ok && !offline && data) {
        setMetrics(data);
        setMetricsState('ready');
      } else {
        setMetricsState('offline');
      }
    });
    return () => { cancelled = true; };
  }, []);
  // Stage order/labels match the real `post-from-topic` pipeline registered
  // in `services/lifeos-pipelines/src/lib.rs::pipeline_registry` (issue #92):
  // research(memvec.recall) -> draft(copywriting) -> verify(eval-gate) ->
  // publish(social.draft, gated).
  const PIPELINE_STAGE_ORDER = ['research', 'draft', 'verify', 'publish'];
  const PIPELINE_STAGE_META = {
    research: { label: '1. memvec.recall', icon: Database },
    draft: { label: '2. copywriting', icon: Terminal },
    verify: { label: '3. eval-gate', icon: ShieldCheck },
    publish: { label: '4. social.draft (Gated)', icon: Zap },
  };
  // Read-only mirror of the real static registry in
  // services/lifeos-actions/src/lib.rs::action_registry() (issue #93). That
  // engine has no per-rule enable/disable and no listing API - rules are
  // compiled in, not stored - so there is nothing to fetch or toggle here.
  // A previous version of this panel had a toggle that flipped only local
  // component state and silently dropped the "change" on reload (finding
  // 25); since the backend has no concept of disabling a rule, the honest
  // fix is to drop the fake control rather than fake its persistence.
  // `wired: false` means the trigger event isn't emitted anywhere in the
  // codebase yet (see that file's doc comment), so the rule never fires.
  const ACTION_RULES = [
    { id: 'thumbnail_caption_draft', trigger: 'version.created', action: 'asset.thumbnail_caption_draft', wired: true },
    { id: 'equity_curve_journal', trigger: 'trade.closed', action: 'trading.equity_curve_and_journal', wired: false },
    { id: 'topic_quiz', trigger: 'topic.due', action: 'telegram.quiz', wired: false },
  ];

  // Real POST /api/pipeline/run (issue #92) + polling GET /api/event?run_id=
  // for the job's per-stage events, via the shared hook (issue #94, also
  // used by PipelineBuilder.jsx's Harness > Pipelines tab).
  const {
    logs: pipelineStageLogs,
    running: pipelineRunning,
    runState: pipelineRunState,
    trigger: triggerPipeline,
  } = usePipelineRun(PIPELINE_STAGE_ORDER);
  const pipelineLogs = pipelineStageLogs.map((log) => ({ ...log, ...PIPELINE_STAGE_META[log.stage] }));
  const runPipelineDemo = () => triggerPipeline('post-from-topic', {});

  // Live aggregates from GET /api/metrics; null fields render as '-' while
  // loading/offline rather than fabricating a count.
  const moduleCount = metrics ? Object.keys(metrics.entities_by_module || {}).length : null;
  const stats = [
    { name: 'Total Entities', value: metrics?.entities, change: `${metrics?.events ?? 0} events logged`, icon: Database, color: 'var(--neo-yellow)' },
    { name: 'Active Modules', value: moduleCount, change: 'By distinct entity module', icon: Cpu, color: 'var(--neo-mint)' },
    { name: 'Harness Runs Logged', value: metrics?.harness_runs, change: `Avg cost $${(metrics?.cost ?? 0).toFixed(2)}`, icon: Terminal, color: 'var(--neo-blue-bright)' },
    { name: 'Active Connections', value: metrics?.active_connections, change: `${metrics?.jobs_queued ?? 0} jobs queued`, icon: ShieldCheck, color: 'var(--neo-red)' },
  ];

  return (
    <div className="flex flex-col gap-8">
      {/* Welcome Hero */}
      <div className="neo-surface neo-border-thick neo-shadow p-6 relative overflow-hidden bg-neo-yellow">
        <div className="max-w-3xl flex flex-col items-start gap-4">
          <div>
            <h2 className="neo-title-lg text-neo-text mb-2">Life OS is Live</h2>
            <p className="neo-body-lg text-neo-text font-semibold">
              Your self-extending personal operating system. Managed by a generic entity-graph, backed by Turso, and controlled via Telegram & local Claude Code harness.
            </p>
          </div>
          <Link to="/docs" className="neo-btn py-2.5 px-4 bg-neo-surface text-neo-text text-xs font-mono font-bold flex items-center gap-1.5 neo-shadow">
            VIEW SYSTEM SPECIFICATIONS PORTAL →
          </Link>
        </div>
        <div className="absolute right-4 bottom-4 opacity-10 pointer-events-none hidden lg:block">
          <Terminal size={120} className="text-neo-text" />
        </div>
      </div>

      {/* Stats Grid */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-6">
        {stats.map((stat, i) => (
          <div key={i} className="neo-surface neo-border neo-shadow neo-shadow-hover p-5 flex flex-col justify-between">
            <div className="flex justify-between items-start">
              <div>
                <span className="neo-label-sm text-neo-text-muted block mb-1">{stat.name}</span>
                <span className="neo-title-md block">
                  {metricsState === 'loading' ? '...' : metricsState === 'offline' ? '-' : stat.value}
                </span>
              </div>
              <div
                className="w-10 h-10 neo-border flex items-center justify-center"
                style={{ backgroundColor: stat.color }}
              >
                <stat.icon size={20} className="text-neo-text" />
              </div>
            </div>
            <div className="mt-4 pt-3 border-t-2 border-neo-border border-dashed flex justify-between items-center">
              <span className="neo-label-sm text-neo-text-muted">{metricsState === 'offline' ? 'Backend unreachable' : stat.change}</span>
              {metricsState === 'offline' ? (
                <span className="neo-chip neo-chip--review py-0.5 text-[10px] flex items-center gap-1"><AlertTriangle size={10} /> OFFLINE</span>
              ) : (
                <span className="neo-chip neo-chip--completed py-0.5 text-[10px]">ACTIVE</span>
              )}
            </div>
          </div>
        ))}
      </div>

      {/* Main Grid: Systems Architecture & Harness logs */}
      <div className="grid grid-cols-1 xl:grid-cols-12 gap-8">
        
        {/* Systems Architecture Visualizer */}
        <div className="xl:col-span-8 neo-surface neo-border-thick neo-shadow p-6 flex flex-col gap-6">
          <div className="flex justify-between items-center border-b-4 border-neo-border pb-4">
            <h3 className="neo-title-md">Architectural Topology</h3>
            <div className="flex gap-2">
              <button 
                onClick={() => setActiveTab('architecture')}
                className={`neo-btn py-1 px-3 neo-label-sm ${activeTab === 'architecture' ? 'bg-neo-yellow' : ''}`}
              >
                Workflow
              </button>
              <button 
                onClick={() => setActiveTab('data')}
                className={`neo-btn py-1 px-3 neo-label-sm ${activeTab === 'data' ? 'bg-neo-yellow' : ''}`}
              >
                Data Sync
              </button>
            </div>
          </div>

          {activeTab === 'architecture' ? (
            <div className="flex flex-col gap-6">
              <p className="neo-body-md text-neo-text-muted">
                Three tiers coordinate seamlessly. Telegram acts as a 24/7 client while laptop is off. Heavy actions are enqueued in Turso and compiled locally by the Mac agent.
              </p>
              
              <div className="grid grid-cols-1 md:grid-cols-3 gap-4 text-center items-stretch mt-4">
                
                {/* Mobile/Cloud interface */}
                <div className="neo-surface neo-border p-4 flex flex-col gap-3 bg-neo-surface-muted relative">
                  <span className="neo-chip neo-chip--review absolute -top-3 left-4">TIER 1 (ALWAYS ON)</span>
                  <div className="flex justify-center mt-3 text-neo-blue"><Smartphone size={32} /></div>
                  <h4 className="neo-label-md font-bold">Cloud Surface</h4>
                  <p className="neo-label-sm text-left">
                    • grammY Telegram Bot<br/>
                    • Cloudflare Worker (Free)<br/>
                    • Powered by Claude Haiku<br/>
                    • Capture & low-latency query
                  </p>
                  <div className="mt-auto pt-2 border-t border-neo-border text-xs font-bold text-left text-neo-red">
                    * Outward actions are Gated
                  </div>
                </div>

                {/* Database Bridge */}
                <div className="neo-surface neo-border p-4 flex flex-col gap-3 bg-neo-surface relative justify-center">
                  <span className="neo-chip neo-chip--completed absolute -top-3 left-4">CANONICAL LAYER</span>
                  <div className="flex justify-center text-neo-mint"><Database size={32} /></div>
                  <h4 className="neo-label-md font-bold">Turso Database</h4>
                  <p className="neo-label-sm text-left">
                    • Multi-tenant SQLite store<br/>
                    • Schema-less Entities Graph<br/>
                    • Bidirectional Sync Replica<br/>
                    • Shared event & jobs logs
                  </p>
                </div>

                {/* Local Mac harness */}
                <div className="neo-surface neo-border p-4 flex flex-col gap-3 bg-neo-surface-high relative">
                  <span className="neo-chip neo-chip--active absolute -top-3 left-4">TIER 2 (LOCAL HEAVY)</span>
                  <div className="flex justify-center text-neo-text"><Laptop size={32} /></div>
                  <h4 className="neo-label-md font-bold">Mac Local Agent</h4>
                  <p className="neo-label-sm text-left">
                    • Claude Code Harness<br/>
                    • Self-Extension compiler<br/>
                    • 3-layer sandbox protection<br/>
                    • Local-first replica (Offline)
                  </p>
                </div>

              </div>

              {/* Data Flow arrow display */}
              <div className="neo-surface neo-border p-4 bg-neo-surface flex flex-col md:flex-row items-center justify-between gap-4">
                <div className="flex items-center gap-2">
                  <span className="neo-chip neo-chip--completed">Telegram Input</span>
                  <ArrowRight size={16} />
                  <span className="neo-chip neo-chip--review">Haiku enqueues job</span>
                  <ArrowRight size={16} />
                  <span className="neo-chip neo-chip--active">Mac pulls & compiles</span>
                </div>
                <div className="neo-tag bg-neo-yellow">Hot-reloaded live</div>
              </div>
            </div>
          ) : (
            <div className="flex flex-col gap-6">
              <p className="neo-body-md text-neo-text-muted">
                Sync is built on SQLite wire-compatible libSQL replication. Writes are committed locally on the Mac to protect privacy and integrity, then merged using event logs.
              </p>
              <div className="neo-border p-4 bg-neo-surface neo-radius flex flex-col gap-4">
                <div className="flex justify-between items-center">
                  <span className="neo-label-md">Sync Strategy: Last-Push-Wins Mitigation</span>
                  <span className="neo-chip neo-chip--completed">SECURE</span>
                </div>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div className="p-3 bg-neo-surface-muted neo-border">
                    <h5 className="neo-label-sm font-bold mb-1">1. Clean events log</h5>
                    <p className="text-xs">All user actions and runs append to the log. History is never overwritten, allowing easy state rebuilds on conflict.</p>
                  </div>
                  <div className="p-3 bg-neo-surface-muted neo-border">
                    <h5 className="neo-label-sm font-bold mb-1">2. Single-Writer lanes</h5>
                    <p className="text-xs">Mac and Cloud Bot mutate separate entities. Field-level locks inside the attrs JSON block concurrent conflicts.</p>
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Telegram Digest Simulator */}
        <div className="xl:col-span-4 flex flex-col gap-6">
          <div className="neo-surface neo-border-thick neo-shadow p-5 bg-zinc-950 text-white min-h-[380px] flex flex-col justify-between">
            <div className="flex justify-between items-center border-b border-zinc-800 pb-2 mb-3">
              <span className="neo-label-sm text-zinc-400">Telegram Bot Client</span>
              <div className="w-2 h-2 rounded-full bg-neo-mint animate-pulse" />
            </div>

            <div className="flex-1 flex flex-col gap-3 justify-center text-xs">
              <div className="bg-zinc-900 border border-zinc-800 p-3 neo-radius text-left">
                <span className="text-zinc-500 font-bold block mb-1">🤖 Life OS Daily Digest:</span>
                <p className="font-mono mt-1 text-[11px] leading-relaxed text-zinc-200">
                  ⚠️ 4 tasks overdue today.<br/>
                  📈 Portfolio: Closed AAPL option (PnL: +$140)<br/>
                  📥 Social: X post draft awaits approval.<br/>
                  🤖 Ask AI: Health tracker built successfully.
                </p>
              </div>
              <div className="flex gap-2">
                <Link
                  to="/modules"
                  className="flex-1 py-1.5 border border-zinc-700 bg-zinc-800 hover:bg-zinc-700 font-bold text-center text-[10px]"
                >
                  APPROVE DRAFTS
                </Link>
                <Link
                  to="/harness-loop"
                  className="flex-1 py-1.5 border border-zinc-700 bg-zinc-800 hover:bg-zinc-700 font-bold text-center text-[10px]"
                >
                  VIEW METRICS
                </Link>
              </div>
            </div>

            <div className="pt-2 border-t border-zinc-800 text-[10px] text-zinc-500 text-center font-mono">
              Always-on compute: Cloudflare Workers
            </div>
          </div>
        </div>

      </div>

      {/* Cross-Cutting Platform Workflows: Pipelines & Actions */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-8">
        
        {/* Agent DAG Pipeline Visualizer */}
        <div className="lg:col-span-7 neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
          <div className="flex justify-between items-center border-b-2 border-neo-border pb-3 mb-4">
            <h3 className="neo-title-md flex items-center gap-2">
              <Cpu size={18} />
              Agent Workflows (DAG Pipeline Engine)
            </h3>
            <button 
              onClick={runPipelineDemo}
              disabled={pipelineRunning}
              className="neo-btn py-1 px-3 bg-neo-yellow text-xs font-bold"
            >
              {pipelineRunning ? 'RUNNING...' : 'TRIGGER PIPELINE'}
            </button>
          </div>
          
          <p className="text-xs text-neo-text-muted mb-4">
            Runs the real <code>post-from-topic</code> pipeline via <code>POST /api/pipeline/run</code>, polling per-stage
            events from <code>lifeos-pipelines</code>. The publish stage is always gated - it drafts a <code>pending_approval</code>
            entity and halts rather than posting.
          </p>

          <div className="flex flex-col gap-3">
            {pipelineLogs.map((log, idx) => {
              const Icon = log.icon;
              return (
                <div key={idx} className="p-3 bg-neo-bg neo-border flex justify-between items-center text-xs">
                  <div className="flex items-center gap-2 font-semibold">
                    <Icon size={14} className="text-neo-blue" />
                    <span>{log.label}</span>
                  </div>
                  <span className={`text-[10px] px-2 py-0.5 border font-bold uppercase ${
                    log.status === 'done' ? 'bg-neo-mint' :
                    log.status === 'running' ? 'bg-neo-yellow animate-pulse' :
                    log.status === 'gated' ? 'bg-neo-red text-white' :
                    log.status === 'failed' ? 'bg-neo-red text-white' : 'bg-neo-surface text-neo-text-muted'
                  }`}>
                    {log.status}
                  </span>
                </div>
              );
            })}
          </div>

          {pipelineRunState && (
            <div className="mt-3 text-[10px] font-mono text-neo-text-muted uppercase">
              Run state: {pipelineRunState.replace('_', ' ')}
            </div>
          )}
        </div>

        {/* Life OS Actions Rules Engine */}
        <div className="lg:col-span-5 neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
          <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
            <Zap size={18} />
            Life OS Actions Rules
          </h3>
          <p className="text-xs text-neo-text-muted mb-4">
            Define automated rules that trigger actions based on event store outputs (GitHub Actions paradigm).
          </p>
          <p className="text-[10px] text-neo-text-muted italic -mt-2 mb-3">
            Read-only preview of the static registry in <code>services/lifeos-actions</code> - rules are
            compiled in, not stored, so there is no per-rule enable/disable here.
          </p>
          <div className="flex flex-col gap-3">
            {ACTION_RULES.map((rule) => (
              <div key={rule.id} className="p-3 bg-neo-surface-muted neo-border flex justify-between items-center text-xs">
                <div>
                  <span className="neo-label-sm text-[10px] text-neo-blue block mb-0.5">ON EVENT: {rule.trigger}</span>
                  <span className="font-bold">RUN: {rule.action}</span>
                </div>
                <span className={`neo-chip py-0.5 text-[9px] ${rule.wired ? 'neo-chip--completed' : 'neo-chip--review'}`}>
                  {rule.wired ? 'LIVE' : 'NOT YET WIRED'}
                </span>
              </div>
            ))}
          </div>
        </div>

      </div>

    </div>
  );
}
