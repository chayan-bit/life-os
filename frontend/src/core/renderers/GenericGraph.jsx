import React, { useEffect, useMemo, useState } from 'react';
import { apiCall } from '../../lib/api';
import { resolveDisplay } from './displayHelpers';

// Generic node-link graph renderer (T1, issue #133) - the 9th renderer kind,
// proving the self-extension ladder can grow past the closed set of 8. Any
// module can declare a `kind: 'graph'` view (e.g. modules/learning's
// "Knowledge Tree" over topics) and get a graph with zero bespoke UI code,
// matching every sibling Generic*.jsx's props contract exactly:
// entities/setEntities/display/onSelect come from ModuleManifestPage.jsx's
// render call, driven purely by manifest config.
const WIDTH = 560;
const HEIGHT = 360;
const NODE_RADIUS = 18;

// Deterministic radial layout: node i sits at angle (2*pi*i/n) around a
// center, same input -> same output every time (no Math.random, no
// Date.now) - the T1 validator's a11y/visual-diff gate depends on this
// being reproducible across runs.
export function computeCircularLayout(entities, width = WIDTH, height = HEIGHT) {
  const cx = width / 2;
  const cy = height / 2;
  const radius = Math.min(width, height) / 2 - NODE_RADIUS * 2;
  const n = entities.length;
  const positions = new Map();
  entities.forEach((entity, i) => {
    const angle = n === 0 ? 0 : (2 * Math.PI * i) / n;
    positions.set(entity.id, {
      x: cx + radius * Math.cos(angle),
      y: cy + radius * Math.sin(angle),
    });
  });
  return positions;
}

// `/api/edge` has no per-module scoping (services/lifeos-api/src/routes/
// edge.rs's ListParams only filters by src_id/dst_id/rel) - it is
// workspace-scoped by the caller's headers (lib/api.js::authHeaders), and
// this renderer additionally filters client-side to edges whose endpoints
// are both in the current view's entity set, so a workspace with many
// modules' edges never bleeds unrelated nodes into this graph.
function relevantEdges(edges, entityIds) {
  return edges.filter((edge) => entityIds.has(edge.src_id) && entityIds.has(edge.dst_id));
}

export default function GenericGraph({ entities, display = {}, onSelect, emptyLabel = 'No nodes yet.' }) {
  const [edges, setEdges] = useState([]);
  const [edgeState, setEdgeState] = useState('loading');

  useEffect(() => {
    let cancelled = false;
    setEdgeState('loading');
    apiCall('GET', '/api/edge?limit=2000').then(({ ok, data, offline }) => {
      if (cancelled) return;
      if (offline) { setEdgeState('offline'); return; }
      setEdges(ok ? data || [] : []);
      setEdgeState('ready');
    });
    return () => { cancelled = true; };
  }, []);

  const positions = useMemo(() => computeCircularLayout(entities || []), [entities]);
  const entityIds = useMemo(() => new Set((entities || []).map((e) => e.id)), [entities]);
  const links = useMemo(() => relevantEdges(edges, entityIds), [edges, entityIds]);

  const activate = (entity) => onSelect?.(entity);
  const handleKeyDown = (entity) => (event) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      activate(entity);
    }
  };

  if (!entities?.length) {
    return <p className="text-xs text-neo-text-muted">{emptyLabel}</p>;
  }

  return (
    <div className="neo-border overflow-auto bg-neo-bg" style={{ maxWidth: '100%' }}>
      <svg
        role="img"
        aria-label="Node-link graph"
        width={WIDTH}
        height={HEIGHT}
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      >
        {links.map((edge) => {
          const from = positions.get(edge.src_id);
          const to = positions.get(edge.dst_id);
          if (!from || !to) return null;
          return (
            <line
              key={edge.id}
              x1={from.x}
              y1={from.y}
              x2={to.x}
              y2={to.y}
              stroke="var(--neo-border, #1a1a1a)"
              strokeWidth={1.5}
              opacity={0.5}
            />
          );
        })}
        {entities.map((entity) => {
          const { title } = resolveDisplay(entity, display);
          const pos = positions.get(entity.id);
          if (!pos) return null;
          return (
            <g
              key={entity.id}
              transform={`translate(${pos.x}, ${pos.y})`}
              tabIndex={0}
              role="button"
              aria-label={`${title}`}
              onClick={() => activate(entity)}
              onKeyDown={handleKeyDown(entity)}
              style={{ cursor: onSelect ? 'pointer' : 'default', outline: 'none' }}
              data-graph-node={entity.id}
            >
              <circle r={NODE_RADIUS} fill="var(--neo-yellow, #ffd60a)" stroke="var(--neo-border, #1a1a1a)" strokeWidth={2} />
              <text
                textAnchor="middle"
                dominantBaseline="central"
                y={NODE_RADIUS + 12}
                className="text-[9px] font-mono"
                fill="var(--neo-text, #1a1a1a)"
              >
                {String(title).slice(0, 14)}
              </text>
            </g>
          );
        })}
      </svg>
      {edgeState === 'offline' && <p className="text-xs text-neo-red font-bold p-2">Backend unreachable - edges not loaded.</p>}
    </div>
  );
}
