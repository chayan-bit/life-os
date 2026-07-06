// Finding 52: AgentLedger used to silently render "No agent actions yet."
// whether the events fetch genuinely returned nothing or actually FAILED,
// masking an outage as emptiness.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import AgentLedger from './AgentLedger';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
vi.mock('../lib/agentActions', () => ({ undoAction: vi.fn(), undoPlan: vi.fn() }));
import { apiCall } from '../lib/api';

afterEach(cleanup);

beforeEach(() => {
  apiCall.mockReset();
});

describe('AgentLedger error vs empty states', () => {
  it('shows the genuine empty state when both event fetches succeed with no rows', async () => {
    apiCall.mockResolvedValue({ ok: true, data: [], offline: false });

    render(<AgentLedger />);

    await waitFor(() => expect(screen.getByText('No agent actions yet.')).toBeTruthy());
    expect(screen.queryByText(/Failed to load/i)).toBeNull();
  });

  it('shows an error message (not the empty state) when a fetch fails', async () => {
    apiCall.mockImplementation((method, path) => {
      if (path.includes('action.applied')) {
        return Promise.resolve({ ok: false, data: null, error: 'boom', offline: false });
      }
      return Promise.resolve({ ok: true, data: [], offline: false });
    });

    render(<AgentLedger />);

    await waitFor(() => expect(screen.getByText(/Failed to load the agent ledger/i)).toBeTruthy());
    expect(screen.queryByText('No agent actions yet.')).toBeNull();
  });

  it('retries the fetch when the retry button is clicked', async () => {
    apiCall.mockResolvedValueOnce({ ok: false, data: null, offline: false });
    apiCall.mockResolvedValueOnce({ ok: false, data: null, offline: false });

    render(<AgentLedger />);
    await waitFor(() => expect(screen.getByText(/Failed to load the agent ledger/i)).toBeTruthy());

    apiCall.mockResolvedValue({ ok: true, data: [], offline: false });
    screen.getByRole('button', { name: /Retry/i }).click();

    await waitFor(() => expect(screen.getByText('No agent actions yet.')).toBeTruthy());
  });
});
