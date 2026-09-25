import { test, expect, type Response } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the real-server route regression test");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(90_000);

test("deep links and hard reloads boot the SPA without a service worker", async ({ context, page }) => {
  const server = await TestBurrow.create("Route Regression", "a34700");
  const errors: string[] = [];
  const failedAssets: string[] = [];
  let assets: { url: string; status: number; worker: boolean }[] = [];
  const auth = { password: 0, resume: 0, accepted: 0 };
  const isBundle = (url: string) => /\/rabbithole-ui-web-[^/]+\.(js|wasm)$/.test(new URL(url).pathname);

  page.on("pageerror", (error) => errors.push(error.message));
  page.on("requestfailed", (request) => {
    if (isBundle(request.url())) failedAssets.push(`${request.url()}: ${request.failure()?.errorText}`);
  });
  page.on("response", (response) => {
    if (isBundle(response.url())) {
      assets.push({ url: response.url(), status: response.status(), worker: response.fromServiceWorker() });
    }
  });
  // Observe message types only, never passwords or bearer tokens. RHP v1's
  // first four postcard bytes are version, kind, family, and message type.
  page.on("websocket", (socket) => {
    if (new URL(socket.url()).href !== new URL(server.wsURL).href) return;
    socket.on("framesent", ({ payload }) => {
      if (!Buffer.isBuffer(payload) || payload[0] !== 1 || payload[1] !== 0 || payload[2] !== 0) return;
      if (payload[3] === 10) auth.password += 1;
      if (payload[3] === 12) auth.resume += 1;
    });
    socket.on("framereceived", ({ payload }) => {
      if (Buffer.isBuffer(payload) && payload[0] === 1 && payload[1] === 1 && payload[2] === 0 && payload[3] === 13) {
        auth.accepted += 1;
      }
    });
  });

  async function shellLoaded(response: Response | null, path: string) {
    expect(response, `document response for ${path}`).not.toBeNull();
    expect(response!.status()).toBe(200);
    expect(response!.url()).toBe(`${server.httpURL}${path}`);
    expect(await response!.headerValue("content-type")).toContain("text/html");
    expect(response!.fromServiceWorker()).toBe(false);
    await expect(page.locator(".rh-app")).toBeVisible();
    for (const extension of [".js", ".wasm"]) {
      await expect.poll(() => assets.some((asset) => asset.url.endsWith(extension) && asset.status === 200 && !asset.worker))
        .toBe(true);
    }
  }

  try {
    // Keep public directory requests outside this isolated server fixture.
    await context.route("**/*", (route) => {
      const host = new URL(route.request().url()).hostname;
      return ["127.0.0.1", "localhost", "[::1]"].includes(host)
        ? route.continue() : route.abort();
    });
    // A brand-new browser has no cached shell and no installed worker. The
    // app may send an unauthenticated lobby visitor to its normal login view.
    await shellLoaded(await page.goto(`${server.httpURL}/lobby`), "/lobby");
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    await page.locator("#rh-login-server").fill(server.wsURL);
    await page.locator("#rh-login-handle").fill("theme-viewer");
    await page.locator("#rh-login-password").fill("theme-e2e-password");
    await page.locator('.rh-login button[type="submit"]').click();
    await expect.poll(() => auth.password).toBeGreaterThan(0);
    await expect.poll(() => auth.accepted).toBeGreaterThan(0);
    await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    await expect(page).toHaveURL(`${server.httpURL}/lobby`);

    const passwords = auth.password;
    const beforeReload = { ...auth };
    assets = [];
    await shellLoaded(await page.reload(), "/lobby");
    await expect.poll(() => auth.resume).toBeGreaterThan(beforeReload.resume);
    await expect.poll(() => auth.accepted).toBeGreaterThan(beforeReload.accepted);
    await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    await expect(page.locator("#rh-login-handle")).toHaveCount(0);
    expect(auth.password).toBe(passwords);

    // Nested routes must resolve hashed assets from the web root, including
    // a route with a query string. The app currently restores to the lobby;
    // these assertions concern the requested document and its real boot.
    for (const path of ["/boards/general", "/people/route-check?from=reload"]) {
      const before = { ...auth };
      assets = [];
      await shellLoaded(await page.goto(`${server.httpURL}${path}`), path);
      await expect.poll(() => auth.resume).toBeGreaterThan(before.resume);
      await expect.poll(() => auth.accepted).toBeGreaterThan(before.accepted);
      await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    }

    // A missing script or WASM binary must never receive the HTML fallback.
    for (const path of ["/rabbithole-ui-web-missing.js", "/boards/assets/missing.wasm"]) {
      const response = await context.request.get(`${server.httpURL}${path}`);
      expect(response.status()).toBe(404);
      expect(response.headers()["content-type"]).not.toContain("text/html");
    }
    expect(auth.password).toBe(passwords);
    expect(failedAssets).toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("route-diagnostics", {
      body: JSON.stringify({ auth, assets, failedAssets, errors, server: server.logs() }, null, 2),
      contentType: "application/json",
    });
  }
});
