// Issue #150: the presence hook must announce the current user (heartbeat ping)
// and read the roster on mount while the tab is visible.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, renderHook, waitFor } from '@testing-library/react';

vi.mock('./api', () => ({ apiCall: vi.fn() }));
import { apiCall } from './api';
import { usePresence } from './usePresence';

afterEach(cleanup);

describe('usePresence', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({
      ok: true,
      data: { ttl_secs: 120, present: [{ user_id: 'usr_1', last_seen: 1 }] },
    });
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => 'visible',
    });
  });

  it('fires a heartbeat ping and reads the roster on mount', async () => {
    const { result } = renderHook(() => usePresence());

    await waitFor(() =>
      expect(apiCall).toHaveBeenCalledWith('POST', '/api/presence/ping', {}),
    );
    expect(apiCall).toHaveBeenCalledWith('GET', '/api/presence');
    await waitFor(() => expect(result.current.present).toHaveLength(1));
    expect(result.current.present[0].user_id).toBe('usr_1');
  });

  it('does not ping while the tab is hidden', async () => {
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => 'hidden',
    });
    renderHook(() => usePresence());

    // The roster read still happens, but no heartbeat is announced.
    await waitFor(() => expect(apiCall).toHaveBeenCalledWith('GET', '/api/presence'));
    expect(apiCall).not.toHaveBeenCalledWith('POST', '/api/presence/ping', {});
  });
});
