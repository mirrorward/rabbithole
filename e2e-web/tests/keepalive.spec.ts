import { test, expect } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the default-limit browser test");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(60_000);

test("initial app load and immediate reload succeed with production rate limits", async ({ context, page }) => {
  const server = await TestBurrow.create("Default Limits", "a34700", { rateLimits: "default" });
  const errors: string[] = [];
  const failedAssets: string[] = [];
  let assets: { url: string; status: number }[] = [];
  const isBundle = (url: string) => /\/rabbithole-ui-web-[^/]+\.(js|wasm)$/.test(new URL(url).pathname);
  const isLocal = (url: string) => new URL(url).origin === server.httpURL;
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("requestfailed", (request) => {
    if (isLocal(request.url())) failedAssets.push(`${request.url()}: ${request.failure()?.errorText}`);
  });
  page.on("response", (response) => {
    if (isBundle(response.url())) assets.push({ url: response.url(), status: response.status() });
    if (isLocal(response.url()) && response.status() >= 400) {
      failedAssets.push(`${response.url()}: HTTP ${response.status()}`);
    }
  });
  try {
    // Interception also disables the browser HTTP cache, so both loads fetch
    // the real assets. No worker or cached copy can hide a reset connection.
    await context.route("**/*", (route) => {
      const host = new URL(route.request().url()).hostname;
      return ["127.0.0.1", "localhost", "[::1]"].includes(host)
        ? route.continue() : route.abort();
    });
    const first = await page.goto(server.httpURL);
    expect(first?.status()).toBe(200);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    for (const extension of [".js", ".wasm"]) {
      expect(assets.some((asset) => asset.url.endsWith(extension) && asset.status === 200)).toBe(true);
    }

    // No pause or budget reset between the two loads.
    assets = [];
    const reloaded = await page.reload();
    expect(reloaded?.status()).toBe(200);
    expect(reloaded?.fromServiceWorker()).toBe(false);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    for (const extension of [".js", ".wasm"]) {
      expect(assets.some((asset) => asset.url.endsWith(extension) && asset.status === 200)).toBe(true);
    }
    expect(failedAssets).toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("default-limit-diagnostics", {
      body: JSON.stringify({ assets, failedAssets, errors, server: server.logs() }, null, 2),
      contentType: "application/json",
    });
  }
});
