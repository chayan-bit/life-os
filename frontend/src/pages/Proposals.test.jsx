// Issue #148: the Proposals page lists draft changesets, renders a per-attr
// before/after diff for a selected proposal, and exposes merge/reject controls
// only to a reviewer (owner/editor) - a viewer sees the diff but no controls.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import Proposals from './Proposals';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const PROPOSALS = {
  ok: true,
  data: [
    { id: 'prop_1', title: 'Bump priority', attrs: { title: 'Bump priority', status: 'open', changes: [{}], reviewers: ['usr_r'] } },
    { id: 'prop_2', title: 'Archive stale', attrs: { title: 'Archive stale', status: 'merged', changes: [{}] } },
  ],
};

const DIFF = {
  ok: true,
  data: {
    proposal_id: 'prop_1',
    title: 'Bump priority',
    status: 'open',
    needs_rebase: false,
    entities: [
      {
        entity_id: 'ent_x',
        exists: true,
        conflict: false,
        attrs: [{ attr: 'priority', before: 'low', after: 'high', changed: true }],
      },
    ],
  },
};

const NO_COMMENTS = { ok: true, data: [] };

afterEach(cleanup);

describe('Proposals', () => {
  beforeEach(() => {
    apiCall.mockReset();
  });

  it('lists proposals with their status', async () => {
    apiCall
      .mockResolvedValueOnce(PROPOSALS) // GET /api/proposal
      .mockResolvedValueOnce({ ok: true, data: { your_role: 'owner' } }); // GET /api/members

    render(<Proposals />);

    await waitFor(() => expect(screen.getByText('Bump priority')).toBeTruthy());
    expect(screen.getByText('Archive stale')).toBeTruthy();
    // Status tags render.
    expect(screen.getByText('merged')).toBeTruthy();
  });

  it('renders the before/after diff and reviewer merge controls when a proposal is opened', async () => {
    apiCall
      .mockResolvedValueOnce(PROPOSALS)
      .mockResolvedValueOnce({ ok: true, data: { your_role: 'editor' } })
      .mockResolvedValueOnce(DIFF) // GET /api/proposal/prop_1/diff
      .mockResolvedValueOnce(NO_COMMENTS); // GET /api/annotation

    render(<Proposals />);
    await waitFor(() => expect(screen.getByText('Bump priority')).toBeTruthy());

    fireEvent.click(screen.getByText('Bump priority'));

    // Diff cells: before (low) -> after (high) for the changed attr.
    await waitFor(() => expect(screen.getByText('low')).toBeTruthy());
    expect(screen.getByText('high')).toBeTruthy();
    expect(screen.getByText('priority')).toBeTruthy();

    // An editor sees the merge + reject controls.
    expect(screen.getByRole('button', { name: 'Merge' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Reject' })).toBeTruthy();
  });

  it('hides merge/reject controls from a viewer', async () => {
    apiCall
      .mockResolvedValueOnce(PROPOSALS)
      .mockResolvedValueOnce({ ok: true, data: { your_role: 'viewer' } })
      .mockResolvedValueOnce(DIFF)
      .mockResolvedValueOnce(NO_COMMENTS);

    render(<Proposals />);
    await waitFor(() => expect(screen.getByText('Bump priority')).toBeTruthy());
    fireEvent.click(screen.getByText('Bump priority'));

    // The diff still renders for a viewer...
    await waitFor(() => expect(screen.getByText('high')).toBeTruthy());
    // ...but there are no merge/reject controls.
    expect(screen.queryByRole('button', { name: 'Merge' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Reject' })).toBeNull();
  });

  it('calls the merge endpoint when a reviewer merges', async () => {
    apiCall
      .mockResolvedValueOnce(PROPOSALS)
      .mockResolvedValueOnce({ ok: true, data: { your_role: 'owner' } })
      .mockResolvedValueOnce(DIFF)
      .mockResolvedValueOnce(NO_COMMENTS)
      .mockResolvedValueOnce({ ok: true, data: { id: 'prop_1', status: 'merged' } }) // POST merge
      .mockResolvedValueOnce(PROPOSALS) // reload list
      .mockResolvedValueOnce({ ok: true, data: { your_role: 'owner' } }) // reload members
      .mockResolvedValueOnce({ ok: true, data: { ...DIFF.data, status: 'merged' } }) // reload diff
      .mockResolvedValueOnce(NO_COMMENTS);

    render(<Proposals />);
    await waitFor(() => expect(screen.getByText('Bump priority')).toBeTruthy());
    fireEvent.click(screen.getByText('Bump priority'));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Merge' })).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: 'Merge' }));

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/proposal/prop_1/merge', {}),
    );
  });
});
