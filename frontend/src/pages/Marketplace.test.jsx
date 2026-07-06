// Marketplace UI tests (issue #147): browse/search, detail drawer with a
// signature badge + version history, validated install (success and the honest
// validation-failure surfacing), and the gated publish-from-installed flow.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import Marketplace from './Marketplace';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const PKG = {
  id: 'pkg_1',
  module_id: 'reading',
  version: '1.0.0',
  manifest: {
    id: 'reading', name: 'Reading', version: '1.0.0',
    entityTypes: { book: { label: 'Book', plural: 'Books', icon: 'Book', attrs: {} } },
    views: [{ id: 'all', label: 'All', kind: 'list', type: 'book' }],
  },
  signature: 'sig-abc',
  publisher_pubkey: 'pubkey0123456789',
};

const INSTALLED_MODULE = {
  id: 'ent_manifest', module: 'system', type: 'module_manifest',
  title: 'module_manifest_reading', attrs: PKG.manifest,
};

// Routes apiCall by (method, path); per-test overrides win.
function mockApi(overrides = {}) {
  apiCall.mockImplementation((method, path) => {
    const key = `${method} ${path}`;
    for (const [pattern, value] of Object.entries(overrides)) {
      if (key.startsWith(pattern)) return Promise.resolve(value);
    }
    if (key.startsWith('GET /api/marketplace/packages')) return Promise.resolve({ ok: true, data: { packages: [PKG] } });
    if (key.startsWith('GET /api/entity?module=system')) return Promise.resolve({ ok: true, data: [INSTALLED_MODULE] });
    if (key.startsWith('POST /api/marketplace/verify')) return Promise.resolve({ ok: true, data: { valid: true } });
    if (key.startsWith('GET /api/marketplace/package/')) return Promise.resolve({ ok: true, data: { versions: [{ id: 'pkg_1', version: '1.0.0' }, { id: 'pkg_0', version: '0.9.0' }] } });
    if (key.startsWith('POST /api/marketplace/install')) return Promise.resolve({ ok: true, data: { installed: true } });
    if (key.startsWith('POST /api/entity')) return Promise.resolve({ ok: true, data: { id: 'ent_draft' } });
    return Promise.resolve({ ok: true, data: null });
  });
}

afterEach(cleanup);
beforeEach(() => apiCall.mockReset());

describe('Marketplace browse', () => {
  it('lists published packages', async () => {
    mockApi();
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
  });

  it('shows an empty state when nothing is published', async () => {
    mockApi({ 'GET /api/marketplace/packages': { ok: true, data: { packages: [] } } });
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByText(/No packages published yet/)).toBeTruthy());
  });

  it('surfaces a load error', async () => {
    mockApi({ 'GET /api/marketplace/packages': { ok: false, error: 'boom' } });
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByText('boom')).toBeTruthy());
  });

  it('filters packages by the search query', async () => {
    mockApi();
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
    fireEvent.change(screen.getByLabelText('Search packages'), { target: { value: 'trading' } });
    await waitFor(() => expect(screen.getByText(/No packages match your search/)).toBeTruthy());
  });
});

describe('Marketplace detail drawer', () => {
  it('opens with the signature verified badge and version history', async () => {
    mockApi();
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /reading@1\.0\.0/ }));

    await waitFor(() => expect(screen.getByText('Signature verified')).toBeTruthy());
    expect(screen.getByText('Version history')).toBeTruthy();
    // Two versions listed (current + an older one for rollback).
    expect(screen.getByText('0.9.0')).toBeTruthy();
    expect(screen.getByRole('button', { name: /Roll back/ })).toBeTruthy();
  });

  it('shows an invalid signature badge when verify fails', async () => {
    mockApi({ 'POST /api/marketplace/verify': { ok: true, data: { valid: false } } });
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /reading@1\.0\.0/ }));
    await waitFor(() => expect(screen.getByText('Signature invalid')).toBeTruthy());
  });
});

describe('Marketplace install', () => {
  it('installs and confirms activation on success', async () => {
    mockApi();
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /reading@1\.0\.0/ }));
    await waitFor(() => expect(screen.getByText('Version history')).toBeTruthy());

    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: /^Install/ }));

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/marketplace/install', { package_id: 'pkg_1' }),
    );
    await waitFor(() => expect(screen.getByText(/Installed - validated and activated/)).toBeTruthy());
  });

  it('surfaces the validation failure when the server rejects the bundle', async () => {
    mockApi({ 'POST /api/marketplace/install': { ok: false, error: "package failed validation: (root) must have required property 'name'" } });
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByRole('button', { name: /reading@1\.0\.0/ })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /reading@1\.0\.0/ }));
    await waitFor(() => expect(screen.getByText('Version history')).toBeTruthy());

    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: /^Install/ }));

    await waitFor(() => expect(screen.getByText(/package failed validation/)).toBeTruthy());
  });
});

// Finding 56: the detail drawer is a modal-style overlay - it needs Escape
// to close and to manage focus (into the drawer on open, back to the
// triggering row on close), not just a close button.
describe('Marketplace detail drawer accessibility', () => {
  it('closes on Escape and returns focus to the package that opened it', async () => {
    mockApi();
    render(<Marketplace />);
    const trigger = await screen.findByRole('button', { name: /reading@1\.0\.0/ });
    fireEvent.click(trigger);

    await waitFor(() => expect(screen.getByRole('dialog')).toBeTruthy());
    fireEvent.keyDown(window, { key: 'Escape' });

    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(trigger);
  });

  it('focuses the close button when the drawer opens', async () => {
    mockApi();
    render(<Marketplace />);
    fireEvent.click(await screen.findByRole('button', { name: /reading@1\.0\.0/ }));

    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close details' })));
  });
});

describe('Marketplace gated publish', () => {
  it('creates a pending-approval draft instead of publishing directly', async () => {
    mockApi();
    render(<Marketplace />);
    await waitFor(() => expect(screen.getByLabelText('Module to publish')).toBeTruthy());

    fireEvent.change(screen.getByLabelText('Module to publish'), { target: { value: 'reading' } });
    fireEvent.click(screen.getByRole('button', { name: /Request publish/ }));

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/entity', expect.objectContaining({
        module: 'bot', type: 'draft', status: 'pending_approval',
        attrs: expect.objectContaining({ kind: 'marketplace_publish', module_id: 'reading' }),
      })),
    );
    await waitFor(() => expect(screen.getByText(/sent for approval/)).toBeTruthy());
  });
});
