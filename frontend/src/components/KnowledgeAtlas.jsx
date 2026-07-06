import React, { useState, useEffect, useRef, useMemo } from 'react';
import MarkdownRenderer from './MarkdownRenderer';
import DomainWorkspace from './DomainWorkspace';
import {
  BookOpen, Search, Terminal, Cpu, MessageSquare, Plus, Compass, X, Sparkles, GitBranch, Pencil, ChevronRight, ChevronLeft, Link as LinkIcon,
  Download, Upload, Trash2, Check, ArrowRight, Maximize2, ChevronDown, ChevronUp, Loader2
} from 'lucide-react';
import { getCustomDomains, addCustomDomain, removeCustomDomain } from '../lib/atlasStore';
import { scaffoldDomain, llmSelection } from '../lib/ai';
import { apiCall } from '../lib/api';
import {
  listAnnotations,
  createAnnotation,
  updateAnnotation,
  deleteAnnotation,
  migrateLegacyAnnotations,
} from '../lib/annotationsApi';

// Palette + icons used to backfill metadata for domain entries that were
// seeded with only { id, topics[] } (the JSON has ~132 such stubs). Without
// this, their cards render "undefined" and the topics inside become unreachable.
const FALLBACK_COLORS = ['#2f29e8', '#ff4b4b', '#00b894', '#e17055', '#6c5ce7', '#0984e3', '#d63031', '#00cec9'];
const FALLBACK_ICONS = ['◆', '▲', '●', '■', '✦', '❖', '⬢', '✧'];

const humanizeId = (id) =>
  String(id || 'untitled')
    .replace(/[-_]+/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());

const normalizeDomain = (d, i) => ({
  ...d,
  num: d.num || String(i + 1).padStart(2, '0'),
  title: d.title || humanizeId(d.id),
  icon: d.icon || FALLBACK_ICONS[i % FALLBACK_ICONS.length],
  color: d.color || FALLBACK_COLORS[i % FALLBACK_COLORS.length],
  tagline: d.tagline || `${(d.topics || []).length} topics in ${humanizeId(d.id)}`,
  overview: d.overview || `Knowledge domain covering ${humanizeId(d.id)}. ${(d.topics || []).length} topics authored.`,
});

// The shipped atlas_data.json exploded every domain's topics into duplicate
// single-topic pseudo-domains that share the same `id` (e.g. 9 "os" entries,
// 23 "aiml"). Collapse them back into one domain per id: keep the richest
// metadata and union the topics (dedup by title/id). 136 entries -> 13 domains.
const mergeDomainsById = (list) => {
  const byId = new Map();
  for (const d of list) {
    const existing = byId.get(d.id);
    if (!existing) {
      byId.set(d.id, { ...d, topics: [...(d.topics || [])] });
      continue;
    }
    const seen = new Set(existing.topics.map((t) => t.id || t.title));
    for (const t of d.topics || []) {
      const key = t.id || t.title;
      if (!seen.has(key)) { existing.topics.push(t); seen.add(key); }
    }
    for (const field of ['title', 'overview', 'tagline', 'icon', 'color', 'num']) {
      if (!existing[field] && d[field]) existing[field] = d[field];
    }
  }
  return [...byId.values()];
};

const PROG_KEY = "KA_PROGRESS_V1";
const CONN_KEY = "KA_USERCONN_V1";

export default function KnowledgeAtlas() {
  const [view, setView] = useState('home'); // 'home', 'domain', 'topic'
  const [activeDomain, setActiveDomain] = useState(null);
  const [activeTopic, setActiveTopic] = useState(null);
  const [searchQuery, setSearchQuery] = useState('');

  const [annotations, setAnnotations] = useState([]);
  const [progress, setProgress] = useState({});
  const [userConns, setUserConns] = useState([]);

  // Full-screen subtopic viewer
  const [activeSubtopic, setActiveSubtopic] = useState(null);
  const [activeSubtopicIdx, setActiveSubtopicIdx] = useState(0);

  // Annotation Modal
  const [showAnnotationModal, setShowAnnotationModal] = useState(false);
  const [annotationDraft, setAnnotationDraft] = useState({ id: null, text: '', type: 'comment', linkTo: '', linkUrl: '', linkNote: '' });
  const [pendingAnchor, setPendingAnchor] = useState(null);

  // Tooltip (fixed positioning)
  const [tooltipPos, setTooltipPos] = useState(null);
  const [pencilPos, setPencilPos] = useState(null);
  const [hoveredResource, setHoveredResource] = useState(null);

  // Notes Panel
  const [showNotesPanel, setShowNotesPanel] = useState(false);
  const [notesFilter, setNotesFilter] = useState(null);

  // AI panel
  const [aiMode, setAiMode] = useState(null);
  const [aiLogs, setAiLogs] = useState([]);
  const [aiInput, setAiInput] = useState('');
  const [aiLoading, setAiLoading] = useState(false);
  const [aiSelection, setAiSelection] = useState([]);

  const contentRef = useRef(null);

  // The shipped atlas is a 2.4MB JSON file - loaded lazily (code-split out of
  // the main bundle) rather than statically imported, so it only downloads
  // when this view actually mounts.
  const [baseDomains, setBaseDomains] = useState([]);
  const [atlasLoading, setAtlasLoading] = useState(true);
  useEffect(() => {
    let cancelled = false;
    import('../atlas_data.json').then((mod) => {
      if (cancelled) return;
      setBaseDomains(mergeDomainsById(mod.default).map(normalizeDomain));
      setAtlasLoading(false);
    });
    return () => { cancelled = true; };
  }, []);

  // Custom (user/AI-scaffolded) domains merged on top of the shipped atlas.
  const [customRev, setCustomRev] = useState(0);
  const DOMAINS = useMemo(
    () => [...baseDomains, ...getCustomDomains().map((d, i) => normalizeDomain(d, baseDomains.length + i))],
    [customRev, baseDomains]
  );
  const customIds = useMemo(() => new Set(getCustomDomains().map((d) => d.id)), [customRev]);

  // Add Domain flow
  const [showAddDomain, setShowAddDomain] = useState(false);
  const [newDomainName, setNewDomainName] = useState('');
  const [newDomainIntent, setNewDomainIntent] = useState('');
  const [scaffolding, setScaffolding] = useState(false);

  const handleAddDomain = async () => {
    if (!newDomainName.trim()) return;
    setScaffolding(true);
    const scaffolded = await scaffoldDomain(newDomainName.trim(), newDomainIntent.trim());
    addCustomDomain(scaffolded);
    setScaffolding(false);
    setShowAddDomain(false);
    setNewDomainName('');
    setNewDomainIntent('');
    setCustomRev((r) => r + 1);
    const normalized = normalizeDomain(scaffolded, DOMAINS.length);
    setActiveDomain(normalized);
    setView('domain');
  };

  const deleteCustomDomain = (id) => {
    if (!window.confirm('Delete this custom domain? (Shipped domains cannot be deleted.)')) return;
    removeCustomDomain(id);
    setCustomRev((r) => r + 1);
    setView('home');
  };

  useEffect(() => {
    // Annotations now live in the workspace-scoped API. On first load we do a
    // one-time, best-effort import of any legacy localStorage notes, then read
    // the canonical set back from the API. Progress/connections stay local.
    (async () => {
      try {
        await migrateLegacyAnnotations();
        setAnnotations(await listAnnotations());
      } catch (e) {}
      try {
        if (localStorage.getItem(PROG_KEY)) setProgress(JSON.parse(localStorage.getItem(PROG_KEY)));
        if (localStorage.getItem(CONN_KEY)) setUserConns(JSON.parse(localStorage.getItem(CONN_KEY)));
      } catch (e) {}
    })();
  }, []);

  // Escape key closes subtopic viewer
  useEffect(() => {
    if (!activeSubtopic) return;
    const onKey = (e) => { if (e.key === 'Escape') setActiveSubtopic(null); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [activeSubtopic]);

  // Delete one annotation via the API, then drop it from local state.
  const removeAnnotation = async (id) => {
    if (await deleteAnnotation(id)) {
      setAnnotations((prev) => prev.filter((x) => x.id !== id));
    }
  };

  // Clear every annotation (each is its own DELETE - there is no bulk route).
  const clearAllAnnotations = async () => {
    const ids = annotations.map((a) => a.id);
    for (const id of ids) await deleteAnnotation(id);
    setAnnotations([]);
  };

  // Use fixed positioning for tooltip - getBoundingClientRect() gives viewport coords directly
  const handleSelection = () => {
    // Allow selecting/annotating inside the full-screen subtopic reader too;
    // the tooltip (z-200) and modal (z-160) both layer above the viewer (z-150).
    if (showAnnotationModal) return;
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) {
      if (tooltipPos) setTooltipPos(null);
      return;
    }
    const range = sel.getRangeAt(0);
    const text = sel.toString().trim();
    if (text.length < 2) return;
    const rect = range.getBoundingClientRect();
    setTooltipPos({
      top: rect.top - 48,
      left: rect.left + rect.width / 2
    });
    setPendingAnchor({ type: 'selection', quote: text, topicId: activeTopic?.id });
  };

  const handleResourceHover = (e, item, type) => {
    const rect = e.currentTarget.getBoundingClientRect();
    setPencilPos({ top: rect.top + 4, left: rect.right - 28 });
    setHoveredResource({ type, topicId: activeTopic?.id, quote: item.label || item.to });
  };

  const openAnnotationComposer = (type, existing = null) => {
    if (existing) {
      setAnnotationDraft({
        id: existing.id,
        text: existing.text || '',
        type: existing.kind,
        linkTo: existing.link?.to || '',
        linkUrl: existing.link?.url || '',
        linkNote: existing.link?.note || ''
      });
      setPendingAnchor({ type: existing.anchorType, quote: existing.quote, topicId: existing.topicId });
    } else {
      setAnnotationDraft({ id: null, text: '', type, linkTo: '', linkUrl: '', linkNote: '' });
      if (hoveredResource && !tooltipPos) setPendingAnchor(hoveredResource);
    }
    setShowAnnotationModal(true);
    setTooltipPos(null);
    setPencilPos(null);
  };

  const saveAnnotation = async () => {
    if (annotationDraft.type !== 'link' && !annotationDraft.text.trim()) return;
    const draft = {
      kind: annotationDraft.type,
      topicId: pendingAnchor?.topicId || activeTopic?.id,
      anchorType: pendingAnchor?.type || 'selection',
      quote: pendingAnchor?.quote,
      text: annotationDraft.text,
      link: annotationDraft.type === 'link' ? { to: annotationDraft.linkTo, url: annotationDraft.linkUrl, note: annotationDraft.linkNote } : null,
    };

    let saved;
    if (annotationDraft.id) {
      // Edit: the PATCH replaces attrs wholesale, so carry the existing
      // answer/createdAt forward rather than blanking them.
      const existing = annotations.find(a => a.id === annotationDraft.id) || {};
      saved = await updateAnnotation(annotationDraft.id, {
        ...draft, createdAt: existing.createdAt, answer: existing.answer, answeredAt: existing.answeredAt,
      });
      if (saved) setAnnotations(prev => prev.map(a => a.id === saved.id ? saved : a));
    } else {
      saved = await createAnnotation({ ...draft, createdAt: new Date().toISOString(), answer: '', answeredAt: null });
      if (saved) setAnnotations(prev => [...prev, saved]);
    }
    setShowAnnotationModal(false);
    window.getSelection()?.removeAllRanges();
    if (!saved) return;

    if (saved.kind === 'question' && !saved.answer) {
      // Route through apiCall so the request carries API_BASE + tenant/auth
      // headers (X-Workspace-Id, Authorization). A raw fetch here would be
      // untenanted and pinned to localhost, breaking non-local deployments.
      const { ok, data } = await apiCall('POST', '/api/llm', {
        system: "You are a helpful study assistant. Answer the user's question based on the provided context.",
        prompt: `Context Quote: "${saved.quote}"\nQuestion: ${saved.text}`,
        ...llmSelection(),
      });
      const answer = ok && data && data.text
        ? data.text
        : "Mock response - Rust backend `/api/llm` not connected yet.";
      const answered = await updateAnnotation(saved.id, { ...saved, answer, answeredAt: new Date().toISOString() });
      if (answered) setAnnotations(prev => prev.map(a => a.id === answered.id ? answered : a));
    }
  };

  const exportNotes = () => {
    const blob = new Blob([JSON.stringify(annotations, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url; a.download = "annotations.json"; a.click();
    URL.revokeObjectURL(url);
  };

  const callLLM = async (system, prompt) => {
    // Route through apiCall for API_BASE + tenant/auth headers (see above).
    const { ok, data } = await apiCall('POST', '/api/llm', { system, prompt, ...llmSelection() });
    if (ok && data && data.text) return data.text;
    {
      return new Promise(resolve => {
        setTimeout(() => {
          if (system.includes("Socratic")) resolve(`How does this specifically relate to the core model of **${activeTopic?.title}**? Give a concrete example.`);
          else if (system.includes("Teach")) resolve(`**Grade: 88/100**\n\nYou correctly identified the main structure but missed edge cases.\n\n- Good summary of core concepts.\n- Missing clarification on edge constraints.`);
          else if (system.includes("analyse a learner")) resolve(`**Knowledge Frontier:**\n1. **Advanced Vector Databases** - High ROI\n2. **Memory Alignment** - Contradiction found in notes\n\n*Suggestion:* Review memory bounds section.`);
          else if (system.includes("NON-OBVIOUS")) resolve(`**Discovered Links:**\n- *${activeTopic?.title}* connects to *Memory Constraints* in Systems Programming via optimal bounds checking.`);
          else if (system.includes("Synthesis")) resolve(`**Synthesis Essay**\n\nThese topics are fundamentally connected through resource optimization. By comparing them side by side...`);
          else resolve("Mock AI Response.");
        }, 1200);
      });
    }
  };

  const handleAiSubmit = async () => {
    if (!aiInput.trim()) return;
    const userInput = aiInput;
    setAiInput('');
    setAiLogs(prev => [...prev, { role: 'user', text: userInput }]);
    setAiLoading(true);
    let sys = "", prompt = "";
    if (aiMode === 'examiner') {
      sys = "You are a rigorous Socratic examiner. Ask exactly ONE probing question at a time.";
      prompt = `Topic: ${activeTopic?.title}\nStudent says: ${userInput}`;
    } else if (aiMode === 'teach') {
      sys = "Compare the student's explanation against the reference. Reply with a score out of 100, then specific misconceptions as bullets, then a 2-line corrected summary.";
      prompt = `Reference: ${JSON.stringify(activeTopic?.body)}\nStudent explanation: ${userInput}`;
    }
    const reply = await callLLM(sys, prompt);
    setAiLogs(prev => [...prev, { role: 'ai', text: reply }]);
    setAiLoading(false);
  };

  const startAiSession = async (mode) => {
    setAiMode(mode);
    setAiLogs([]);
    if (mode === 'examiner') {
      setAiLogs([{ role: 'ai', text: `Let's begin the Socratic viva on **${activeTopic?.title}**. Explain the core premise in your own words.` }]);
    } else if (mode === 'teach') {
      setAiLogs([{ role: 'ai', text: `Teach **${activeTopic?.title}** back to me. I will grade your understanding.` }]);
    } else if (mode === 'gap') {
      setAiLoading(true);
      const reply = await callLLM("You analyse a learner's knowledge graph. Find gaps.", `Topic: ${activeTopic?.title}`);
      setAiLogs([{ role: 'ai', text: reply }]);
      setAiLoading(false);
    } else if (mode === 'discover') {
      setAiLoading(true);
      const reply = await callLLM("Find NON-OBVIOUS conceptual links between technical topics.", `Topic: ${activeTopic?.title}`);
      setAiLogs([{ role: 'ai', text: reply }]);
      setAiLoading(false);
    } else if (mode === 'synth') {
      setAiLogs([{ role: 'ai', text: `Select 2-4 topics below to synthesize.` }]);
    }
  };

  const runSynth = async () => {
    if (aiSelection.length < 2) return;
    setAiLoading(true);
    const reply = await callLLM(
      "Write a tight cross-domain synthesis essay (4-6 paragraphs) connecting the given topics. Name each topic explicitly.",
      `Synthesize: ${aiSelection.join(', ')}`
    );
    setAiLogs(prev => [...prev, { role: 'ai', text: reply }]);
    setAiLoading(false);
  };

  // Navigate between subtopics in full-screen mode
  const openSubtopic = (idx) => {
    const sts = activeTopic?.subtopics;
    if (!sts || idx < 0 || idx >= sts.length) return;
    setActiveSubtopicIdx(idx);
    setActiveSubtopic(sts[idx]);
  };

  const allTopics = DOMAINS.flatMap(d => (d.topics || []).map(t => ({ ...t, domainTitle: d.title })));

  // Search results
  const searchResults = searchQuery.length > 1
    ? allTopics.filter(t =>
        t.title?.toLowerCase().includes(searchQuery.toLowerCase()) ||
        (t.subtopics || []).some(s => s.title?.toLowerCase().includes(searchQuery.toLowerCase()))
      ).slice(0, 12)
    : [];

  return (
    <div className="flex flex-col gap-6" onMouseUp={handleSelection}>

      {/* Add Domain modal - any domain is addable, AI scaffolds the skeleton */}
      {showAddDomain && (
        <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/50 p-4" onClick={(e) => e.target === e.currentTarget && setShowAddDomain(false)}>
          <div className="neo-surface neo-border-thick neo-shadow-lg p-5 w-full max-w-md bg-[var(--neo-surface)] relative">
            <button onClick={() => setShowAddDomain(false)} className="absolute right-4 top-4 neo-icon-btn p-1.5"><X size={16} /></button>
            <h3 className="neo-title-md mb-1 flex items-center gap-2"><Sparkles size={18} className="text-neo-blue" /> Add a Domain</h3>
            <p className="text-xs text-neo-text-muted mb-4">Name any subject. AI scaffolds an overview, starter topics and a roadmap - then it becomes a full workspace.</p>
            <input value={newDomainName} onChange={(e) => setNewDomainName(e.target.value)} placeholder="Domain name (e.g. Spanish, Quantum Computing)" className="neo-input text-sm w-full mb-2" autoFocus />
            <textarea value={newDomainIntent} onChange={(e) => setNewDomainIntent(e.target.value)} placeholder="What do you want to get out of it? (optional)" className="neo-input text-sm w-full min-h-[70px] mb-4" />
            <div className="flex gap-2">
              <button onClick={() => setShowAddDomain(false)} className="neo-btn bg-neo-surface text-neo-text py-2 px-4 flex-1 text-xs">Cancel</button>
              <button onClick={handleAddDomain} disabled={scaffolding || !newDomainName.trim()} className="neo-btn bg-neo-blue text-white py-2 px-4 flex-1 text-xs flex items-center justify-center gap-2 disabled:opacity-50">
                {scaffolding ? <><Loader2 size={14} className="animate-spin" /> Scaffolding…</> : <><Sparkles size={14} /> Scaffold with AI</>}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Fixed Tooltip for text selection */}
      {tooltipPos && (
        <div
          className="fixed z-[200] flex gap-1 p-1 bg-neo-surface neo-border neo-shadow"
          style={{ top: tooltipPos.top, left: tooltipPos.left, transform: 'translateX(-50%)' }}
        >
          <button
            onMouseDown={(e) => { e.preventDefault(); openAnnotationComposer('comment'); }}
            className="px-2 py-1 text-[10px] font-bold bg-neo-yellow text-black neo-border hover:opacity-90 flex items-center gap-1"
          >
            <MessageSquare size={10} /> Note
          </button>
          <button
            onMouseDown={(e) => { e.preventDefault(); openAnnotationComposer('question'); }}
            className="px-2 py-1 text-[10px] font-bold bg-neo-blue text-white neo-border hover:opacity-90 flex items-center gap-1"
          >
            <Sparkles size={10} /> Ask AI
          </button>
          <button
            onMouseDown={(e) => { e.preventDefault(); openAnnotationComposer('link'); }}
            className="px-2 py-1 text-[10px] font-bold bg-neo-mint text-black neo-border hover:opacity-90 flex items-center gap-1"
          >
            <LinkIcon size={10} /> Link
          </button>
          <button
            onMouseDown={(e) => { e.preventDefault(); setTooltipPos(null); }}
            className="px-1 py-1 text-[10px] text-neo-text-muted hover:text-neo-text"
          >
            <X size={10} />
          </button>
        </div>
      )}

      {/* Fixed Pencil hover button */}
      {pencilPos && !tooltipPos && (
        <button
          className="fixed z-[200] p-1 bg-neo-surface neo-border neo-shadow hover:bg-neo-yellow transition-colors"
          style={{ top: pencilPos.top, left: pencilPos.left }}
          onClick={() => openAnnotationComposer('comment')}
        >
          <Pencil size={12} className="text-neo-text" />
        </button>
      )}

      {/* Full-Screen Subtopic Viewer */}
      {activeSubtopic && activeTopic && (
        <div className="fixed inset-0 z-[150] bg-[var(--neo-bg)] flex flex-col">
          {/* Header bar */}
          <div className="shrink-0 neo-surface border-b-4 border-neo-border px-6 py-3 flex items-center justify-between">
            <div className="flex items-center gap-2 text-xs font-mono text-neo-text-muted truncate">
              <span className="hidden sm:inline">{activeDomain?.title}</span>
              <ChevronRight size={12} className="hidden sm:inline shrink-0" />
              <span className="text-neo-text font-bold truncate">{activeTopic?.title}</span>
              <ChevronRight size={12} className="shrink-0" />
              <span className="text-neo-blue font-bold truncate">{activeSubtopic.title}</span>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              {activeTopic.subtopics?.length > 1 && (
                <div className="flex items-center gap-1">
                  <button
                    onClick={() => openSubtopic(activeSubtopicIdx - 1)}
                    disabled={activeSubtopicIdx === 0}
                    className="neo-icon-btn p-1 disabled:opacity-30"
                  >
                    <ChevronLeft size={14} />
                  </button>
                  <span className="text-[10px] font-mono px-2 text-neo-text-muted">
                    {activeSubtopicIdx + 1} / {activeTopic.subtopics.length}
                  </span>
                  <button
                    onClick={() => openSubtopic(activeSubtopicIdx + 1)}
                    disabled={activeSubtopicIdx >= activeTopic.subtopics.length - 1}
                    className="neo-icon-btn p-1 disabled:opacity-30"
                  >
                    <ChevronRight size={14} />
                  </button>
                </div>
              )}
              <button onClick={() => setActiveSubtopic(null)} className="neo-icon-btn p-2 ml-2">
                <X size={16} />
              </button>
            </div>
          </div>

          {/* Subtopic index sidebar + content */}
          <div className="flex-1 overflow-hidden flex">
            {/* Mini subtopic index */}
            {activeTopic.subtopics?.length > 1 && (
              <div className="hidden lg:flex flex-col w-60 shrink-0 border-r-2 border-neo-border overflow-y-auto bg-[var(--neo-surface)]">
                <div className="px-3 py-2 border-b-2 border-neo-border neo-label-sm text-neo-text-muted text-[10px]">
                  SUBTOPICS ({activeTopic.subtopics.length})
                </div>
                {activeTopic.subtopics.map((st, i) => (
                  <button
                    key={i}
                    onClick={() => openSubtopic(i)}
                    className={`text-left px-3 py-2.5 border-b border-neo-border text-xs font-mono transition-colors ${
                      i === activeSubtopicIdx
                        ? 'bg-neo-blue text-white font-bold'
                        : 'text-neo-text hover:bg-[var(--neo-surface-muted)]'
                    }`}
                  >
                    <span className="text-[9px] block text-opacity-60 mb-0.5">
                      {(i + 1).toString().padStart(2, '0')}
                    </span>
                    {st.title}
                  </button>
                ))}
              </div>
            )}

            {/* Main reading content */}
            <div className="flex-1 overflow-y-auto">
              <article className="max-w-3xl mx-auto px-6 py-8 md:px-12 md:py-12">
                <h1 className="neo-title-md mb-2 text-neo-text">{activeSubtopic.title}</h1>
                <div className="flex items-center gap-2 mb-8 text-[10px] font-mono text-neo-text-muted">
                  <span>{activeTopic?.title}</span>
                  <span>·</span>
                  <span>Subtopic {activeSubtopicIdx + 1}</span>
                </div>

                <div className="text-[15px] leading-relaxed text-neo-text-muted">
                  <MarkdownRenderer content={activeSubtopic.body} />
                </div>

                {activeSubtopic.resources?.length > 0 && (
                  <div className="mt-10 pt-6 border-t-2 border-neo-border">
                    <h3 className="neo-label-md mb-4 text-neo-text">Resources</h3>
                    <ul className="flex flex-col gap-2">
                      {activeSubtopic.resources.map((r, i) => (
                        <li key={i} className="flex gap-2 items-center text-sm">
                          <span className="text-[10px] font-mono px-1.5 py-0.5 bg-neo-surface-high border border-neo-border text-neo-text shrink-0">{r.type || 'link'}</span>
                          <a href={r.url} target="_blank" rel="noopener noreferrer" className="text-neo-blue hover:underline font-bold">{r.label}</a>
                          {r.note && <span className="text-neo-text-muted italic text-[11px]">- {r.note}</span>}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}

                {/* Prev/next at bottom of content */}
                {activeTopic.subtopics?.length > 1 && (
                  <div className="mt-12 flex justify-between items-center pt-6 border-t-2 border-neo-border">
                    <button
                      onClick={() => openSubtopic(activeSubtopicIdx - 1)}
                      disabled={activeSubtopicIdx === 0}
                      className="neo-btn py-2 px-4 text-xs flex items-center gap-2 disabled:opacity-30"
                    >
                      <ChevronLeft size={14} />
                      {activeSubtopicIdx > 0 && activeTopic.subtopics[activeSubtopicIdx - 1]?.title}
                    </button>
                    <button
                      onClick={() => openSubtopic(activeSubtopicIdx + 1)}
                      disabled={activeSubtopicIdx >= activeTopic.subtopics.length - 1}
                      className="neo-btn py-2 px-4 text-xs flex items-center gap-2 disabled:opacity-30"
                    >
                      {activeSubtopicIdx < activeTopic.subtopics.length - 1 && activeTopic.subtopics[activeSubtopicIdx + 1]?.title}
                      <ChevronRight size={14} />
                    </button>
                  </div>
                )}
              </article>
            </div>
          </div>
        </div>
      )}

      {/* Notes Slide-over Panel */}
      {showNotesPanel && (
        <aside className="fixed right-0 top-0 bottom-0 w-80 bg-[var(--neo-surface)] border-l-4 border-neo-border neo-shadow-xl z-[100] flex flex-col">
          <div className="p-4 border-b-2 border-neo-border flex justify-between items-center bg-[var(--neo-surface-muted)]">
            <h3 className="neo-label-md text-sm">Notes & Questions</h3>
            <button onClick={() => setShowNotesPanel(false)} className="neo-icon-btn p-1"><X size={16} /></button>
          </div>
          <div className="p-2 border-b-2 border-neo-border flex flex-wrap gap-2">
            <button onClick={exportNotes} className="neo-btn bg-neo-surface py-1 px-2 text-[10px] flex items-center gap-1"><Download size={10} /> Export</button>
            <button onClick={() => { if (window.confirm("Clear all annotations?")) clearAllAnnotations(); }} className="neo-btn bg-neo-red text-white py-1 px-2 text-[10px] flex items-center gap-1"><Trash2 size={10} /> Clear</button>
            {notesFilter && <button onClick={() => setNotesFilter(null)} className="neo-btn bg-neo-yellow text-black py-1 px-2 text-[10px]">Show All</button>}
          </div>
          <div className="flex-1 overflow-y-auto p-4 flex flex-col gap-4 bg-[var(--neo-bg)]">
            {(notesFilter ? annotations.filter(a => a.topicId === notesFilter) : annotations).map(a => (
              <div key={a.id} className="p-3 border-2 border-neo-border bg-[var(--neo-surface)] flex flex-col gap-2">
                <div className="flex justify-between items-center">
                  <span className="font-bold text-xs">{a.kind === 'question' ? '❓ Question' : a.kind === 'link' ? '🔗 Link' : '💬 Note'}</span>
                  <div className="flex gap-1">
                    <button onClick={() => openAnnotationComposer(a.kind, a)} className="hover:text-neo-blue"><Pencil size={12} /></button>
                    <button onClick={() => removeAnnotation(a.id)} className="hover:text-neo-red"><Trash2 size={12} /></button>
                  </div>
                </div>
                {a.quote && <blockquote className="border-l-2 border-neo-blue pl-2 text-[10px] text-neo-text-muted italic">"{a.quote}"</blockquote>}
                {a.kind === 'link' ? (
                  <div className="text-xs">
                    {a.link?.to && <span className="font-mono text-neo-blue block">→ {a.link.to}</span>}
                    {a.link?.url && <a href={a.link.url} target="_blank" rel="noopener noreferrer" className="text-neo-blue hover:underline block">↗ {a.link.url}</a>}
                    {a.link?.note && <span className="text-neo-text mt-1 block">{a.link.note}</span>}
                  </div>
                ) : (
                  <p className="text-xs font-medium text-neo-text">{a.text}</p>
                )}
                {a.answer && (
                  <div className="mt-2 p-2 bg-[var(--neo-surface-muted)] text-[10px] border border-neo-border">
                    <span className="font-bold text-neo-blue block mb-1">Answer (AI):</span>
                    <MarkdownRenderer content={a.answer} />
                  </div>
                )}
              </div>
            ))}
            {annotations.length === 0 && <p className="text-xs text-neo-text-muted text-center py-8">No notes yet. Select text in a topic to annotate.</p>}
          </div>
        </aside>
      )}

      {/* Annotation Modal */}
      {showAnnotationModal && (
        <div
          className="fixed inset-0 z-[160] flex items-center justify-center bg-black/50 p-4"
          onClick={(e) => e.target === e.currentTarget && setShowAnnotationModal(false)}
        >
          <div className="neo-surface neo-border-thick neo-shadow-lg p-5 w-full max-w-md bg-[var(--neo-surface)] relative">
            <button onClick={() => setShowAnnotationModal(false)} className="absolute right-4 top-4 neo-icon-btn p-1.5"><X size={16} /></button>
            <h3 className="neo-title-md mb-2">
              {annotationDraft.id ? 'Edit Note' : annotationDraft.type === 'question' ? 'Ask AI' : annotationDraft.type === 'link' ? 'Add Link' : 'Add Note'}
            </h3>
            <div className="p-3 bg-[var(--neo-surface-muted)] neo-border mb-4 text-xs italic border-l-4 border-neo-blue text-neo-text-muted">
              "{pendingAnchor?.quote || 'No context selected'}"
            </div>
            <div className="flex border-2 border-neo-border mb-4 bg-[var(--neo-surface-high)]">
              {['comment', 'question', 'link'].map(t => (
                <button
                  key={t}
                  onClick={() => setAnnotationDraft({ ...annotationDraft, type: t })}
                  className={`flex-1 py-1 text-[10px] font-bold uppercase transition-colors ${annotationDraft.type === t ? 'bg-neo-yellow text-black' : 'text-neo-text-muted hover:text-neo-text'}`}
                >
                  {t}
                </button>
              ))}
            </div>
            {annotationDraft.type === 'link' ? (
              <div className="flex flex-col gap-2 mb-4">
                <input type="text" placeholder="Topic ID or Title" className="neo-input text-xs w-full" value={annotationDraft.linkTo} onChange={e => setAnnotationDraft({ ...annotationDraft, linkTo: e.target.value })} />
                <input type="text" placeholder="...or URL" className="neo-input text-xs w-full" value={annotationDraft.linkUrl} onChange={e => setAnnotationDraft({ ...annotationDraft, linkUrl: e.target.value })} />
                <textarea placeholder="Why this connects..." rows="2" className="neo-input text-xs w-full mt-2" value={annotationDraft.linkNote} onChange={e => setAnnotationDraft({ ...annotationDraft, linkNote: e.target.value })} />
              </div>
            ) : (
              <textarea
                className="neo-input w-full min-h-[100px] text-sm mb-4"
                placeholder={annotationDraft.type === 'question' ? "What do you want explained?" : "Your thoughts..."}
                value={annotationDraft.text}
                onChange={(e) => setAnnotationDraft({ ...annotationDraft, text: e.target.value })}
                autoFocus
              />
            )}
            <div className="flex gap-2">
              <button onClick={() => setShowAnnotationModal(false)} className="neo-btn bg-neo-surface py-2 px-4 flex-1 text-neo-text font-bold text-xs">Cancel</button>
              <button onClick={saveAnnotation} className="neo-btn bg-neo-mint py-2 px-4 flex-1 text-black font-bold text-xs">Save</button>
            </div>
          </div>
        </div>
      )}

      {/* ---- Header ---- */}
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-yellow text-black flex flex-wrap justify-between items-center gap-3">
        <div>
          <h2 className="neo-title-md mb-1 flex items-center gap-2">
            <Compass size={22} /> Knowledge Atlas
          </h2>
          <p className="font-semibold text-sm">One connected map of deep technical mastery.</p>
        </div>
        <div className="flex items-center gap-3 flex-wrap">
          <button
            onClick={() => { setNotesFilter(null); setShowNotesPanel(true); }}
            className="neo-btn bg-neo-surface py-1.5 px-3 text-xs font-bold flex items-center gap-2 text-neo-text"
          >
            <MessageSquare size={13} /> Notes ({annotations.length})
          </button>
          <div className="relative">
            <Search size={14} className="absolute left-2.5 top-2 text-neo-text-muted" />
            <input
              type="text"
              placeholder="Search topics..."
              className="w-52 pl-8 pr-3 py-1.5 border-2 border-neo-border font-mono text-xs bg-neo-surface text-neo-text placeholder:text-neo-text-muted"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
            />
            {searchResults.length > 0 && (
              <div className="absolute top-full left-0 right-0 z-50 bg-neo-surface text-neo-text border-2 border-neo-border shadow-lg max-h-60 overflow-y-auto">
                {searchResults.map((t, i) => (
                  <button
                    key={i}
                    onClick={() => {
                      const domain = DOMAINS.find(d => (d.topics || []).some(tp => tp.id === t.id));
                      if (domain) { setActiveDomain(domain); setActiveTopic(t); setView('topic'); setAiMode(null); }
                      setSearchQuery('');
                    }}
                    className="w-full text-left px-3 py-2 text-xs hover:bg-neo-surface-muted border-b border-neo-border flex flex-col"
                  >
                    <span className="font-bold text-neo-text">{t.title}</span>
                    <span className="text-neo-text-muted text-[10px]">{t.domainTitle}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* ---- Home View ---- */}
      {view === 'home' && (
        <div className="flex flex-col gap-4">
          <div className="w-full border-2 border-neo-border bg-[var(--neo-surface)] flex items-center justify-between gap-3 px-5 py-4 relative overflow-hidden flex-wrap">
            <GitBranch size={48} className="text-neo-text-muted opacity-10 absolute left-1/2 -translate-x-1/2" />
            <p className="text-xs font-mono font-bold z-10 text-neo-text">Knowledge Graph - {DOMAINS.length} Domains · {allTopics.length} Topics</p>
            <button
              onClick={() => setShowAddDomain(true)}
              className="neo-btn bg-neo-blue text-white py-1.5 px-3 text-xs flex items-center gap-1.5 z-10"
            >
              <Plus size={14} /> Add Domain
            </button>
          </div>
          {atlasLoading && (
            <div className="flex items-center justify-center gap-2 py-16 text-xs font-mono text-neo-text-muted">
              <Loader2 size={16} className="animate-spin" /> Loading knowledge atlas…
            </div>
          )}
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
            {DOMAINS.map(d => (
              <div
                key={d.id}
                onClick={() => { setActiveDomain(d); setView('domain'); }}
                className="neo-surface neo-border-thick neo-shadow neo-shadow-hover p-4 cursor-pointer flex flex-col gap-2 transition-all hover:bg-[var(--neo-surface-muted)]"
                style={{ borderLeftColor: d.color, borderLeftWidth: '6px' }}
              >
                <div className="flex justify-between items-start">
                  <span className="font-extrabold text-sm uppercase tracking-tight truncate pr-2 text-neo-text" style={{ fontFamily: 'Montserrat, sans-serif' }}>
                    {d.icon || '◆'} {d.title}
                  </span>
                  <div className="flex items-center gap-1 shrink-0">
                    {customIds.has(d.id) && (
                      <>
                        <span className="neo-tag bg-neo-mint text-neo-text text-[8px] px-1 py-0.5">custom</span>
                        <button
                          onClick={(e) => { e.stopPropagation(); deleteCustomDomain(d.id); }}
                          className="text-neo-text-muted hover:text-neo-red"
                          title="Delete custom domain"
                        >
                          <Trash2 size={12} />
                        </button>
                      </>
                    )}
                    <span className="font-mono text-xs font-bold px-1.5 py-0.5 bg-[var(--neo-surface-high)] border border-neo-border text-neo-text">{d.num}</span>
                  </div>
                </div>
                <p className="text-[11px] text-neo-text-muted leading-tight">{d.tagline}</p>
                <div className="text-[10px] font-mono text-neo-text-muted mt-auto">{(d.topics || []).length} topics</div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* ---- Domain View (full workspace) ---- */}
      {view === 'domain' && activeDomain && (
        <DomainWorkspace
          domain={activeDomain}
          annotations={annotations}
          progress={progress}
          onBack={() => setView('home')}
          onOpenTopic={(t) => { setActiveTopic(t); setView('topic'); setAiMode(null); }}
        />
      )}

      {/* ---- Topic View ---- */}
      {view === 'topic' && activeTopic && activeDomain && (
        <div className="flex flex-col lg:flex-row gap-6">
          <div className="lg:w-3/4 flex flex-col gap-5" onMouseLeave={() => setPencilPos(null)}>
            <button onClick={() => setView('domain')} className="neo-btn self-start py-1 px-3 text-xs flex items-center gap-1">
              <ChevronLeft size={14} /> {activeDomain.title}
            </button>

            <article className="p-6 neo-border-thick bg-[var(--neo-surface)] neo-shadow relative" ref={contentRef}>
              <div className="flex justify-between items-start mb-5 gap-3">
                <h1 className="neo-title-md text-neo-text">{activeTopic.title}</h1>
                <button
                  onClick={() => {
                    const states = ['new', 'learning', 'mastered'];
                    const cur = progress[activeTopic.id] || 'new';
                    const next = states[(states.indexOf(cur) + 1) % states.length];
                    const p = { ...progress, [activeTopic.id]: next };
                    setProgress(p);
                    localStorage.setItem(PROG_KEY, JSON.stringify(p));
                  }}
                  className={`neo-chip py-1 px-2 text-[10px] cursor-pointer hover:opacity-90 shrink-0 ${
                    progress[activeTopic.id] === 'mastered' ? 'bg-neo-mint text-black' :
                    progress[activeTopic.id] === 'learning' ? 'bg-neo-yellow text-black' :
                    'bg-[var(--neo-surface)] text-neo-text'
                  }`}
                >
                  {progress[activeTopic.id] === 'mastered' ? '● Mastered' :
                   progress[activeTopic.id] === 'learning' ? '◐ Learning' : '○ New'}
                </button>
              </div>

              <div className="text-[15px] leading-relaxed text-neo-text-muted">
                <MarkdownRenderer content={activeTopic.body} />
              </div>

              {/* Subtopics - clickable cards opening full-screen */}
              {(activeTopic.subtopics?.length > 0) && (
                <div className="mt-8 pt-6 border-t-2 border-neo-border border-dashed">
                  <div className="flex items-center justify-between mb-4">
                    <h3 className="neo-label-md text-neo-text">
                      Deep Dive Subtopics ({activeTopic.subtopics.length})
                    </h3>
                    <button
                      onClick={() => openSubtopic(0)}
                      className="neo-btn py-1 px-2 text-[10px] flex items-center gap-1 bg-neo-yellow"
                    >
                      <Maximize2 size={10} /> Read All
                    </button>
                  </div>
                  <div className="flex flex-col gap-2">
                    {activeTopic.subtopics.map((st, i) => (
                      <button
                        key={i}
                        onClick={() => openSubtopic(i)}
                        className="flex items-center gap-4 p-3 border-2 border-neo-border bg-[var(--neo-surface)] hover:bg-[var(--neo-surface-muted)] text-left transition-colors group w-full"
                      >
                        <span className="text-lg font-bold font-mono text-neo-text-muted shrink-0 w-6">{(i + 1).toString().padStart(2, '0')}</span>
                        <div className="flex flex-col gap-0.5 min-w-0 flex-1">
                          <h4 className="font-bold text-sm text-neo-text group-hover:text-neo-blue transition-colors">{st.title}</h4>
                          <p className="text-[11px] text-neo-text-muted truncate">
                            {Array.isArray(st.body) ? st.body[0]?.slice(0, 120) : String(st.body || '').slice(0, 120)}…
                          </p>
                        </div>
                        <Maximize2 size={14} className="text-neo-text-muted shrink-0 opacity-0 group-hover:opacity-100 transition-opacity" />
                      </button>
                    ))}
                  </div>
                </div>
              )}

              {/* Resources */}
              {activeTopic.resources?.length > 0 && (
                <div className="mt-8 pt-6 border-t-2 border-neo-border border-dashed">
                  <h3 className="neo-label-md mb-3 text-neo-text">Resources</h3>
                  <ul className="flex flex-col gap-2">
                    {activeTopic.resources.map((r, i) => (
                      <li key={i} className="text-sm flex gap-2 items-center w-max pr-4" onMouseEnter={(e) => handleResourceHover(e, r, 'resource')}>
                        <span className="text-[10px] font-mono px-1.5 py-0.5 bg-[var(--neo-surface-high)] border border-neo-border text-neo-text">{r.type || 'link'}</span>
                        <a href={r.url} target="_blank" rel="noopener noreferrer" className="text-neo-blue hover:underline font-bold">{r.label}</a>
                        {r.note && <span className="text-neo-text-muted italic text-[11px]">- {r.note}</span>}
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {/* Connections */}
              {((activeTopic.connections?.length > 0) || userConns.filter(c => c.from === activeTopic.id).length > 0) && (
                <div className="mt-8 pt-6 border-t-2 border-neo-border border-dashed">
                  <h3 className="neo-label-md mb-3 text-neo-text">Connects to</h3>
                  <div className="flex flex-col gap-2">
                    {activeTopic.connections?.map((c, i) => (
                      <div key={i} className="flex items-center gap-2 text-[11px] bg-[var(--neo-surface-muted)] p-2 neo-border w-max pr-4" onMouseEnter={(e) => handleResourceHover(e, c, 'connection')}>
                        <span className="font-bold text-neo-text">→ {c.to}</span>
                        <span className="text-neo-text-muted">{c.label} {c.note && `(${c.note})`}</span>
                      </div>
                    ))}
                    {userConns.filter(c => c.from === activeTopic.id).map((c, i) => (
                      <div key={`uc_${i}`} className="flex items-center gap-2 text-[11px] bg-neo-yellow/10 p-2 border-2 border-neo-yellow w-max pr-4" onMouseEnter={(e) => handleResourceHover(e, c, 'connection')}>
                        <span className="font-bold text-neo-yellow">→ {c.to} (Your link)</span>
                        <span className="text-neo-text-muted">{c.note && `(${c.note})`}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </article>
          </div>

          {/* AI Sidebar */}
          <div className="lg:w-1/4 flex flex-col gap-5">
            <div className="p-4 neo-border-thick bg-[var(--neo-surface)] flex flex-col gap-4">
              <div className="flex justify-between items-center border-b-2 border-neo-border pb-2">
                <h3 className="neo-label-md flex items-center gap-2 text-neo-blue"><Sparkles size={15} /> Study AI</h3>
              </div>
              {!aiMode ? (
                <div className="flex flex-col gap-1.5">
                  {[
                    { mode: 'examiner', label: '🎓 Examiner', desc: 'Socratic viva loop' },
                    { mode: 'teach', label: '🪞 Teach-back', desc: 'Grade understanding' },
                    { mode: 'synth', label: '🔗 Synthesis', desc: 'Cross-domain essay' },
                    { mode: 'gap', label: '🧭 Gap map', desc: 'Knowledge frontiers' },
                    { mode: 'discover', label: '✨ Discover', desc: 'Non-obvious links' },
                  ].map(({ mode, label, desc }) => (
                    <button key={mode} onClick={() => startAiSession(mode)} className="neo-btn text-left p-2.5 flex flex-col gap-0.5 border-neo-border">
                      <span className="text-xs font-bold text-neo-text">{label}</span>
                      <span className="text-[9px] text-neo-text-muted">{desc}</span>
                    </button>
                  ))}
                </div>
              ) : (
                <div className="flex flex-col gap-3">
                  <div className="flex justify-between items-center border-b border-neo-border pb-2">
                    <span className="text-xs font-bold text-neo-blue uppercase">{aiMode}</span>
                    <button onClick={() => setAiMode(null)} className="text-[10px] underline text-neo-text-muted hover:text-neo-text">Close</button>
                  </div>

                  {aiMode === 'synth' && aiLogs.length === 1 && (
                    <div className="flex flex-col gap-2 mb-2">
                      <select multiple value={aiSelection} onChange={e => setAiSelection([...e.target.selectedOptions].map(o => o.value))} className="neo-input text-xs h-28">
                        {DOMAINS.flatMap(d => (d.topics || []).map(t => (
                          <option key={t.id} value={t.title}>{t.title}</option>
                        )))}
                      </select>
                      <button onClick={runSynth} className="neo-btn bg-neo-yellow py-1 text-[10px] font-bold text-black">Generate Synthesis</button>
                    </div>
                  )}

                  <div className="flex flex-col gap-2 max-h-[280px] overflow-y-auto pr-1">
                    {aiLogs.map((log, i) => (
                      <div key={i} className={`p-2 text-xs neo-border ${log.role === 'ai' ? 'bg-[var(--neo-surface-muted)] text-neo-text' : 'bg-neo-blue text-white self-end'}`}>
                        <MarkdownRenderer content={log.text} className={log.role === 'user' ? 'text-white' : ''} />
                      </div>
                    ))}
                    {aiLoading && <div className="p-2 text-xs neo-border bg-[var(--neo-surface-muted)] text-neo-text animate-pulse">Computing…</div>}
                  </div>

                  {(aiMode === 'examiner' || aiMode === 'teach') && (
                    <div className="mt-2 flex flex-col gap-2">
                      <textarea
                        value={aiInput}
                        onChange={e => setAiInput(e.target.value)}
                        placeholder="Your answer..."
                        className="neo-input text-xs min-h-[56px]"
                        onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleAiSubmit(); } }}
                      />
                      <button onClick={handleAiSubmit} disabled={aiLoading} className="neo-btn bg-neo-blue text-white py-1.5 text-xs font-bold">Send</button>
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* Annotations preview for this topic */}
            {annotations.filter(a => a.topicId === activeTopic.id).length > 0 && (
              <div className="p-4 neo-border-thick bg-[var(--neo-surface)] flex flex-col gap-3">
                <div className="flex justify-between items-center border-b-2 border-neo-border pb-2">
                  <h3 className="neo-label-md flex items-center gap-2 text-neo-yellow text-xs"><MessageSquare size={14} /> Annotations</h3>
                  <button onClick={() => { setNotesFilter(activeTopic.id); setShowNotesPanel(true); }} className="text-[10px] underline text-neo-text-muted hover:text-neo-text">All</button>
                </div>
                {annotations.filter(a => a.topicId === activeTopic.id).slice(0, 3).map(a => (
                  <div key={a.id} className="p-2 border border-neo-border bg-[var(--neo-bg)] text-[11px] cursor-pointer" onClick={() => { setNotesFilter(activeTopic.id); setShowNotesPanel(true); }}>
                    <span className="font-bold text-neo-text">{a.kind === 'question' ? '❓' : a.kind === 'link' ? '🔗' : '💬'} {a.kind}</span>
                    {a.text && <div className="text-neo-text mt-1 line-clamp-2">{a.text}</div>}
                    {a.quote && <div className="italic text-neo-text-muted border-l-2 border-neo-border pl-1 mt-1 line-clamp-2">"{a.quote}"</div>}
                    {a.kind === 'question' && (
                      a.answer ? (
                        <div className="mt-1.5 p-1.5 bg-[var(--neo-surface-muted)] border border-neo-border">
                          <span className="font-bold text-neo-blue text-[9px] flex items-center gap-1"><Sparkles size={9} /> AI answer</span>
                          <div className="text-neo-text-muted line-clamp-3 mt-0.5"><MarkdownRenderer content={a.answer} /></div>
                        </div>
                      ) : (
                        <div className="mt-1.5 text-[9px] text-neo-blue font-bold flex items-center gap-1 animate-pulse">
                          <Loader2 size={9} className="animate-spin" /> AI is answering…
                        </div>
                      )
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
