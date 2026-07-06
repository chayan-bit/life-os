// Finding 53: Database.jsx had three MVP leftovers - (a) a localStorage
// dual-write with a hardcoded workspace_id that duplicated what the real
// API + apiCall's tenant headers already handle, (b) a success toast shown
// even when the create API call FAILED, (c) stray console.log/console.warn
// in production paths. This test covers (b): the create-failure path must
// show an error, never a false success toast.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import DatabaseView from './Database';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const mockApiCall = (overrides = {}) => {
  apiCall.mockImplementation((method, path) => {
    if (path === '/api/health') {
      return Promise.resolve({ ok: true, data: { status: 'healthy' }, offline: false });
    }
    if (method === 'GET' && path.startsWith('/api/entity')) {
      return Promise.resolve({ ok: true, data: [], offline: false });
    }
    if (method === 'GET' && path.startsWith('/api/jobs')) {
      return Promise.resolve({ ok: true, data: [], offline: false });
    }
    if (method === 'POST' && path === '/api/entity') {
      return Promise.resolve(overrides.create || { ok: true, data: { id: 'e1' }, offline: false });
    }
    return Promise.resolve({ ok: true, data: null, offline: false });
  });
};

const renderDatabase = () => render(<DatabaseView />, { wrapper: MemoryRouter });

afterEach(cleanup);

beforeEach(() => {
  apiCall.mockReset();
  localStorage.clear();
});

describe('Database create-entity form', () => {
  it('shows a success toast (not an error) when creation succeeds', async () => {
    mockApiCall({ create: { ok: true, data: { id: 'e1' }, offline: false } });
    renderDatabase();

    fireEvent.click(screen.getByRole('button', { name: /Create & Save Entity/i }));

    await waitFor(() => expect(screen.getByText(/created\./i)).toBeTruthy());
    expect(screen.queryByText(/Failed to create entity/i)).toBeNull();
  });

  it('shows an error (not a success toast) when the create API call fails', async () => {
    mockApiCall({ create: { ok: false, data: null, error: 'Failed to create entity.', offline: false } });
    renderDatabase();

    fireEvent.click(screen.getByRole('button', { name: /Create & Save Entity/i }));

    await waitFor(() => expect(screen.getByText(/Failed to create entity/i)).toBeTruthy());
    expect(screen.queryByText(/created\./i)).toBeNull();
  });

  it('shows an offline error (not a success toast) when the backend is unreachable', async () => {
    mockApiCall({ create: { ok: false, data: null, offline: true } });
    renderDatabase();

    fireEvent.click(screen.getByRole('button', { name: /Create & Save Entity/i }));

    await waitFor(() => expect(screen.getByText(/Cannot reach the Life OS API/i)).toBeTruthy());
    expect(screen.queryByText(/created\./i)).toBeNull();
  });

  it('never writes a local custom-entities copy to localStorage', async () => {
    mockApiCall({ create: { ok: true, data: { id: 'e1' }, offline: false } });
    renderDatabase();

    fireEvent.click(screen.getByRole('button', { name: /Create & Save Entity/i }));

    await waitFor(() => expect(screen.getByText(/created\./i)).toBeTruthy());
    expect(localStorage.getItem('life_os_custom_entities')).toBeNull();
  });
});
