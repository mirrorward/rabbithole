/* RabbitHole's cached web shell. A complete document + its hashed bundles
 * are promoted together. Existing tabs retain their old hashed assets;
 * installing an update never reloads a page or deletes its working cache. */
"use strict";

const CACHE_VERSION = "rabbithole-shell-v2";
const FILES_PREFIX = "/files/";
const CACHE_PREFIX = "rabbithole-shell-v";
const STATIC_ASSETS = ["/logo.png", "/icon-192.png", "/icon-512.png",
  "/space-grotesk-latin.woff2", "/manifest.webmanifest"];
let preparing;

function bundles(html) {
  return [...new Set(html.match(/\/rabbithole-ui-web-[a-f0-9]+(?:_bg)?\.(?:js|wasm)\b/g) || [])];
}

function buildOf(html) {
  return bundles(html).find((url) => url.endsWith(".js")) || "";
}

async function network(request) {
  // A hung app host must not strand an otherwise usable saved shell.
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 10_000);
  try {
    const response = await fetch(request, { cache: "no-store", signal: controller.signal });
    // Keep the deadline through body consumption, not only response headers.
    const body = await response.arrayBuffer();
    return new Response([204, 205, 304].includes(response.status) ? null : body, { status: response.status, statusText: response.statusText, headers: response.headers });
  } finally {
    clearTimeout(timer);
  }
}

function validBundle(url, response) {
  const type = response.headers.get("content-type") || "";
  return response.ok && (new URL(url, self.location.origin).pathname.endsWith(".wasm") ? type.includes("application/wasm") : type.includes("javascript"));
}

/* Coalesce checks from tabs and navigation. Crucially, put '/' LAST: a
 * failed bundle fetch, quota error, or partial deploy keeps the prior shell.
 * Hashes are immutable, so retaining old files also supports old open tabs
 * after another tab explicitly activates a newer worker. */
async function prepareShell(response) {
  const html = await response.clone().text();
  const assets = bundles(html);
  if (!response.ok || !assets.some((url) => url.endsWith(".wasm")) || !buildOf(html)) {
    throw new Error("The app shell is incomplete");
  }
  const cache = await caches.open(CACHE_VERSION);
  // Bundle bodies finish caching before the document is promoted. Auxiliary
  // icons/fonts are best effort; their failure must not discard a usable app.
  for (const url of assets) {
    const hit = await cache.match(url);
    if (hit && validBundle(url, hit)) continue;
    if (hit) await cache.delete(url);
    const asset = await network(url);
    if (!validBundle(url, asset)) {
      throw new Error(`Incomplete app bundle: ${url}`);
    }
    await cache.put(url, asset);
  }
  await Promise.all(STATIC_ASSETS.map(async (url) => {
    try {
      const asset = await network(url);
      if (asset.ok) await cache.put(url, asset);
    } catch (_) { /* Optional visual assets do not block the shell. */ }
  }));
  await cache.put("/", response);
  return buildOf(html);
}

function refreshShell() {
  if (!preparing) {
    preparing = (async () => {
      let response;
      try { response = await network("/"); }
      catch (_) { return { reachable: false, ready: false, build: "" }; }
      if (!response.ok) return { reachable: false, ready: false, build: "" };
      // Cache quota and an incomplete deploy are not network disconnection.
      try { return { reachable: true, ready: true, build: await prepareShell(response) }; }
      catch (_) { return { reachable: true, ready: false, build: "" }; }
    })().finally(() => { preparing = undefined; });
  }
  return preparing;
}

async function savedShell() {
  try { return await (await caches.open(CACHE_VERSION)).match("/"); }
  catch (_) { return undefined; }
}

async function savedNavigation() {
  const hit = await savedShell();
  if (!hit) return Response.error();
  // This is evidence of a real cache fallback, available before WASM starts.
  // An online/offline heuristic or a disconnected burrow cannot set it.
  const html = (await hit.text()).replace(/<head(?:\s[^>]*)?>/i,
    '$&<script>window.__RH_CACHED_SHELL__=true;</script>');
  const headers = new Headers(hit.headers);
  headers.delete("content-length");
  headers.set("X-RabbitHole-Shell", "cached");
  return new Response(html, { status: 200, headers });
}

async function navigate(request, event) {
  try {
    let response = await network(request);
    // Older burrows do not map deep links to the SPA. A navigation may use
    // their root document; ordinary missing asset fetches never do this.
    if (response.status === 404 && new URL(request.url).pathname !== "/") {
      response = await network("/");
    }
    if (response.ok) {
      // The current navigation can use its successful network document even
      // when storage is unavailable. Offline promotion is independently safe.
      event.waitUntil(prepareShell(response.clone()).catch(() => { /* keep prior shell */ }));
      return response;
    }
    // Real route/asset errors must remain errors, not masquerade as the app.
    if (response.status < 500 && response.status !== 404) return response;
  } catch (_) { /* Network failed: use only a complete saved shell. */ }
  return savedNavigation();
}

async function cacheFirst(request, event) {
  // Search only RabbitHole caches, including older workers' still-live
  // hashed bundles. Do not consume another application's CacheStorage.
  try {
  for (const name of (await caches.keys()).filter((key) => key.startsWith(CACHE_PREFIX)).reverse()) {
    const cache = await caches.open(name);
    const hit = await cache.match(request);
    const bundle = /\.(?:js|wasm)$/.test(new URL(request.url).pathname);
    if (hit && (!bundle || validBundle(request.url, hit))) return hit;
    if (hit) await cache.delete(request);
  }
  } catch (_) { /* Storage is optional; try the network. */ }
  // Runtime assets keep their streaming body and have no precache timeout.
  // Cache writes cannot delay WASM streaming compilation or fail its request.
  const response = await fetch(request);
  const bundle = /\.(?:js|wasm)$/.test(new URL(request.url).pathname);
  if (response.ok && (!bundle || validBundle(request.url, response))) {
    const copy = response.clone();
    event.waitUntil(caches.open(CACHE_VERSION).then((cache) => cache.put(request, copy)).catch(() => {}));
  }
  return response;
}

self.addEventListener("install", (event) => {
  // A replacement waits for explicit user consent. First installation can
  // activate normally once this complete offline snapshot is available.
  event.waitUntil(refreshShell().then((snapshot) => {
    if (!snapshot.ready) throw new Error("No complete app shell to install");
  }));
});

self.addEventListener("activate", (event) => {
  // Keep old hashed bundles: another open tab can still need them offline.
  // Cache eviction is the browser's job while those tabs remain open.
  event.waitUntil(self.clients.claim());
});

self.addEventListener("message", (event) => {
  let message;
  try { message = JSON.parse(event.data); } catch (_) { return; }
  if (!event.source || !message || typeof message.type !== "string") return;
  if (message.type === "RH_PWA_ACTIVATE") {
    event.waitUntil(self.skipWaiting());
  } else if (message.type === "RH_PWA_CHECK") {
    event.waitUntil((async () => {
      const snapshot = await refreshShell();
      event.source.postMessage(JSON.stringify({ type: "RH_PWA_STATUS", reachable: snapshot.reachable,
        update: snapshot.ready && !!message.build && snapshot.build !== message.build,
        cached: !!(await savedShell()) }));
    })());
  }
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith(FILES_PREFIX)) return;
  if (request.mode === "navigate") {
    const response = navigate(request, event);
    event.respondWith(response);
    event.waitUntil(response.then(() => {}, () => {}));
  } else if (/^\/rabbithole-ui-web-[a-f0-9]+(?:_bg)?\.(js|wasm)$/.test(url.pathname) || STATIC_ASSETS.includes(url.pathname)) {
    const response = cacheFirst(request, event);
    event.respondWith(response);
    event.waitUntil(response.then(() => {}, () => {}));
  }
  // APIs, media, downloads and unknown/missing assets always use the network.
});
