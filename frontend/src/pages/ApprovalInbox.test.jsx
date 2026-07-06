// Issue #142: the PWA approval inbox lists pending rows grouped by kind,
// resolves them via /api/approval/:id/{approve,deny} with optimistic removal,
// and refuses a typed-confirm gate until the exact phrase is entered.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ApprovalInbox from './ApprovalInbox';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const DRAFT = {
  id: 'ent_draft', module: 'bot', type: 'draft', title: null,
  status: 'pending_approval', attrs: { text: 'announce the launch' },
};
const GATE = {
  id: 'ent_gate', module: 'pipelines', type: 'pending_approval', title: 'T5 crate',
  status: 'awaiting_approval', attrs: { requires_typed_confirm: true, node: 't5', tier: 'T5' },
};

afterEach(cleanup);

describe('ApprovalInbox', () => {
  beforeEach(() => {
    apiCall.mockReset();
  });

  it('lists pending rows grouped by kind', async () => {
    apiCall.mockResolvedValueOnce({ ok: true, data: [DRAFT, GATE] });

    render(<ApprovalInbox />);

    await waitFor(() => expect(screen.getByText('announce the launch')).toBeTruthy());
    expect(screen.getByText(/^Drafts/)).toBeTruthy();
    expect(screen.getByText(/^Build gates/)).toBeTruthy();
  });

  it('approves a plain draft optimistically via the approve route', async () => {
    apiCall
      .mockResolvedValueOnce({ ok: true, data: [DRAFT] }) // load
      .mockResolvedValueOnce({ ok: true, data: { status: 'approved' } }); // approve

    render(<ApprovalInbox />);
    await waitFor(() => expect(screen.getByText('announce the launch')).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: /Approve/i }));

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/approval/ent_draft/approve', {}),
    );
    // Optimistic removal: the row is gone.
    await waitFor(() => expect(screen.queryByText('announce the launch')).toBeNull());
  });

  it('refuses a typed-confirm gate until the exact phrase is typed', async () => {
    apiCall
      .mockResolvedValueOnce({ ok: true, data: [GATE] }) // load
      .mockResolvedValueOnce({ ok: true, data: { status: 'approved' } }); // approve

    render(<ApprovalInbox />);
    await waitFor(() => expect(screen.getByText('T5 crate')).toBeTruthy());

    const approveBtn = () => screen.getByRole('button', { name: /Approve/i });
    // Approve is disabled with no phrase typed.
    expect(approveBtn().disabled).toBe(true);

    // Wrong phrase keeps it disabled.
    const input = screen.getByLabelText(/Typed confirmation for t5/i);
    fireEvent.change(input, { target: { value: 'nope' } });
    expect(approveBtn().disabled).toBe(true);

    // Exact phrase enables it, and approve sends { typed: 't5' }.
    fireEvent.change(input, { target: { value: 't5' } });
    expect(approveBtn().disabled).toBe(false);
    fireEvent.click(approveBtn());

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/approval/ent_gate/approve', { typed: 't5' }),
    );
  });

  it('rolls the row back into the list when the API call fails', async () => {
    apiCall
      .mockResolvedValueOnce({ ok: true, data: [DRAFT] }) // load
      .mockResolvedValueOnce({ ok: false, error: 'boom' }); // deny fails

    render(<ApprovalInbox />);
    await waitFor(() => expect(screen.getByText('announce the launch')).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: /Deny/i }));

    // Row returns after the failure, and the error surfaces.
    await waitFor(() => expect(screen.getByText('boom')).toBeTruthy());
    expect(screen.getByText('announce the launch')).toBeTruthy();
  });
});
