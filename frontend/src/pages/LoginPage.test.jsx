// Finding 21: registration minted a valid session but never set the
// life_os_loggedin flag the login success path sets, so a brand-new user
// bounced back to the login screen on first reload. This test asserts the
// registration success path sets the exact same localStorage keys login does.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import LoginPage from './LoginPage';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual('../lib/api');
  return { ...actual, apiCall: vi.fn() };
});
import { apiCall, WORKSPACE_ID_KEY, KEY_TOKEN_KEY, REFRESH_TOKEN_KEY } from '../lib/api';

afterEach(cleanup);

beforeEach(() => {
  apiCall.mockReset();
  localStorage.clear();
});

describe('LoginPage registration', () => {
  it('sets the logged-in flag (and session keys) on successful registration, matching login', async () => {
    apiCall.mockResolvedValueOnce({
      ok: true,
      data: { workspace_id: 'ws_new', key_token: 'kt_1', refresh_token: 'rt_1' },
      offline: false,
    });

    render(<LoginPage onLogin={() => {}} />);

    fireEvent.click(screen.getByRole('button', { name: /Register Workspace/i }));
    fireEvent.change(screen.getByLabelText(/Full Name/i), { target: { value: 'Chayan' } });
    fireEvent.change(screen.getByLabelText(/Email Address/i), { target: { value: 'chayan@example.com' } });
    fireEvent.change(screen.getByLabelText(/Password/i), { target: { value: 'a-real-password' } });
    fireEvent.change(screen.getByLabelText(/Workspace Name/i), { target: { value: "Chayan's Brain" } });
    fireEvent.click(screen.getByRole('button', { name: /Scaffold Tenant Workspace/i }));

    await waitFor(() => expect(screen.getByText(/Workspace Scaffolded Successfully/i)).toBeTruthy());

    expect(localStorage.getItem('life_os_loggedin')).toBe('true');
    expect(localStorage.getItem('life_os_user_email')).toBe('chayan@example.com');
    expect(localStorage.getItem(WORKSPACE_ID_KEY)).toBe('ws_new');
    expect(localStorage.getItem(KEY_TOKEN_KEY)).toBe('kt_1');
    expect(localStorage.getItem(REFRESH_TOKEN_KEY)).toBe('rt_1');
  });

  it('does not set the logged-in flag when registration fails', async () => {
    apiCall.mockResolvedValueOnce({ ok: false, data: null, error: 'Email already registered.', offline: false });

    render(<LoginPage onLogin={() => {}} />);

    fireEvent.click(screen.getByRole('button', { name: /Register Workspace/i }));
    fireEvent.change(screen.getByLabelText(/Full Name/i), { target: { value: 'Chayan' } });
    fireEvent.change(screen.getByLabelText(/Email Address/i), { target: { value: 'chayan@example.com' } });
    fireEvent.change(screen.getByLabelText(/Password/i), { target: { value: 'a-real-password' } });
    fireEvent.change(screen.getByLabelText(/Workspace Name/i), { target: { value: "Chayan's Brain" } });
    fireEvent.click(screen.getByRole('button', { name: /Scaffold Tenant Workspace/i }));

    await waitFor(() => expect(screen.getByText(/Email already registered/i)).toBeTruthy());
    expect(localStorage.getItem('life_os_loggedin')).toBeNull();
  });
});
