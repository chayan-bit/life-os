// Regression coverage for the two service-worker findings fixed alongside
// this file (security+offline audit #8, #9): /api/* must never touch the
// Cache API (cross-tenant bleed + stale-after-logout + masked health checks),
// same-origin static assets must be cache-first with runtime population (so
// offline navigation's <script src> actually resolves), and the #151
// notificationclick deep-link behavior must survive the rewrite untouched.
//
// sw.js is a plain (non-module) script meant to run as a ServiceWorkerGlobal-
// Scope, so it can't be `import`ed directly in a jsdom test. Instead we read
// its source and execute it as a Function body with `self`/`caches`/`fetch`
// passed in as parameters - inside the script, bare references to `self`,
// `caches`, and `fetch` then resolve to our mocks instead of any real global,
// giving a fully sandboxed, dependency-free harness.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it, vi } from 'vitest';

const swSource = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), 'sw.js'), 'utf8');

function flushMicrotasks() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function loadServiceWorker({ caches, fetch }) {
  const listeners = {};
  const self = {
    addEventListener: (type, handler) => {
      listeners[type] = handler;
    },
    skipWaiting: vi.fn(),
    clients: { claim: vi.fn(), matchAll: vi.fn().mockResolvedValue([]), openWindow: vi.fn() },
    registration: { showNotification: vi.fn() },
    location: { origin: 'https://app.example.test' },
  };
  // eslint-disable-next-line no-new-func -- sandboxed SW harness, see file header
  const run = new Function('self', 'caches', 'fetch', swSource);
  run(self, caches, fetch);
  return { listeners, self };
}

function makeCachesMock({ matchResult = undefined, cacheKeys = [] } = {}) {
  const cachePut = vi.fn().mockResolvedValue(undefined);
  const cache = { match: vi.fn().mockResolvedValue(matchResult), put: cachePut, addAll: vi.fn().mockResolvedValue(undefined) };
  return {
    open: vi.fn().mockResolvedValue(cache),
    match: vi.fn().mockResolvedValue(matchResult),
    keys: vi.fn().mockResolvedValue(cacheKeys),
    delete: vi.fn().mockResolvedValue(true),
    _cache: cache,
  };
}

function makeFetchEvent(request) {
  return { request, respondWith: vi.fn(), waitUntil: vi.fn() };
}

describe('service worker: /api/* is network-only (finding #8, #9b)', () => {
  it('never touches the Cache API for an /api/* GET', async () => {
    const caches = makeCachesMock();
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'GET', url: 'https://app.example.test/api/entity?workspace=ws-1' };
    const event = makeFetchEvent(request);
    listeners.fetch(event);
    await flushMicrotasks();

    expect(fetch).toHaveBeenCalledWith(request);
    expect(event.respondWith).toHaveBeenCalledTimes(1);
    expect(caches.open).not.toHaveBeenCalled();
    expect(caches.match).not.toHaveBeenCalled();
    expect(caches._cache.put).not.toHaveBeenCalled();
  });

  it('does not special-case /api/health back into caching', async () => {
    const caches = makeCachesMock();
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'GET', url: 'https://app.example.test/api/health' };
    listeners.fetch(makeFetchEvent(request));
    await flushMicrotasks();

    expect(fetch).toHaveBeenCalledWith(request);
    expect(caches.open).not.toHaveBeenCalled();
    expect(caches._cache.put).not.toHaveBeenCalled();
  });

  it('does not run any cache logic for a non-GET /api/* write', () => {
    const caches = makeCachesMock();
    const fetch = vi.fn();
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'POST', url: 'https://app.example.test/api/entity' };
    const event = makeFetchEvent(request);
    listeners.fetch(event);

    expect(event.respondWith).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
  });
});

describe('service worker: same-origin static assets are cache-first + runtime-populated (finding #9a)', () => {
  it('fetches and populates the runtime asset cache on a cache miss', async () => {
    const caches = makeCachesMock({ matchResult: undefined });
    const response = { ok: true, clone: vi.fn().mockReturnValue({ ok: true, cloned: true }) };
    const fetch = vi.fn().mockResolvedValue(response);
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'GET', url: 'https://app.example.test/assets/index-abc123.js' };
    listeners.fetch(makeFetchEvent(request));
    await flushMicrotasks();
    await flushMicrotasks();

    expect(fetch).toHaveBeenCalledWith(request);
    expect(caches.open).toHaveBeenCalledWith('lifeos-assets-v2');
    expect(caches._cache.put).toHaveBeenCalledWith(request, { ok: true, cloned: true });
  });

  it('serves the cached asset without hitting the network on a cache hit', async () => {
    const cachedResponse = { ok: true, cached: true };
    const caches = makeCachesMock({ matchResult: cachedResponse });
    const fetch = vi.fn();
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'GET', url: 'https://app.example.test/assets/index-abc123.js' };
    listeners.fetch(makeFetchEvent(request));
    await flushMicrotasks();

    expect(fetch).not.toHaveBeenCalled();
  });

  it('passes cross-origin GETs straight through without touching the cache', async () => {
    const caches = makeCachesMock();
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    const { listeners } = loadServiceWorker({ caches, fetch });

    const request = { method: 'GET', url: 'https://cdn.example.net/font.woff2' };
    listeners.fetch(makeFetchEvent(request));
    await flushMicrotasks();

    expect(fetch).toHaveBeenCalledWith(request);
    expect(caches.open).not.toHaveBeenCalled();
    expect(caches.match).not.toHaveBeenCalled();
  });
});

describe('service worker: activate purges stale caches on every version bump', () => {
  it('deletes caches not in the current shell/asset set and claims clients', async () => {
    const caches = makeCachesMock({
      cacheKeys: ['lifeos-shell-v1', 'lifeos-api-v1', 'lifeos-shell-v2', 'lifeos-assets-v2'],
    });
    const { listeners, self } = loadServiceWorker({ caches, fetch: vi.fn() });

    const event = { waitUntil: vi.fn() };
    listeners.activate(event);
    expect(event.waitUntil).toHaveBeenCalledTimes(1);
    await event.waitUntil.mock.calls[0][0];

    expect(caches.delete).toHaveBeenCalledWith('lifeos-shell-v1');
    expect(caches.delete).toHaveBeenCalledWith('lifeos-api-v1');
    expect(caches.delete).not.toHaveBeenCalledWith('lifeos-shell-v2');
    expect(caches.delete).not.toHaveBeenCalledWith('lifeos-assets-v2');
    expect(self.clients.claim).toHaveBeenCalled();
  });
});

describe('service worker: #151 notificationclick deep-link is preserved', () => {
  it('focuses and navigates an existing window client to the notification url', async () => {
    const caches = makeCachesMock();
    const { listeners, self } = loadServiceWorker({ caches, fetch: vi.fn() });

    const client = { focus: vi.fn() };
    client.navigate = vi.fn().mockResolvedValue(client);
    self.clients.matchAll.mockResolvedValue([client]);

    const event = {
      notification: { close: vi.fn(), data: { url: '/dashboard/agents/42' } },
      waitUntil: vi.fn(),
    };
    listeners.notificationclick(event);
    await event.waitUntil.mock.calls[0][0];

    expect(event.notification.close).toHaveBeenCalled();
    expect(client.navigate).toHaveBeenCalledWith('/dashboard/agents/42');
    expect(client.focus).toHaveBeenCalled();
  });

  it('opens a new window at the deep-link url when no client is open', async () => {
    const caches = makeCachesMock();
    const { listeners, self } = loadServiceWorker({ caches, fetch: vi.fn() });
    self.clients.matchAll.mockResolvedValue([]);

    const event = { notification: { close: vi.fn(), data: { url: '/dashboard/agents/42' } }, waitUntil: vi.fn() };
    listeners.notificationclick(event);
    await event.waitUntil.mock.calls[0][0];

    expect(self.clients.openWindow).toHaveBeenCalledWith('/dashboard/agents/42');
  });

  it('defaults to /dashboard when the notification carries no url', async () => {
    const caches = makeCachesMock();
    const { listeners, self } = loadServiceWorker({ caches, fetch: vi.fn() });
    self.clients.matchAll.mockResolvedValue([]);

    const event = { notification: { close: vi.fn(), data: null }, waitUntil: vi.fn() };
    listeners.notificationclick(event);
    await event.waitUntil.mock.calls[0][0];

    expect(self.clients.openWindow).toHaveBeenCalledWith('/dashboard');
  });
});
