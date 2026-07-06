// Life OS service worker (issue #103, #151, docs/PLATFORM-SYSTEMS.md).
// Caches the app shell + same-origin static assets so the SPA loads offline.
//
// /api/* is NETWORK-ONLY (security+offline audit findings #8, #9). Tenant
// identity travels in headers (X-Workspace-Id, Authorization), never in the
// URL, so a cache keyed on URL+method can't tell workspaces or logged-in
// users apart - caching authenticated responses served workspace A's data
// back to workspace B after a switch/re-login, and logout never purged it
// (finding #8, a cross-tenant data leak). The same cache-first behavior also
// masked real backend outages: once /api/health returned one 200, the app's
// 15s poll would read that stale cached response forever (finding #9b). So
// every /api/* GET goes straight to the network, full stop - reads stay
// "free" (docs/SECURITY.md §1) via the network, never via a shared cache.
//
// Static same-origin assets (Vite-hashed JS/CSS under /assets/, icons, the
// manifest) ARE cached: precached at install for the shell, and populated at
// runtime on first fetch for hashed bundles Vite names at build time (this
// static file can't know those hashes in advance). That's what makes offline
// navigation actually work - previously the shell HTML was cached but its
// <script src> was not, so an offline reload served a shell that 404'd on
// its own bundle (finding #9a). CACHE_VERSION is bumped on every change here
// so `activate` purges stale shell/asset caches from prior deploys.

const CACHE_VERSION = 'v2';
const SHELL_CACHE = `lifeos-shell-${CACHE_VERSION}`;
const ASSET_CACHE = `lifeos-assets-${CACHE_VERSION}`;
const CURRENT_CACHES = [SHELL_CACHE, ASSET_CACHE];
const SHELL_URLS = ['/', '/dashboard', '/manifest.webmanifest', '/icon.svg', '/icon-192.png', '/icon-512.png', '/apple-touch-icon.png'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(SHELL_CACHE).then((cache) => cache.addAll(SHELL_URLS)).then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => !CURRENT_CACHES.includes(k)).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const { request } = event;
  if (request.method !== 'GET') return; // writes are never cached/served from cache

  const url = new URL(request.url);

  if (url.pathname.startsWith('/api/')) {
    // Network-only - see the file header. No cache.match, no cache.put, no
    // exceptions (this covers /api/health too: it must never come from cache).
    event.respondWith(fetch(request));
    return;
  }

  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request).catch(() => caches.match('/dashboard').then((r) => r || caches.match('/')))
    );
    return;
  }

  if (url.origin === self.location.origin) {
    // Same-origin static asset: cache-first, and populate the runtime cache
    // on first fetch so it survives to the next offline navigation.
    event.respondWith(
      caches.match(request).then((cached) => {
        if (cached) return cached;
        return fetch(request).then((response) => {
          if (response.ok) {
            const copy = response.clone();
            caches.open(ASSET_CACHE).then((cache) => cache.put(request, copy));
          }
          return response;
        });
      })
    );
    return;
  }

  // Cross-origin GET (fonts, third-party widgets): pass through, never cached.
  event.respondWith(fetch(request));
});

self.addEventListener('push', (event) => {
  const data = event.data ? event.data.json() : { title: 'Life OS', body: 'New activity.' };
  event.waitUntil(
    self.registration.showNotification(data.title || 'Life OS', {
      body: data.body || '',
      icon: '/icon-192.png',
      // Mirrors the Telegram digest's alert-red gating (docs/PLATFORM-SYSTEMS.md) -
      // the payload carries the same urgency tag the digest uses.
      tag: data.tag || 'lifeos-digest',
      // Deep-link the click handler below reads (issue #151). Defaults to the
      // dashboard so a payload with no url still opens somewhere sensible.
      data: { url: data.url || '/dashboard' },
    })
  );
});

// Web push click deep-link (issue #151): focuses an already-open PWA window
// and navigates it to the notification's url, or opens a new one. Reuses an
// existing client instead of always opening a new tab/window, matching how a
// normal app notification (e.g. a Telegram deep link) behaves.
self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const url = event.notification.data && event.notification.data.url ? event.notification.data.url : '/dashboard';
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((clients) => {
      for (const client of clients) {
        if ('focus' in client) {
          const navigated = 'navigate' in client ? client.navigate(url).catch(() => client) : Promise.resolve(client);
          return navigated.then((c) => c.focus());
        }
      }
      return self.clients.openWindow(url);
    })
  );
});
