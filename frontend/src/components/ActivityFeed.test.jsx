// Issue #150: the activity feed seeds from the event backlog and tails new
// events over an SSE-via-fetch stream (EventSource can't carry auth headers).
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';

vi.mock('../lib/api', () => ({
  apiCall: vi.fn(),
  authHeaders: () => ({ 'X-Workspace-Id': 'ws', Authorization: 'Bearer t' }),
  API_BASE: 'http://test.local',
}));
import { apiCall } from '../lib/api';
import ActivityFeed from './ActivityFeed';

// A ReadableStream that emits the given SSE frames (as bytes) then closes.
function sseStream(frames) {
  const encoder = new TextEncoder();
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i < frames.length) {
        controller.enqueue(encoder.encode(frames[i]));
        i += 1;
      } else {
        controller.close();
      }
    },
  });
}

const nowSec = () => Math.floor(Date.now() / 1000);

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('ActivityFeed', () => {
  beforeEach(() => {
    apiCall.mockReset();
  });

  it('renders the backlog then appends a streamed event', async () => {
    apiCall.mockResolvedValueOnce({
      ok: true,
      data: [
        { id: 'evt_b1', type: 'entity.created', actor: 'user', entity_id: 'ent_x', ts: nowSec() },
      ],
    });

    const streamed = {
      id: 'evt_s1',
      type: 'entity.updated',
      actor: 'bob',
      entity_id: 'ent_y',
      ts: nowSec(),
    };
    const frame = `id: evt_s1\nevent: entity.updated\ndata: ${JSON.stringify(streamed)}\n\n`;
    global.fetch = vi.fn().mockResolvedValue({ ok: true, body: sseStream([frame]) });

    render(<ActivityFeed />);

    // Backlog event shows first...
    await waitFor(() => expect(screen.getByText('entity.created')).toBeTruthy());
    // ...then the streamed event is appended live.
    await waitFor(() => expect(screen.getByText('entity.updated')).toBeTruthy());

    // The stream is opened via fetch (not EventSource), with the SSE Accept header.
    expect(global.fetch).toHaveBeenCalledWith(
      'http://test.local/api/events/stream',
      expect.objectContaining({
        headers: expect.objectContaining({ Accept: 'text/event-stream' }),
      }),
    );
  });

  it('shows an empty state when the backlog is empty and no events stream', async () => {
    apiCall.mockResolvedValueOnce({ ok: true, data: [] });
    // A stream that opens but never yields a frame.
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      body: new ReadableStream({ start() {} }),
    });

    render(<ActivityFeed />);

    await waitFor(() => expect(screen.getByText(/No recent activity/i)).toBeTruthy());
  });
});
