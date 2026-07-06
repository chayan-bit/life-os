// Finding 24: this page (the "Media Ingest" tab of Storage.jsx) used to
// render a hardcoded "Universal Version History" list, canned per-type
// diffs, and a hardcoded semantic-search clip list, all presented as if
// backed by the real API. This locks in the honest replacements:
// CommittedFilesPanel reads the real listFileEntities() and
// SegmentSearchPanel calls the real GET /api/search endpoint, both falling
// back to an honest empty state instead of fabricated rows.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import VcsIngest from './VcsIngest';

vi.mock('../lib/api', () => ({ apiCall: vi.fn() }));
vi.mock('../lib/vcsApi', () => ({ listFileEntities: vi.fn() }));

import { apiCall } from '../lib/api';
import { listFileEntities } from '../lib/vcsApi';

afterEach(cleanup);

describe('VcsIngest committed files panel', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: {} });
    listFileEntities.mockReset();
  });

  it('shows an honest empty state instead of canned version history when nothing is committed', async () => {
    listFileEntities.mockResolvedValue([]);
    render(<VcsIngest />);

    await waitFor(() => expect(screen.getByText('No files committed to lifeos-vcs yet.')).toBeTruthy());
    // The old fabricated rows must be gone for good.
    expect(screen.queryByText('Universal Version History')).toBeNull();
    expect(screen.queryByText(/dashboard_mockup\.fig/)).toBeNull();
    expect(screen.queryByText(/Per-Type Semantic Diff Explorer/)).toBeNull();
  });

  it('renders a real, honest commit-status badge sourced from the entity blob_ref', async () => {
    listFileEntities.mockResolvedValue([
      { id: 'ent_1', attrs: { name: 'notes.txt' }, blob_ref: 'b3_abc', updated_at: 1735689600 },
      { id: 'ent_2', attrs: { name: 'draft.txt' }, blob_ref: null, updated_at: 1735689600 },
    ]);
    render(<VcsIngest />);

    await waitFor(() => expect(screen.getByText('COMMITTED')).toBeTruthy());
    expect(screen.getByText('NOT COMMITTED')).toBeTruthy();
  });
});

describe('VcsIngest semantic voice search', () => {
  beforeEach(() => {
    apiCall.mockReset();
    listFileEntities.mockReset();
    listFileEntities.mockResolvedValue([]);
  });

  it('calls the real GET /api/search endpoint and renders real segment text, filtering non-segment hits', async () => {
    apiCall.mockImplementation((method, path) => {
      if (String(path).startsWith('/api/search')) {
        return Promise.resolve({
          ok: true,
          data: {
            results: [
              {
                id: 'segment_abc', type: 'segment', module: 'files',
                attrs: { text: 'the Nango proxy holds credentials', t_start: 4.5, t_end: 9.2 },
                score: 0.87,
              },
              { id: 'ent_other', type: 'file', module: 'files', attrs: {}, score: 0.1 },
            ],
          },
        });
      }
      return Promise.resolve({ ok: true, data: {} });
    });

    render(<VcsIngest />);
    fireEvent.change(screen.getByLabelText('Search transcribed segments'), { target: { value: 'nango' } });

    await waitFor(
      () => expect(screen.getByText(/the Nango proxy holds credentials/)).toBeTruthy(),
      { timeout: 2000 },
    );
    // The non-segment hit is filtered out, not rendered as a fake clip.
    expect(screen.queryByText('ent_other')).toBeNull();

    const call = apiCall.mock.calls.find((c) => String(c[1]).startsWith('/api/search'));
    expect(call[0]).toBe('GET');
    expect(call[1]).toContain('module=files');
    expect(call[1]).toContain('nango');
  });

  it('shows an honest empty state instead of canned clips when nothing matches', async () => {
    apiCall.mockImplementation((method, path) => {
      if (String(path).startsWith('/api/search')) return Promise.resolve({ ok: true, data: { results: [] } });
      return Promise.resolve({ ok: true, data: {} });
    });

    render(<VcsIngest />);
    fireEvent.change(screen.getByLabelText('Search transcribed segments'), { target: { value: 'zzz-no-match' } });

    await waitFor(
      () => expect(screen.getByText('No matching transcript segments found.')).toBeTruthy(),
      { timeout: 2000 },
    );
  });
});
