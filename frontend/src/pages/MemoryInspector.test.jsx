// Finding 52: MemoryInspector used to render the same "nothing yet" empty
// state whether the recall ledger / rules fetch genuinely returned nothing
// or actually FAILED, masking an outage as emptiness. These tests assert
// the two are now visibly distinct.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import MemoryInspector from './MemoryInspector';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

afterEach(cleanup);

beforeEach(() => {
  apiCall.mockReset();
});

describe('MemoryInspector error vs empty states', () => {
  it('shows the genuine empty state when both fetches succeed with no data', async () => {
    apiCall.mockImplementation((method, path) => {
      if (path.startsWith('/api/memory/inspect')) {
        return Promise.resolve({ ok: true, data: { entries: [], stats: null }, offline: false });
      }
      if (path.startsWith('/api/memory/rules')) {
        return Promise.resolve({ ok: true, data: { rules: [] }, offline: false });
      }
      return Promise.resolve({ ok: false, data: null, offline: false });
    });

    render(<MemoryInspector />);

    await waitFor(() => expect(screen.getByText('No memory activity yet.')).toBeTruthy());
    expect(screen.getByText('No learned rules yet.')).toBeTruthy();
    expect(screen.queryByText(/Failed to load/i)).toBeNull();
  });

  it('shows an error message (not the empty state) when the ledger fetch fails', async () => {
    apiCall.mockImplementation((method, path) => {
      if (path.startsWith('/api/memory/inspect')) {
        return Promise.resolve({ ok: false, data: null, error: 'boom', offline: false });
      }
      if (path.startsWith('/api/memory/rules')) {
        return Promise.resolve({ ok: true, data: { rules: [] }, offline: false });
      }
      return Promise.resolve({ ok: false, data: null, offline: false });
    });

    render(<MemoryInspector />);

    await waitFor(() => expect(screen.getByText(/Failed to load the recall ledger/i)).toBeTruthy());
    expect(screen.queryByText('No memory activity yet.')).toBeNull();
    // The unrelated rules section still resolves to its own genuine empty state.
    expect(screen.getByText('No learned rules yet.')).toBeTruthy();
  });

  it('shows an error message for procedural rules when that fetch fails independently', async () => {
    apiCall.mockImplementation((method, path) => {
      if (path.startsWith('/api/memory/inspect')) {
        return Promise.resolve({ ok: true, data: { entries: [], stats: null }, offline: false });
      }
      if (path.startsWith('/api/memory/rules')) {
        return Promise.resolve({ ok: false, data: null, error: 'boom', offline: false });
      }
      return Promise.resolve({ ok: false, data: null, offline: false });
    });

    render(<MemoryInspector />);

    await waitFor(() => expect(screen.getByText(/Failed to load procedural rules/i)).toBeTruthy());
    expect(screen.queryByText('No learned rules yet.')).toBeNull();
    expect(screen.getByText('No memory activity yet.')).toBeTruthy();
  });
});
