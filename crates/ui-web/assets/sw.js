/* RabbitHole app-shell service worker.
 *
 * Checked in as plain JS (no build step, no external code) and copied to the
 * web root by trunk (`index.html`, `data-trunk rel="copy-file"`), so it is
 * registered from `/sw.js` and its default scope covers the whole app.
 * Load-bearing markers in this file are asserted by the host-side shape
 * tests in `crates/ui-web/src/pwa.rs` — keep them in sync.
 *
 * Strategy:
 *  - CACHE_VERSION stamps the one shell cache; bump it to shed every
 *    previously cached shell ("activate" deletes all other versions).
 *  - Same-origin GETs (trunk's content-hashed wasm/js bundles, icons, the
 *    manifest): cache-first with a runtime "fetch-then-cache" fill. Trunk
 *    hashes bundle filenames, so a stale entry can never mask a new build
 *    and no hardcoded precache list exists to drift out of date.
 *  - Navigations: network-first (the entry document is NOT content-hashed,
 *    so the network copy must win when reachable) with the cached shell
 *    document as the offline fallback. Unknown paths also fall back to the
 *    shell: the embedded server only maps "/", so this is what lets
 *    client-side routes (/lobby, /boards, ...) survive a reload.
 *  - /files/ downloads: never touched — straight to the network, never
 *    cached, so a library file can never be served stale.
 */
"use strict";

/* Version stamp for the shell cache. Bump when the caching strategy (or a
 * non-hashed asset like this file's siblings) must be invalidated. */
const CACHE_VERSION = "rabbithole-shell-v1";

/* The download handoff prefix (apps/server/src/http.rs). Requests under it
 * bypass the worker entirely: downloads are never cached, never stale. */
const FILES_PREFIX = "/files/";

/* Best-effort cache write: quota errors and private-mode storage failures
 * are swallowed — caching is an optimization, never a dependency. */
async function stash(key, response) {
  try {
    const cache = await caches.open(CACHE_VERSION);
    await cache.put(key, response);
  } catch (_) {
    /* ignore */
  }
}

/* Cache-first with runtime fill for the hashed bundles and static assets.
 * The stash is deliberately not awaited: cache.put consumes the cloned body
 * stream, and waiting on it would stall streaming wasm compilation. */
async function cacheFirst(request) {
  const hit = await caches.match(request);
  if (hit) {
    return hit;
  }
  const response = await fetch(request);
  if (response.ok && response.type === "basic") {
    stash(request, response.clone());
  }
  return response;
}

/* The shell document, network-first: fetch "/" and refresh the cached copy;
 * offline, answer with whatever shell was cached last. */
async function shellDocument() {
  try {
    const response = await fetch("/");
    if (response.ok) {
      stash("/", response.clone());
      return response;
    }
  } catch (_) {
    /* offline: fall through to the cache */
  }
  const hit = await caches.match("/");
  return hit || Response.error();
}

/* Navigations: serve the network's real answer when it has one; otherwise
 * (offline, or the server's plain 404 for a client-side route) fall back to
 * the shell document so the SPA router can take over. */
async function navigate(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      if (new URL(request.url).pathname === "/") {
        stash("/", response.clone());
      }
      return response;
    }
  } catch (_) {
    /* offline: fall through to the shell */
  }
  return shellDocument();
}

self.addEventListener("install", (event) => {
  /* Seed the shell document so the very first offline visit still boots,
   * then take over without waiting for an older worker to retire. */
  event.waitUntil(
    (async () => {
      try {
        const cache = await caches.open(CACHE_VERSION);
        await cache.add("/");
      } catch (_) {
        /* seeding is best-effort; runtime caching will fill it in */
      }
      await self.skipWaiting();
    })()
  );
});

self.addEventListener("activate", (event) => {
  /* Cache cleanup: delete every cache version except the current one. */
  event.waitUntil(
    (async () => {
      const keys = await caches.keys();
      await Promise.all(
        keys
          .filter((key) => key !== CACHE_VERSION)
          .map((key) => caches.delete(key))
      );
      await self.clients.claim();
    })()
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") {
    return; /* uploads etc. go straight to the network */
  }
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) {
    return; /* cross-origin (e.g. an external radio stream) is not ours */
  }
  if (url.pathname.startsWith(FILES_PREFIX)) {
    return; /* /files/ bypass: downloads are never cached */
  }
  if (request.mode === "navigate") {
    event.respondWith(navigate(request));
    return;
  }
  event.respondWith(cacheFirst(request));
});
