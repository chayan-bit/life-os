import React, { useState, useEffect, useCallback } from 'react';
import { FileCode, Play, FileAudio, Search, GitCommit, UploadCloud, RefreshCw, ExternalLink } from 'lucide-react';
import { apiCall } from '../lib/api';
import { listFileEntities } from '../lib/vcsApi';

// Same poll-only-while-active pattern as pages/Database.jsx's JOBS_POLL_MS.
const INGEST_POLL_MS = 3000;

// Real ingest status panel (issue #91): file.imported/version.created now
// auto-enqueue an ingest job (services/lifeos-api routes/files.rs,
// routes/drive.rs); this panel is the manual trigger + status/segment-count/
// re-index UI docs/MEDIA-INTELLIGENCE.md §4 and frontend/FRONTEND.md §2 call
// for, backed by real POST /api/ingest + GET /api/entity, not mock data.
function IngestStatusPanel() {
  const [files, setFiles] = useState([]);
  const [selectedId, setSelectedId] = useState('');
  const [statusById, setStatusById] = useState({});
  const [busyId, setBusyId] = useState(null);
  const [error, setError] = useState('');

  const loadFiles = useCallback(() => {
    listFileEntities()
      .then((data) => {
        setFiles(data);
        if (!selectedId && data.length) setSelectedId(data[0].id);
      })
      .catch((e) => setError(e.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => { loadFiles(); }, [loadFiles]);

  const refreshStatus = useCallback(async (entityId) => {
    const [entityRes, segmentsRes] = await Promise.all([
      apiCall('GET', `/api/entity/${encodeURIComponent(entityId)}`),
      apiCall('GET', `/api/entity?type=segment&parent_id=${encodeURIComponent(entityId)}`),
    ]);
    const attrs = entityRes.ok ? entityRes.data?.attrs || {} : {};
    const segmentCount = segmentsRes.ok && Array.isArray(segmentsRes.data) ? segmentsRes.data.length : 0;
    setStatusById((prev) => ({
      ...prev,
      [entityId]: {
        ingestStatus: attrs.ingest_status || (segmentCount > 0 || attrs.transcript_ref ? 'completed' : 'not_ingested'),
        blockedBy: attrs.ingest_blocked_by || null,
        segmentCount,
        hasTranscript: Boolean(attrs.transcript_ref),
      },
    }));
  }, []);

  useEffect(() => {
    if (!selectedId) return undefined;
    refreshStatus(selectedId);
    const status = statusById[selectedId];
    if (busyId !== selectedId && status?.ingestStatus !== 'not_ingested') return undefined;
    const interval = setInterval(() => refreshStatus(selectedId), INGEST_POLL_MS);
    return () => clearInterval(interval);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, busyId]);

  const triggerIngest = async (entityId) => {
    setBusyId(entityId);
    setError('');
    const { ok, error: err } = await apiCall('POST', '/api/ingest', { entity_id: entityId });
    if (!ok) setError(err || 'ingest enqueue failed');
    await refreshStatus(entityId);
    setBusyId(null);
  };

  const current = selectedId ? statusById[selectedId] : null;

  return (
    <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
      <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
        <UploadCloud size={18} />
        Ingest Status
      </h3>

      {error && <div className="text-xs text-neo-red mb-3">{error}</div>}

      <div className="flex gap-2 mb-4">
        <select
          className="neo-input flex-1 text-xs"
          value={selectedId}
          onChange={(e) => setSelectedId(e.target.value)}
        >
          {files.length === 0 && <option value="">No file entities yet - commit one first</option>}
          {files.map((f) => (
            <option key={f.id} value={f.id}>{f.attrs?.name || f.id}</option>
          ))}
        </select>
        <button
          onClick={() => selectedId && triggerIngest(selectedId)}
          disabled={!selectedId || busyId === selectedId}
          className="neo-btn py-1.5 px-3 bg-neo-yellow text-[10px] font-bold flex items-center gap-1 disabled:opacity-50"
        >
          <RefreshCw size={12} className={busyId === selectedId ? 'animate-spin' : ''} />
          {current?.ingestStatus === 'not_ingested' ? 'Ingest' : 'Re-index'}
        </button>
      </div>

      {current && (
        <div className="p-3 bg-neo-bg neo-border text-xs flex flex-col gap-1.5">
          <div className="flex justify-between items-center">
            <span className="font-bold">Status</span>
            <span className={`neo-chip py-0.5 text-[9px] ${current.ingestStatus === 'unsupported' ? 'neo-chip--review' : 'neo-chip--completed'}`}>
              {(busyId === selectedId ? 'queued' : current.ingestStatus).toUpperCase()}
            </span>
          </div>
          <div className="flex justify-between items-center">
            <span className="text-neo-text-muted">Segments</span>
            <span className="font-mono">{current.segmentCount}</span>
          </div>
          {current.blockedBy && (
            <div className="pt-1.5 border-t border-neo-border border-dashed text-[10px] italic text-neo-text-muted">
              Blocked: {current.blockedBy}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// Real committed-files summary (finding 24), reusing the same
// listFileEntities() the Ingest Status panel above already calls. This used
// to be a "Universal Version History" list with invented version counts and
// commit messages. Full commit timelines and per-type diffs already exist
// for real one tab over (components/TimeTravel.jsx, Storage.jsx's Versions
// tab, backed by GET /api/vcs/history + /api/vcs/diff) - duplicating that
// logic here would just be a second implementation to keep in sync, so this
// stays a small, honest read of what is actually committed.
function CommittedFilesPanel() {
  const [files, setFiles] = useState([]);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    listFileEntities()
      .then(setFiles)
      .catch((e) => setError(e.message))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="lg:col-span-6 neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
      <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
        <GitCommit size={18} />
        Committed Files
      </h3>

      {error && <div className="text-xs text-neo-red mb-3">{error}</div>}
      {!loading && !error && files.length === 0 && (
        <div className="text-center py-6 text-neo-text-muted italic text-xs">
          No files committed to lifeos-vcs yet.
        </div>
      )}
      {loading && <div className="text-xs text-neo-text-muted italic">Loading…</div>}

      <div className="flex flex-col gap-3">
        {files.map((f) => (
          <div key={f.id} className="p-4 bg-neo-bg neo-border flex flex-col gap-2">
            <div className="flex justify-between items-start gap-2">
              <span className="neo-label-md block font-bold text-neo-blue truncate">{f.attrs?.name || f.title || f.id}</span>
              <span className={`neo-chip py-0.5 text-[9px] shrink-0 ${f.blob_ref ? 'neo-chip--completed' : 'neo-chip--review'}`}>
                {f.blob_ref ? 'COMMITTED' : 'NOT COMMITTED'}
              </span>
            </div>
            <span className="text-[10px] text-neo-text-muted font-mono">
              Last updated {new Date(f.updated_at * 1000).toLocaleString()}
            </span>
          </div>
        ))}
      </div>

      <p className="text-[10px] text-neo-text-muted italic mt-4 flex items-center gap-1">
        <ExternalLink size={11} /> Full commit timeline, branches/tags and per-type diffs live in this page's Versions tab.
      </p>
    </div>
  );
}

// Debounced free-text search over real transcript segments (finding 24),
// same GET /api/search hybrid lexical+semantic endpoint components/
// CommandBar.jsx already uses. `type=segment` rows are the timestamped
// transcript chunks lifeos-ingest writes (services/lifeos-ingest/src/
// lib.rs::insert_segment) - text, and t_start/t_end for transcribed audio.
// There is no dedicated segment-search route yet, so this reuses the
// generic entity search rather than inventing one; it replaces a fully
// hardcoded clip list that never made a network call.
const SEARCH_DEBOUNCE_MS = 300;

function formatTimestamp(secs) {
  if (typeof secs !== 'number' || Number.isNaN(secs)) return null;
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

function SegmentSearchPanel() {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState([]);
  const [status, setStatus] = useState('idle'); // idle | loading | done | error
  const [error, setError] = useState('');

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults([]);
      setStatus('idle');
      return undefined;
    }
    setStatus('loading');
    const handle = setTimeout(() => {
      apiCall('GET', `/api/search?q=${encodeURIComponent(q)}&module=files&limit=20`).then(({ ok, data, error: err }) => {
        if (!ok) {
          setError(err || 'search failed');
          setStatus('error');
          return;
        }
        setError('');
        setResults((data?.results || []).filter((r) => r.type === 'segment'));
        setStatus('done');
      });
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(handle);
  }, [query]);

  return (
    <div className="lg:col-span-6 flex flex-col gap-6">
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface flex-1">
        <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4 flex items-center gap-2">
          <FileAudio size={18} />
          Semantic Voice Search
        </h3>

        <div className="flex gap-2 mb-6">
          <div className="relative flex-1">
            <Search size={16} className="absolute left-3 top-3 text-neo-text-muted" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search transcribed audio/video segments…"
              aria-label="Search transcribed segments"
              className="neo-input w-full pl-10"
            />
          </div>
        </div>

        {error && <div className="text-xs text-neo-red mb-3">{error}</div>}

        <div className="flex flex-col gap-3 max-h-[300px] overflow-y-auto">
          {status === 'idle' && (
            <div className="text-center py-6 text-neo-text-muted italic text-xs">
              Type to search real transcript segments (GET /api/search, lexical + semantic).
            </div>
          )}
          {status === 'loading' && (
            <div className="text-center py-6 text-neo-text-muted italic text-xs">Searching…</div>
          )}
          {status === 'done' && results.length === 0 && (
            <div className="text-center py-6 text-neo-text-muted italic text-xs">
              No matching transcript segments found.
            </div>
          )}
          {results.map((seg) => {
            const start = formatTimestamp(seg.attrs?.t_start);
            const end = formatTimestamp(seg.attrs?.t_end);
            return (
              <div key={seg.id} className="p-3 bg-neo-surface neo-border text-xs flex flex-col gap-2 relative">
                <div className="flex justify-between items-center border-b border-neo-border pb-1.5">
                  <span className="font-bold flex items-center gap-1 font-mono text-[10px]">
                    <FileAudio size={12} className="text-neo-blue" />
                    {seg.id}
                  </span>
                  {typeof seg.score === 'number' && (
                    <span className="text-[9px] neo-chip neo-chip--completed py-0.5">score {seg.score.toFixed(3)}</span>
                  )}
                </div>
                <p className="italic text-neo-text-muted text-[11px]">"{seg.attrs?.text}"</p>
                {start && (
                  <div className="flex justify-between items-center pt-2">
                    <span className="text-[10px] font-mono bg-neo-bg px-1.5 py-0.5 border">
                      {start} - {end}
                    </span>
                    <span className="text-[10px] text-neo-blue font-bold flex items-center gap-0.5">
                      <Play size={10} className="fill-neo-blue" /> {start}
                    </span>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

export default function VcsIngest() {
  // Interactive slider for FastCDC deduplication simulator
  const [chunkSizeKb, setChunkSizeKb] = useState(64);
  const totalBaseSizeMb = 120.4;
  // Compute mock deduplication ratio based on chunk size
  const dedupRatio = Math.max(12, Math.min(84, Math.round(92 - (chunkSizeKb / 4)))).toFixed(1);
  const finalSizeMb = (totalBaseSizeMb * (1 - (parseFloat(dedupRatio) / 100))).toFixed(1);

  return (
    <div className="flex flex-col gap-8">
      {/* Overview */}
      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface">
        <h2 className="neo-title-md mb-2 flex items-center gap-2">
          <FileCode size={24} className="text-neo-blue" />
          `lifeos-vcs` & Media Intelligence
        </h2>
        <p className="neo-body-md text-neo-text-muted">
          Life OS extends version control to all files (images, designs, videos, audio) using content-addressed BLAKE3 + FastCDC chunking. At the same time, the Rust-based <strong>media intelligence pipeline</strong> parses audio files using whisper-rs, mapping voice queries to timestamped database segments.
        </p>
      </div>

      {/* Ingest Status - real, backed by POST /api/ingest + GET /api/entity (issue #91) */}
      <IngestStatusPanel />

      {/* Main Grid - both panels are real API-backed reads (finding 24) */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-8">
        <CommittedFilesPanel />
        <SegmentSearchPanel />
      </div>

      {/* FastCDC Deduplication Simulator - illustrative model only, not
          measured against your real files: there is no dedup/chunk-size
          stats endpoint yet, so the numbers below are a formula over the
          slider position, not data read from lifeos-vcs (finding 24). */}
      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        <h3 className="neo-title-md border-b-2 border-neo-border pb-3 mb-4">
          FastCDC Deduplication Simulator <span className="text-[10px] font-normal text-neo-text-muted uppercase">(illustrative model, not your real files)</span>
        </h3>
        <p className="text-xs text-neo-text-muted mb-4">
          Content-Defined Chunking splits file revisions dynamically to maximize block reuse. Move the slider to see how block-size boundaries affect the ratio, conceptually - lifeos-vcs does not yet expose a real per-workspace dedup metric to compute this from.
        </p>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-6 items-center">
          <div className="p-4 bg-neo-bg neo-border flex flex-col gap-2">
            <label className="neo-label-sm font-bold block">TARGET CHUNK BOUNDARY: {chunkSizeKb} KB</label>
            <input
              type="range"
              min={16}
              max={256}
              step={16}
              value={chunkSizeKb}
              onChange={(e) => setChunkSizeKb(parseInt(e.target.value))}
              className="w-full cursor-pointer h-2 bg-neo-surface rounded-none border-2 border-neo-border accent-black"
            />
            <div className="flex justify-between text-[10px] font-mono text-neo-text-muted">
              <span>16 KB</span>
              <span>256 KB</span>
            </div>
          </div>

          <div className="p-4 bg-neo-surface border-2 border-neo-border text-center">
            <span className="neo-label-sm block text-neo-text-muted">DEDUPLICATION RATIO</span>
            <span className="neo-title-md text-3xl text-neo-mint font-black block my-1">{dedupRatio}%</span>
            <span className="text-[10px] block">Illustrative, not measured</span>
          </div>

          <div className="p-4 bg-neo-surface border-2 border-neo-border text-center">
            <span className="neo-label-sm block text-neo-text-muted">STORED SIZE VS BASE</span>
            <span className="neo-title-md text-3xl text-neo-blue font-black block my-1">{finalSizeMb} MB</span>
            <span className="text-[10px] block">Down from {totalBaseSizeMb} MB (example base size)</span>
          </div>
        </div>
      </div>

    </div>
  );
}
