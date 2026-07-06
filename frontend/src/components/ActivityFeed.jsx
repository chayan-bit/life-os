// Live activity feed (issue #150). Renders a workspace's recent domain events
// and tails new ones in real time.
//
// SSE-over-fetch (NOT EventSource) on purpose: EventSource cannot send request
// headers, so it could neither carry our `Authorization: Bearer` token nor the
// `X-Workspace-Id`/`Last-Event-ID` headers. Instead we open the stream with
// `fetch` + a ReadableStream body reader and our normal `authHeaders()`, so the
// endpoint authenticates exactly like every other call and no token is ever
// leaked through the query string. On a dropped connection we reconnect,
// resuming from the last event id via the `Last-Event-ID` header.
import React, { useEffect, useRef, useState } from 'react';
import { Activity, AlertTriangle } from 'lucide-react';
import { API_BASE, authHeaders, apiCall } from '../lib/api';

const BACKLOG_LIMIT = 30;
const MAX_ROWS = 100;
const RECONNECT_MS = 3000;

function relativeTime(tsSecs) {
  if (!tsSecs) return '';
  const deltaSec = Math.max(0, Math.floor(Date.now() / 1000 - tsSecs));
  if (deltaSec < 60) return `${deltaSec}s ago`;
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`;
  if (deltaSec < 86400) return `${Math.floor(deltaSec / 3600)}h ago`;
  return `${Math.floor(deltaSec / 86400)}d ago`;
}

// Parse one SSE frame (lines separated by \n, fields `id:`/`event:`/`data:`).
// Comment/keep-alive frames (only `:` lines) carry no data and are ignored.
function parseFrame(frame) {
  const lines = frame.split(/\r?\n/);
  const dataParts = [];
  let id = null;
  for (const line of lines) {
    if (!line || line.startsWith(':')) continue;
    if (line.startsWith('data:')) dataParts.push(line.slice(5).replace(/^ /, ''));
    else if (line.startsWith('id:')) id = line.slice(3).trim();
  }
  if (!dataParts.length) return null;
  try {
    return { id, event: JSON.parse(dataParts.join('\n')) };
  } catch {
    return null;
  }
}

export default function ActivityFeed() {
  const [events, setEvents] = useState([]);
  const [status, setStatus] = useState('connecting'); // connecting | live | error
  const lastIdRef = useRef('');

  useEffect(() => {
    let aborter = null;
    let reconnectTimer = null;
    let cancelled = false;

    const pushEvent = (ev, id) => {
      if (id) lastIdRef.current = id;
      else if (ev.id) lastIdRef.current = ev.id;
      setEvents((prev) => [ev, ...prev].slice(0, MAX_ROWS));
      setStatus('live');
    };

    const connect = async () => {
      if (cancelled) return;
      aborter = new AbortController();
      try {
        const headers = { ...authHeaders(false), Accept: 'text/event-stream' };
        if (lastIdRef.current) headers['Last-Event-ID'] = lastIdRef.current;
        const res = await fetch(`${API_BASE}/api/events/stream`, {
          headers,
          signal: aborter.signal,
        });
        if (!res.ok || !res.body) throw new Error(`stream failed: ${res.status}`);
        if (!cancelled) setStatus('live'); // connection open, even before the first event

        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buf = '';
        // eslint-disable-next-line no-constant-condition
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          buf += decoder.decode(value, { stream: true });
          let idx;
          while ((idx = buf.indexOf('\n\n')) !== -1) {
            const parsed = parseFrame(buf.slice(0, idx));
            buf = buf.slice(idx + 2);
            if (parsed && !cancelled) pushEvent(parsed.event, parsed.id);
          }
        }
        throw new Error('stream ended');
      } catch (err) {
        if (cancelled || err?.name === 'AbortError') return;
        setStatus('error');
        reconnectTimer = setTimeout(connect, RECONNECT_MS);
      }
    };

    // Seed the initial list from the backlog, then tail only newer events.
    const start = async () => {
      const { ok, data } = await apiCall('GET', `/api/event?limit=${BACKLOG_LIMIT}`);
      if (cancelled) return;
      if (ok && Array.isArray(data)) {
        setEvents(data.slice(0, MAX_ROWS));
        if (data.length) lastIdRef.current = data[0].id; // list is newest-first
      }
      connect();
    };
    start();

    return () => {
      cancelled = true;
      aborter?.abort();
      if (reconnectTimer) clearTimeout(reconnectTimer);
    };
  }, []);

  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b-2 border-neo-border flex items-center gap-2">
        <Activity size={16} />
        <span className="neo-title-md text-sm">Activity</span>
        <span
          className={`ml-auto w-2.5 h-2.5 rounded-full ${
            status === 'live'
              ? 'bg-neo-mint animate-pulse'
              : status === 'error'
                ? 'bg-neo-red'
                : 'bg-neo-text-muted animate-pulse'
          }`}
          title={
            status === 'live'
              ? 'Live'
              : status === 'error'
                ? 'Reconnecting…'
                : 'Connecting…'
          }
        />
      </div>

      {status === 'error' && (
        <div className="px-4 py-2 bg-neo-red/10 text-neo-red text-[11px] font-mono flex items-center gap-1.5 border-b-2 border-neo-border">
          <AlertTriangle size={12} />
          <span>Live feed dropped - reconnecting…</span>
        </div>
      )}

      <div className="flex-1 overflow-y-auto">
        {events.length === 0 ? (
          <div className="p-6 text-center neo-label-sm text-neo-text-muted text-xs">
            {status === 'connecting' ? 'Connecting to live feed…' : 'No recent activity yet.'}
          </div>
        ) : (
          <ul className="divide-y-2 divide-neo-border">
            {events.map((ev) => (
              <li key={ev.id} className="px-4 py-2.5 flex flex-col gap-0.5">
                <div className="flex items-center gap-2">
                  <span className="neo-tag text-[9px] px-1.5 py-0.5 bg-neo-yellow">{ev.type}</span>
                  <span className="ml-auto neo-label-sm text-[10px] text-neo-text-muted">
                    {relativeTime(ev.ts)}
                  </span>
                </div>
                <div className="text-[11px] font-mono text-neo-text-muted truncate">
                  {ev.actor ? `${ev.actor} · ` : ''}
                  {ev.entity_id || '—'}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
