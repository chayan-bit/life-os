// Finding 12: content-addressed blob_refs used to always render the
// ImageOff placeholder because GET /api/vcs/blob (frontend/src/lib/vcsApi.js
// ::fetchBlob) went unused here. These tests cover both media-resolution
// paths: a non-http blob_ref fetched and turned into an object URL, and a
// direct http(s) URL passed through untouched.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/react';
import GenericGallery from './GenericGallery';

vi.mock('../../lib/vcsApi', () => ({ fetchBlob: vi.fn() }));
import { fetchBlob } from '../../lib/vcsApi';

afterEach(cleanup);

beforeEach(() => {
  fetchBlob.mockReset();
  URL.createObjectURL = vi.fn(() => 'blob:mock-object-url');
  URL.revokeObjectURL = vi.fn();
});

describe('GenericGallery', () => {
  it('shows the empty label when there are no entities', () => {
    const { getByText } = render(<GenericGallery entities={[]} emptyLabel="Nothing here." />);
    expect(getByText('Nothing here.')).toBeTruthy();
  });

  it('resolves a non-http blob_ref through fetchBlob into an object URL', async () => {
    fetchBlob.mockResolvedValue(new Uint8Array([1, 2, 3]));
    const entity = { id: 'e1', title: 'Design shot', attrs: { blob_ref: 'blake3:abc123', mime: 'image/png' } };

    const { container } = render(<GenericGallery entities={[entity]} />);

    await waitFor(() => expect(container.querySelector('img')).toBeTruthy());
    expect(fetchBlob).toHaveBeenCalledWith('blake3:abc123');
    expect(container.querySelector('img').getAttribute('src')).toBe('blob:mock-object-url');
  });

  it('revokes the object URL on unmount', async () => {
    fetchBlob.mockResolvedValue(new Uint8Array([1, 2, 3]));
    const entity = { id: 'e1', title: 'Design shot', attrs: { blob_ref: 'blake3:abc123' } };

    const { container, unmount } = render(<GenericGallery entities={[entity]} />);
    await waitFor(() => expect(container.querySelector('img')).toBeTruthy());

    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:mock-object-url');
  });

  it('passes an http(s) ref straight through without calling fetchBlob', () => {
    const entity = { id: 'e2', title: 'Hosted asset', attrs: { blob_ref: 'https://cdn.example.com/x.png' } };

    const { container } = render(<GenericGallery entities={[entity]} />);

    expect(fetchBlob).not.toHaveBeenCalled();
    expect(container.querySelector('img').getAttribute('src')).toBe('https://cdn.example.com/x.png');
  });

  it('renders the ImageOff placeholder when there is no media ref', () => {
    const entity = { id: 'e3', title: 'No media' };
    const { container } = render(<GenericGallery entities={[entity]} />);
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('svg')).toBeTruthy();
  });
});
