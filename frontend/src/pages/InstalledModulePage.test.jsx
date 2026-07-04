// Issue #121: a hot-installed module with a full manifest (entityTypes +
// views) must render through ModuleManifestPage; a minimal
// {id,name,version,icon} registry entry must still fall back to GenericList.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import InstalledModulePage from './InstalledModulePage';

const TEST_ID = 'hot_module';

vi.mock('react-router-dom', async (importOriginal) => ({
  ...(await importOriginal()),
  useParams: () => ({ id: TEST_ID }),
}));

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
vi.mock('../lib/manifestApi', () => ({ fetchInstalledManifest: vi.fn() }));
vi.mock('../lib/moduleRegistry', () => ({ getModule: vi.fn(), registerModule: vi.fn() }));

import { apiCall } from '../lib/api';
import { fetchInstalledManifest } from '../lib/manifestApi';
import { getModule } from '../lib/moduleRegistry';

const FULL_MANIFEST = {
  id: TEST_ID,
  name: 'Hot Module',
  icon: '\u{1F527}',
  entityTypes: { item: { label: 'Item', plural: 'Items', display: { title: 'title' } } },
  views: [{ id: 'list', label: 'List', kind: 'list', type: 'item' }],
};

const MINIMAL_MANIFEST = { id: TEST_ID, name: 'Hot Module', version: '0.0.0-dev', icon: '+' };

afterEach(cleanup);

describe('InstalledModulePage', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: [], offline: false });
    fetchInstalledManifest.mockReset();
    fetchInstalledManifest.mockResolvedValue(null);
  });

  it('renders ModuleManifestPage with a view tab when the registry entry has a full manifest', async () => {
    getModule.mockReturnValue(FULL_MANIFEST);

    render(<InstalledModulePage />);

    await waitFor(() => expect(screen.getByRole('button', { name: 'List' }).getAttribute('data-view-tab')).toBe('list'));
  });

  it('falls back to GenericList when the registry entry is a minimal manifest', async () => {
    getModule.mockReturnValue(MINIMAL_MANIFEST);

    render(<InstalledModulePage />);

    await waitFor(() => expect(screen.getByText(/Hot-installed via self-extension/)).toBeTruthy());
    expect(screen.queryByRole('button', { name: 'List' })).toBeNull();
    expect(fetchInstalledManifest).toHaveBeenCalledWith(TEST_ID);
  });
});
