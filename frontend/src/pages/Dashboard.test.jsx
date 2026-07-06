// Finding 25: the "Life OS Actions Rules" panel used to be a hardcoded
// toggle that flipped local component state and silently dropped the
// "change" on reload - it never matched services/lifeos-actions' real
// static registry (issue #93) either. This locks in the honest read-only
// replacement: real rule ids/triggers, and no toggle control claiming
// persistence it doesn't have.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import Dashboard from './Dashboard';

const renderDashboard = () => render(<Dashboard />, { wrapper: MemoryRouter });

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
vi.mock('../lib/usePipelineRun', () => ({
  usePipelineRun: () => ({ logs: [], running: false, runState: null, trigger: vi.fn() }),
}));

import { apiCall } from '../lib/api';

afterEach(cleanup);

describe('Dashboard automations panel', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: false, offline: true, data: null });
  });

  it('shows the real static action registry rules', async () => {
    renderDashboard();

    await waitFor(() => expect(screen.getByText(/ON EVENT: version\.created/)).toBeTruthy());
    expect(screen.getByText('RUN: asset.thumbnail_caption_draft')).toBeTruthy();
    expect(screen.getByText(/ON EVENT: trade\.closed/)).toBeTruthy();
    expect(screen.getByText(/ON EVENT: topic\.due/)).toBeTruthy();
  });

  it('honestly marks which rules actually fire vs which have no wired trigger yet, with no fake toggle', async () => {
    renderDashboard();

    await waitFor(() => expect(screen.getByText('LIVE')).toBeTruthy());
    expect(screen.getAllByText('NOT YET WIRED')).toHaveLength(2);

    // The old fabricated rule (never existed in the real registry) is gone.
    expect(screen.queryByText(/design_file\.updated/)).toBeNull();
    expect(screen.queryByText(/figma-implement-design/)).toBeNull();
  });
});
