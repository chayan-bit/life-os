// Issue #146: the Members page lists members with roles, exposes owner-only
// role changes / removal / invite creation (accept URL shown once) and pending
// invites, and hides those controls from non-owners.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import Members from './Members';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const OWNER_MEMBERS = {
  ok: true,
  data: {
    your_role: 'owner',
    members: [
      { user_id: 'usr_o', role: 'owner', email: 'owner@x.com', name: 'Owner' },
      { user_id: 'usr_e', role: 'editor', email: 'ed@x.com', name: 'Ed' },
    ],
  },
};

afterEach(cleanup);

describe('Members', () => {
  beforeEach(() => {
    apiCall.mockReset();
    // Default clipboard stub so the copy button never throws.
    Object.assign(navigator, { clipboard: { writeText: vi.fn().mockResolvedValue() } });
  });

  it('lists members with their roles and the owner-only controls', async () => {
    apiCall
      .mockResolvedValueOnce(OWNER_MEMBERS) // GET /api/members
      .mockResolvedValueOnce({ ok: true, data: { invites: [] } }); // GET /api/invites

    render(<Members />);

    await waitFor(() => expect(screen.getByText('Owner')).toBeTruthy());
    expect(screen.getByText('Ed')).toBeTruthy();
    // Owner sees the invite form.
    expect(screen.getByLabelText('Invite email')).toBeTruthy();
    // Role editors are selects for the owner.
    expect(screen.getByLabelText('Role for ed@x.com')).toBeTruthy();
  });

  it('changes a member role via POST /api/member/:id/role', async () => {
    apiCall
      .mockResolvedValueOnce(OWNER_MEMBERS)
      .mockResolvedValueOnce({ ok: true, data: { invites: [] } })
      .mockResolvedValueOnce({ ok: true, data: { user_id: 'usr_e', role: 'viewer' } }) // role change
      .mockResolvedValueOnce(OWNER_MEMBERS) // reload
      .mockResolvedValueOnce({ ok: true, data: { invites: [] } });

    render(<Members />);
    await waitFor(() => expect(screen.getByText('Ed')).toBeTruthy());

    fireEvent.change(screen.getByLabelText('Role for ed@x.com'), { target: { value: 'viewer' } });

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/member/usr_e/role', { role: 'viewer' }),
    );
  });

  it('creates an invite and shows the accept URL exactly once', async () => {
    apiCall
      .mockResolvedValueOnce(OWNER_MEMBERS)
      .mockResolvedValueOnce({ ok: true, data: { invites: [] } })
      .mockResolvedValueOnce({
        ok: true,
        data: { id: 'inv_1', email: 'new@x.com', role: 'viewer', accept_url: 'http://app/invite/accept?token=SECRET123' },
      }) // POST /api/invite
      .mockResolvedValueOnce(OWNER_MEMBERS) // reload members
      .mockResolvedValueOnce({ ok: true, data: { invites: [{ id: 'inv_1', email: 'new@x.com', role: 'viewer' }] } });

    render(<Members />);
    await waitFor(() => expect(screen.getByLabelText('Invite email')).toBeTruthy());

    fireEvent.change(screen.getByLabelText('Invite email'), { target: { value: 'new@x.com' } });
    fireEvent.click(screen.getByRole('button', { name: /Send invite/i }));

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/invite', expect.objectContaining({ email: 'new@x.com', role: 'viewer' })),
    );
    // The one-time accept URL is surfaced.
    await waitFor(() => expect(screen.getByText(/token=SECRET123/)).toBeTruthy());
  });

  it('hides owner-only controls from a viewer', async () => {
    apiCall.mockResolvedValueOnce({
      ok: true,
      data: {
        your_role: 'viewer',
        members: [{ user_id: 'usr_v', role: 'viewer', email: 'v@x.com', name: 'Vic' }],
      },
    });

    render(<Members />);
    await waitFor(() => expect(screen.getByText('Vic')).toBeTruthy());

    // No invite form, no role select, no remove button for a viewer.
    expect(screen.queryByLabelText('Invite email')).toBeNull();
    expect(screen.queryByLabelText('Role for v@x.com')).toBeNull();
    expect(screen.queryByRole('button', { name: /Remove/i })).toBeNull();
  });
});
