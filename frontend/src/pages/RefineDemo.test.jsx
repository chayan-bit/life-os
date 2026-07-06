// Finding 55: RefineDemo is a proof-of-concept kept in production nav (the
// nav entry itself lives in App.jsx/Layout.jsx, out of scope here) - this
// locks in the demo banner so users aren't misled into thinking it's a
// shipped feature.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import RefineDemo from './RefineDemo';

vi.mock('../lib/api', () => ({ apiCall: vi.fn(), API_BASE: 'http://127.0.0.1:8080' }));
import { apiCall } from '../lib/api';

afterEach(cleanup);

describe('RefineDemo', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: [] });
  });

  it('shows a demo / proof-of-concept banner', async () => {
    render(<RefineDemo />);
    await waitFor(() => expect(screen.getByText(/Demo \/ proof-of-concept/)).toBeTruthy());
  });
});
