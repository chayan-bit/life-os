// Issue #145: the Observe dashboard renders /api/metrics's agent-turn,
// cache, gating, recovery, and build-run aggregates - no client-side event
// scraping. These tests cover the data-mapping helpers plus the loading,
// offline, and populated render states.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import ObserveDashboard, { objectToBars, cacheHitRate } from './ObserveDashboard';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const FULL_METRICS = {
  agent_turns_total: 5,
  agent_turns_gated: 2,
  agent_turns_allowed: 3,
  tokens_in: 100,
  tokens_out: 50,
  avg_latency_ms: 1234,
  turns_by_day: [
    { day: '2026-07-04', turns: 3, tokens_in: 60, tokens_out: 30 },
    { day: '2026-07-05', turns: 2, tokens_in: 40, tokens_out: 20 },
  ],
  cache_by_result: { exact: 2, semantic: 1, none: 2 },
  recovery_action_count: 3,
  recovery_turns_count: 2,
  recovery_by_kind: { retry: 2, arg_repair: 1 },
  strategy_leaderboard: [
    { group: 'rag.rewrite', variant: 'entity_focus', plays: 3, successes: 3, rate: 1 },
    { group: 'rag.rewrite', variant: 'plain', plays: 2, successes: 1, rate: 0.5 },
    { group: 'planner.prompt', variant: 'checklist', plays: 1, successes: 1, rate: 1 },
  ],
  recent_build_nodes: [
    { run_id: 'build_1', node: 't1-schema', tier: 'T1', outcome: 'completed', type: 'build.node.completed', ts: 100 },
    { run_id: 'build_1', node: 't2-api', tier: 'T2', outcome: 'failed', type: 'build.node.failed', ts: 101 },
  ],
  build_runs_by_outcome: { failed: 1 },
};

const EMPTY_METRICS = {
  agent_turns_total: 0,
  agent_turns_gated: 0,
  agent_turns_allowed: 0,
  tokens_in: 0,
  tokens_out: 0,
  avg_latency_ms: 0,
  turns_by_day: [],
  cache_by_result: {},
  recovery_action_count: 0,
  recovery_turns_count: 0,
  recovery_by_kind: {},
  strategy_leaderboard: [],
  recent_build_nodes: [],
  build_runs_by_outcome: {},
};

afterEach(cleanup);

describe('objectToBars', () => {
  it('maps a count object into label/value pairs', () => {
    expect(objectToBars({ exact: 2, semantic: 1 })).toEqual([
      { label: 'exact', value: 2 },
      { label: 'semantic', value: 1 },
    ]);
  });

  it('returns an empty array for null/undefined input', () => {
    expect(objectToBars(undefined)).toEqual([]);
    expect(objectToBars(null)).toEqual([]);
  });
});

describe('cacheHitRate', () => {
  it('computes hits / (hits + misses)', () => {
    expect(cacheHitRate({ exact: 2, semantic: 1, none: 1 })).toBeCloseTo(0.75);
  });

  it('returns null when there is no data yet, not 0', () => {
    expect(cacheHitRate({})).toBeNull();
    expect(cacheHitRate(undefined)).toBeNull();
  });

  it('returns 0 when every turn missed', () => {
    expect(cacheHitRate({ none: 5 })).toBe(0);
  });
});

describe('ObserveDashboard', () => {
  beforeEach(() => {
    apiCall.mockReset();
  });

  it('shows a loading state before the metrics call resolves', () => {
    apiCall.mockReturnValueOnce(new Promise(() => {})); // never resolves
    render(<ObserveDashboard />);
    expect(screen.getByText(/Loading metrics/i)).toBeTruthy();
  });

  it('shows an offline notice when the backend is unreachable', async () => {
    apiCall.mockResolvedValueOnce({ ok: false, data: null, offline: true });
    render(<ObserveDashboard />);
    await waitFor(() => expect(screen.getByText(/Backend unreachable/i)).toBeTruthy());
  });

  it('renders empty-state notes per card when metrics are all zero', async () => {
    apiCall.mockResolvedValueOnce({ ok: true, data: EMPTY_METRICS, offline: false });
    render(<ObserveDashboard />);
    await waitFor(() => expect(screen.getAllByText(/No agent turns recorded yet/i).length).toBeGreaterThan(0));
    expect(screen.getByText(/No cache-eligible turns yet/i)).toBeTruthy();
    expect(screen.getByText(/No recovery actions recorded yet/i)).toBeTruthy();
    expect(screen.getByText(/No build runs recorded yet/i)).toBeTruthy();
    expect(screen.getByText(/No strategy decisions recorded yet/i)).toBeTruthy();
  });

  it('renders the eval card note explaining there is no events-backed data yet', async () => {
    apiCall.mockResolvedValueOnce({ ok: true, data: EMPTY_METRICS, offline: false });
    render(<ObserveDashboard />);
    await waitFor(() => expect(screen.getByText(/history\.jsonl/i)).toBeTruthy());
  });

  it('renders populated cards from real /api/metrics data', async () => {
    apiCall.mockResolvedValueOnce({ ok: true, data: FULL_METRICS, offline: false });
    render(<ObserveDashboard />);

    await waitFor(() => expect(screen.getByText('5')).toBeTruthy()); // agent_turns_total stat
    expect(screen.getByText('60%')).toBeTruthy(); // cache hit rate: (2+1)/5
    expect(screen.getAllByText('build_1').length).toBe(2);
    expect(screen.getByText('t1-schema')).toBeTruthy();
    expect(screen.getByText('completed')).toBeTruthy();
    expect(screen.getByText('failed')).toBeTruthy();

    // Strategy optimizer leaderboard (issue #156): both decision groups'
    // top variants render, with the higher-rate rag.rewrite variant present.
    expect(screen.getAllByText('rag.rewrite').length).toBe(2);
    expect(screen.getByText('planner.prompt')).toBeTruthy();
    expect(screen.getByText('entity_focus')).toBeTruthy();
    expect(screen.getByText('checklist')).toBeTruthy();
  });

  it('calls GET /api/metrics on mount and again on refresh', async () => {
    apiCall.mockResolvedValue({ ok: true, data: EMPTY_METRICS, offline: false });
    render(<ObserveDashboard />);
    await waitFor(() => expect(apiCall).toHaveBeenCalledWith('GET', '/api/metrics'));

    apiCall.mockClear();
    screen.getByRole('button', { name: /Refresh/i }).click();
    await waitFor(() => expect(apiCall).toHaveBeenCalledWith('GET', '/api/metrics'));
  });
});
