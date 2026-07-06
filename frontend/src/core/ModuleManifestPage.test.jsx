// Issue #121: ModuleManifestPage must expose a `data-view-tab` per declared
// view and mount a `data-view-id` node for whichever view is active - the
// same DOM contract server/validators/render.js's per-view assertion checks
// against a real hot-installed module.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ModuleManifestPage, { KIND_RENDERERS } from './ModuleManifestPage';
import { RENDERER_KINDS } from './rendererKinds';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
import { apiCall } from '../lib/api';

const MANIFEST = {
  id: 'widgets',
  name: 'Widgets',
  icon: '\u{1F527}',
  entityTypes: {
    item: { label: 'Item', plural: 'Items', display: { title: 'title' } },
  },
  views: [
    { id: 'list', label: 'List', kind: 'list', type: 'item' },
    { id: 'board', label: 'Board', kind: 'board', type: 'item', columns: ['todo', 'done'], groupBy: 'status' },
  ],
};

afterEach(cleanup);

describe('ModuleManifestPage', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: [{ id: 'e1', title: 'Thing', status: 'todo' }], offline: false });
  });

  it('renders a tab per declared view and mounts data-view-id for the active view', async () => {
    render(<ModuleManifestPage manifest={MANIFEST} />);

    await waitFor(() => expect(screen.getByText('Thing')).toBeTruthy());

    expect(screen.getByRole('button', { name: 'List' }).getAttribute('data-view-tab')).toBe('list');
    expect(screen.getByRole('button', { name: 'Board' }).getAttribute('data-view-tab')).toBe('board');
    expect(document.querySelector('[data-view-id="list"]')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Board' }));

    await waitFor(() => expect(document.querySelector('[data-view-id="board"]')).toBeTruthy());
  });

  // Finding 52: a non-offline API error used to fall through to an empty
  // entities array and still render 'ready', masking the failure as "no
  // items" instead of surfacing it.
  it('shows an error message (not an empty view) when the entity fetch fails', async () => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: false, data: null, error: 'boom', offline: false });

    render(<ModuleManifestPage manifest={MANIFEST} />);

    await waitFor(() => expect(screen.getByText(/Failed to load this view/i)).toBeTruthy());
  });
});

// Drift guard (T1, issue #133): rendererKinds.js is the plain-JS source of
// truth the server-side structural validator imports directly (it can't
// import KIND_RENDERERS itself - that map needs React/JSX), so the two lists
// must always describe the same set of kinds.
describe('KIND_RENDERERS / RENDERER_KINDS drift guard', () => {
  it('registers exactly the same kinds as rendererKinds.js', () => {
    expect(Object.keys(KIND_RENDERERS).sort()).toEqual([...RENDERER_KINDS].sort());
  });
});
