import { test, expect, type Page } from "@playwright/test";
import { cp, copyFile, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for real-worker PWA tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "allow", actionTimeout: 15_000 });
test.setTimeout(150_000);
test.beforeEach(async ({ context }) => {
  // Updated worker scripts cannot be routed by Playwright. Leave localhost
  // wholly un-intercepted while blocking external directory requests.
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname),
    (route) => route.abort());
});

async function controlled(page: Page) {
  await page.evaluate(() => Promise.race([
    navigator.serviceWorker.ready,
    new Promise((_, reject) => setTimeout(() => reject(new Error("Worker did not become ready")), 20_000)),
  ]));
  await expect.poll(() => page.evaluate(() => !!navigator.serviceWorker.controller)).toBe(true);
}
async function guest(page: Page, server: TestBurrow, handle: string) {
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(handle);
  await page.locator("#rh-login-password").fill("");
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
}
// Exercise the real worker's check message. No registration, install,
// activation, network, or cache API is replaced by a mock.
async function checkShell(page: Page) {
  return page.evaluate(() => new Promise<{ reachable: boolean; update: boolean }>((resolve, reject) => {
    const worker = navigator.serviceWorker.controller;
    if (!worker) return reject(new Error("No controlling worker"));
    const module = document.querySelector('script[type="module"]')?.textContent ?? "";
    const build = module.match(/\/rabbithole-ui-web-[a-f0-9]+\.js/)?.[0] ?? "";
    const timer = setTimeout(() => { navigator.serviceWorker.removeEventListener("message", receive); reject(new Error("No worker check reply")); }, 30_000);
    function receive(event: MessageEvent) {
      let message;
      try { message = JSON.parse(event.data); } catch { return; }
      if (message.type !== "RH_PWA_STATUS") return;
      clearTimeout(timer);
      navigator.serviceWorker.removeEventListener("message", receive);
      resolve(message);
    }
    navigator.serviceWorker.addEventListener("message", receive);
    worker.postMessage(JSON.stringify({ type: "RH_PWA_CHECK", build }));
  }));
}
async function captures(page: Page, state: string) {
  for (const [name, width, height] of [["desktop", 1280, 800], ["mobile", 390, 844]] as const) {
    await page.setViewportSize({ width, height });
    await expect(page.locator(".rh-pwa-notice")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    for (const button of await page.locator(".rh-pwa-actions button").all()) await expect(button).toBeVisible();
    const image = await page.screenshot({ path: test.info().outputPath(`${state}-${name}.png`) });
    await test.info().attach(`${state}-${name}`, { body: image, contentType: "image/png" });
  }
  await page.setViewportSize({ width: 1280, height: 800 });
}

test("real worker prepares complete updates, waits for consent and recovers a saved shell", async ({ context, page }) => {
  const dist = await mkdtemp(join(tmpdir(), "rh-pwa-dist-"));
  const servers: TestBurrow[] = [];
  const errors: string[] = [];
  const workerEvents: string[] = [];
  const started = Date.now();
  const record = (event: unknown) => workerEvents.push(`${Date.now() - started} ${JSON.stringify(event)}`);
  const diagnostics = await context.newCDPSession(page);
  diagnostics.on("ServiceWorker.workerRegistrationUpdated", record);
  diagnostics.on("ServiceWorker.workerVersionUpdated", record);
  diagnostics.on("ServiceWorker.workerErrorReported", record);
  await diagnostics.send("ServiceWorker.enable");
  context.on("console", (message) => { if (message.type() === "error") record({ console: message.text() }); });
  context.on("request", (request) => { if (request.url().endsWith("/sw.js")) record({ request: request.url() }); });
  context.on("requestfailed", (request) => { if (request.url().endsWith("/sw.js")) record({ failed: request.url(), error: request.failure() }); });
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await cp(process.env.SPA_DIST!, dist, { recursive: true });
    const original = await readFile(join(dist, "index.html"), "utf8");
    const oldPrefix = original.match(/rabbithole-ui-web-[a-f0-9]+/)![0];
    const newPrefix = "rabbithole-ui-web-abcdef4500000001";
    const next = original.replaceAll(oldPrefix, newPrefix);
    const host = await TestBurrow.create("PWA App Host", "a34700", { spaDist: dist });
    servers.push(host);
    const burrow = await TestBurrow.create("PWA Separate Burrow", "235b96");
    servers.push(burrow);
    await page.goto(host.httpURL);
    await controlled(page);
    expect(await checkShell(page)).toMatchObject({ reachable: true, update: false });
    await guest(page, burrow, "pwa-reader");
    const composer = page.getByRole("textbox", { name: "Message #lobby", exact: true });
    await composer.fill("A draft stays here until I choose reload");
    const oldTab = await context.newPage();
    await oldTab.goto(host.httpURL);
    await controlled(oldTab);
    await guest(oldTab, burrow, "pwa-other-tab");
    const otherComposer = oldTab.getByRole("textbox", { name: "Message #lobby", exact: true });
    await otherComposer.fill("The other tab keeps its work too");

    // A separately hosted burrow can disconnect while the app host is fine.
    await burrow.stop();
    await expect(page.locator(".rh-header .rh-dot.on")).toHaveCount(0);
    expect(await checkShell(page)).toMatchObject({ reachable: true, update: false });
    await expect(page.locator(".rh-pwa-notice")).toHaveCount(0);
    await expect(composer).toHaveValue("A draft stays here until I choose reload");

    // HTTP errors do not become false "online" reports. A cached navigation
    // is still identified when the browser itself reports a working network.
    await rm(join(dist, "index.html"));
    expect(await checkShell(page)).toMatchObject({ reachable: false, update: false });
    const unavailable = await context.newPage();
    await unavailable.goto(`${host.httpURL}/lobby`);
    await expect(unavailable.locator(".rh-app")).toBeVisible();
    await expect(unavailable.locator(".rh-pwa-notice")).toContainText("Using a saved copy");
    expect(await unavailable.evaluate(() => navigator.onLine)).toBe(true);
    await writeFile(join(dist, "index.html"), original);
    expect(await checkShell(unavailable)).toMatchObject({ reachable: true, update: false });
    await expect(unavailable.locator(".rh-pwa-notice")).toHaveCount(0);
    await unavailable.close();
    await checkShell(page);
    // Preserve root fallback for old servers' unknown navigation routes,
    // while a missing JS/WASM resource keeps its real 404 response.
    const legacy = await context.newPage();
    expect((await legacy.goto(`${host.httpURL}/legacy-deep-link`))!.status()).toBe(200);
    await expect(legacy.locator(".rh-app")).toBeVisible();
    await legacy.close();
    expect(await page.evaluate(async () => (await fetch("/rabbithole-ui-web-deadbeef_bg.wasm")).status)).toBe(404);

    // Partial deploy: the new document/JS exists but its WASM does not. A
    // poisoned cached 200 HTML response must not count as a prepared bundle.
    await copyFile(join(dist, `${oldPrefix}.js`), join(dist, `${newPrefix}.js`));
    await writeFile(join(dist, "index.html"), next);
    await page.evaluate(async (url) => {
      const cache = await caches.open("rabbithole-shell-v2");
      await cache.put(url, new Response("not WASM", { headers: { "Content-Type": "text/html" } }));
    }, `/${newPrefix}_bg.wasm`);
    expect(await checkShell(page)).toMatchObject({ reachable: true, update: false });
    await expect(page.locator(".rh-pwa-notice")).toHaveCount(0);
    await context.setOffline(true);
    const partial = await context.newPage();
    await partial.goto(`${host.httpURL}/lobby`);
    await expect(partial.locator(".rh-app")).toBeVisible();
    await expect(partial.locator(".rh-pwa-notice")).toContainText("Using a saved copy");
    expect(await partial.locator('script[type="module"]').textContent()).toContain(oldPrefix);
    await partial.close();
    await context.setOffline(false);
    await expect(page.locator(".rh-pwa-notice")).toHaveCount(0);

    // Complete the copied real bundle under a different build fingerprint,
    // then change the worker bytes so real updatefound/waiting also execute.
    await copyFile(join(dist, `${oldPrefix}_bg.wasm`), join(dist, `${newPrefix}_bg.wasm`));
    // Normal SPA deployments can leave sw.js byte-for-byte unchanged.
    expect(await checkShell(page)).toMatchObject({ reachable: true, update: true });
    await expect(page.getByRole("button", { name: "Reload to update", exact: true })).toBeVisible();
    await expect(composer).toHaveValue("A draft stays here until I choose reload");
    expect(await page.locator('script[type="module"]').textContent()).toContain(oldPrefix);
    const worker = await readFile(join(dist, "sw.js"), "utf8");
    await writeFile(join(dist, "sw.js"), `${worker}\n// PWA regression deployment two\n`);
    record({ step: "request changed worker" });
    await page.evaluate(async () => {
      const registration = (await navigator.serviceWorker.getRegistration())!;
      void registration.update().catch((error) => console.error("Worker update failed", error));
    });
    await expect.poll(() => page.evaluate(async () => !!(await navigator.serviceWorker.getRegistration())?.waiting), { timeout: 45_000 }).toBe(true);
    await expect(page.getByRole("button", { name: "Reload to update", exact: true })).toBeVisible();
    await expect(composer).toHaveValue("A draft stays here until I choose reload");
    expect(await page.locator('script[type="module"]').textContent()).toContain(oldPrefix);
    await captures(page, "update");

    // Explicit activation boots the fully prepared new document offline.
    await context.setOffline(true);
    await page.getByRole("button", { name: "Reload to update", exact: true }).click();
    await expect(page.locator(".rh-app")).toBeVisible();
    await expect.poll(() => page.locator('script[type="module"]').textContent()).toContain(newPrefix);
    await expect(page.locator(".rh-pwa-notice")).toContainText("Using a saved copy");
    await captures(page, "offline");
    await expect(otherComposer).toHaveValue("The other tab keeps its work too");
    expect(await oldTab.locator('script[type="module"]').textContent()).toContain(oldPrefix);
    expect(await oldTab.evaluate(async (path) => {
      const response = await fetch(path);
      return response.ok && (await response.arrayBuffer()).byteLength > 0;
    }, `/${oldPrefix}_bg.wasm`)).toBe(true);

    // Recovery removes the notice without refreshing or stealing focus.
    const handle = page.locator("#rh-login-handle");
    await handle.fill("keep this field focused");
    await handle.focus();
    await context.setOffline(false);
    await expect(page.locator(".rh-pwa-notice")).toHaveCount(0);
    await expect(handle).toHaveValue("keep this field focused");
    await expect(handle).toBeFocused();
    expect(errors).toEqual([]);
  } finally {
    await test.info().attach("worker-events", { body: workerEvents.join("\n"), contentType: "text/plain" });
    await writeFile(test.info().outputPath("worker-events.log"), workerEvents.join("\n"));
    await context.setOffline(false);
    for (const server of servers.reverse()) {
      await server.dispose();
      await test.info().attach(`${server.name}-server-log`, { body: server.logs(), contentType: "text/plain" });
    }
    await rm(dist, { recursive: true, force: true });
  }
});

test("native shells exclude the web worker and cache status", async ({ page }) => {
  const server = await TestBurrow.create("PWA Native Exclusion", "a34700");
  try {
    await page.addInitScript(() => { (window as unknown as { __RH_IS_NATIVE__: boolean }).__RH_IS_NATIVE__ = true; });
    await page.goto(server.httpURL);
    await expect(page.locator(".rh-app.native")).toBeVisible();
    expect(await page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length)).toBe(0);
    await expect(page.locator(".rh-pwa-notice")).toHaveCount(0);
  } finally {
    await server.dispose();
    await test.info().attach("native-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
