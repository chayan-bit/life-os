// Live presence (issue #150): a lightweight heartbeat + poll. While the tab is
// visible it POSTs /api/presence/ping every 30s (a benign, upserted telemetry
// write - never an outward action) and reads GET /api/presence to learn who
// else is online. Backgrounded tabs stop pinging, so they age out of everyone
// else's presence within the server TTL - exactly the "who is here right now"
// signal we want, without touching the append-only events log.
import { useEffect, useState } from 'react';
import { apiCall } from './api';

const HEARTBEAT_MS = 30000;
const POLL_MS = 30000;

export function usePresence() {
  const [present, setPresent] = useState([]);

  useEffect(() => {
    let cancelled = false;

    const ping = () => {
      // Only announce while the tab is actually in the foreground.
      if (document.visibilityState === 'visible') {
        apiCall('POST', '/api/presence/ping', {});
      }
    };

    const refresh = async () => {
      const { ok, data } = await apiCall('GET', '/api/presence');
      if (!cancelled && ok && data && Array.isArray(data.present)) {
        setPresent(data.present);
      }
    };

    // Announce + read immediately so presence is live on first paint.
    ping();
    refresh();

    const heartbeat = setInterval(ping, HEARTBEAT_MS);
    const poller = setInterval(refresh, POLL_MS);

    // Re-announce the moment a tab is refocused rather than waiting for the
    // next interval, so switching back to the tab feels instant.
    const onVisibility = () => {
      if (document.visibilityState === 'visible') {
        ping();
        refresh();
      }
    };
    document.addEventListener('visibilitychange', onVisibility);

    return () => {
      cancelled = true;
      clearInterval(heartbeat);
      clearInterval(poller);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, []);

  return { present };
}
