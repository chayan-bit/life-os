// T1 proof artifact (issue #133): GenericGraph is the 9th renderer kind,
// fetching /api/edge itself (no sibling renderer needs edges, so this is new
// surface) and laying out entities as an SVG node-link graph.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import GenericGraph, { computeCircularLayout } from './GenericGraph';

vi.mock('../../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../../lib/api';

const ENTITIES = [
  { id: 'e1', title: 'Alpha' },
  { id: 'e2', title: 'Beta' },
  { id: 'e3', title: 'Gamma' },
];

const EDGES = [
  { id: 'edg_1', src_id: 'e1', dst_id: 'e2', rel: 'related_to' },
  { id: 'edg_2', src_id: 'e2', dst_id: 'e3', rel: 'related_to' },
];

afterEach(cleanup);

describe('GenericGraph - renders nodes/edges from mocked fetch', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: EDGES, offline: false });
  });

  it('renders one graph node per entity and the edges that connect them', async () => {
    render(<GenericGraph entities={ENTITIES} display={{ title: 'title' }} />);

    await waitFor(() => expect(apiCall).toHaveBeenCalledWith('GET', '/api/edge?limit=2000'));

    for (const entity of ENTITIES) {
      expect(document.querySelector(`[data-graph-node="${entity.id}"]`)).toBeTruthy();
    }
    await waitFor(() => {
      expect(document.querySelectorAll('line')).toHaveLength(2);
    });
  });

  it('shows the empty label when there are no entities', () => {
    render(<GenericGraph entities={[]} />);
    expect(screen.getByText('No nodes yet.')).toBeTruthy();
  });
});

describe('GenericGraph - keyboard activation', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: [], offline: false });
  });

  it('activates a node on click, same as Enter/Space on a focused node', async () => {
    const onSelect = vi.fn();
    render(<GenericGraph entities={ENTITIES} display={{ title: 'title' }} onSelect={onSelect} />);

    const node = await screen.findByRole('button', { name: 'Alpha' });
    expect(node.tabIndex).toBe(0);

    fireEvent.click(node);
    expect(onSelect).toHaveBeenCalledWith(ENTITIES[0]);

    onSelect.mockClear();
    node.focus();
    fireEvent.keyDown(node, { key: 'Enter' });
    expect(onSelect).toHaveBeenCalledWith(ENTITIES[0]);

    onSelect.mockClear();
    fireEvent.keyDown(node, { key: ' ' });
    expect(onSelect).toHaveBeenCalledWith(ENTITIES[0]);
  });

  it('does not activate on an unrelated key', async () => {
    const onSelect = vi.fn();
    render(<GenericGraph entities={ENTITIES} display={{ title: 'title' }} onSelect={onSelect} />);

    const node = await screen.findByRole('button', { name: 'Alpha' });
    fireEvent.keyDown(node, { key: 'Tab' });
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe('computeCircularLayout - determinism', () => {
  it('produces identical positions for the same input across two calls', () => {
    const a = computeCircularLayout(ENTITIES);
    const b = computeCircularLayout(ENTITIES);
    for (const entity of ENTITIES) {
      expect(a.get(entity.id)).toEqual(b.get(entity.id));
    }
  });

  it('places every entity at a distinct position for a non-trivial graph', () => {
    const positions = computeCircularLayout(ENTITIES);
    const unique = new Set([...positions.values()].map((p) => `${p.x.toFixed(3)},${p.y.toFixed(3)}`));
    expect(unique.size).toBe(ENTITIES.length);
  });
});
