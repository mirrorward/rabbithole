import { test, expect, type Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST for the real-SPA native navigation bridge test");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(90_000);

type NativeFixture = Window & {
  __rhTestNativeEvents: Record<string, (event: { payload: unknown }) => void>;
  __rhTestNativeCalls: { command: string; args: { back?: boolean; forward?: boolean } }[];
};

async function emit(page: Page, event: string, payload: unknown) {
  await page.evaluate(({ event, payload }) => {
    const receive = (window as NativeFixture).__rhTestNativeEvents[event];
    if (!receive) throw new Error(`Native listener missing: ${event}`);
    receive({ payload });
  }, { event, payload });
}

async function available(page: Page) {
  return page.evaluate(() => (window as NativeFixture).__rhTestNativeCalls
    .filter((call) => call.command === "navigation_state").at(-1)?.args);
}

// The real shim and compiled SPA run unchanged. Only the Tauri IPC boundary
// is a fixture: this verifies navigation integration, not macOS menu delivery.
test("native menu navigation dismisses the palette and preserves fragment history", async ({ context, page }) => {
  const server = await TestBurrow.create("Native Menu Bridge", "a34700");
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname),
      (route) => route.abort());
    const shim = (await readFile(new URL("../../apps/desktop/src/native-shim.js", import.meta.url), "utf8"))
      .replace("__RH_MAIN_WINDOW__", "true")
      .replace("__RH_VERSION__", "fixture")
      .replace("__RH_SHA__", "fixture");
    // One script keeps bridge setup before the shim regardless of Playwright's
    // ordering of multiple initialization scripts. The fixture records no
    // credentials or wire payloads, just the command names and menu booleans.
    await page.addInitScript({ content: `
      window.__rhTestNativeEvents = {};
      window.__rhTestNativeCalls = [];
      window.__TAURI_INTERNALS__ = {
        transformCallback: function (callback) { return callback; },
        invoke: function (command, args) {
          window.__rhTestNativeCalls.push({ command: command,
            args: command === 'navigation_state' ? args : {} });
          if (command === 'plugin:event|listen') {
            window.__rhTestNativeEvents[args.event] = args.handler;
            return Promise.resolve(1);
          }
          return Promise.resolve(false);
        }
      };
      ${shim}
    ` });
    await page.goto(`${server.httpURL}/settings`);
    await expect(page.locator(".rh-app.native")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Settings", exact: true, level: 2 })).toBeVisible();
    await expect.poll(() => available(page)).toEqual({ back: false, forward: false });
    const palette = page.getByRole("dialog", { name: "Jump to a section" });
    async function openPalette() {
      await page.keyboard.press("ControlOrMeta+k");
      await expect(palette).toBeVisible();
    }

    await openPalette();
    await emit(page, "rh://navigate", "/servers");
    await expect(page).toHaveURL(`${server.httpURL}/servers`);
    await expect(palette).toBeHidden();
    const length = await page.evaluate(() => history.length);
    await openPalette();
    await emit(page, "rh://navigate", "/servers");
    await expect(palette).toBeHidden();
    expect(await page.evaluate(() => history.length)).toBe(length);

    // This is the actual app skip link. Its rel=external asks the browser to
    // create a fragment entry, bypassing the router's history.pushState call.
    const skip = page.getByRole("link", { name: "Skip to main content" });
    await skip.focus();
    await skip.press("Enter");
    await expect(page).toHaveURL(`${server.httpURL}/servers#rh-main`);
    await expect.poll(() => available(page)).toEqual({ back: true, forward: false });
    await emit(page, "rh://history", "back");
    await expect(page).toHaveURL(`${server.httpURL}/servers`);
    await expect.poll(() => available(page)).toEqual({ back: true, forward: true });
    await emit(page, "rh://history", "back");
    await expect(page).toHaveURL(`${server.httpURL}/settings`);
    await expect.poll(() => available(page)).toEqual({ back: false, forward: true });
    await emit(page, "rh://history", "forward");
    await expect(page).toHaveURL(`${server.httpURL}/servers`);
    await emit(page, "rh://navigate", "/you");
    await expect(page).toHaveURL(`${server.httpURL}/you`);
    await expect.poll(() => available(page)).toEqual({ back: true, forward: false });
    await page.reload();
    await expect(page.locator(".rh-app.native")).toBeVisible();
    await expect.poll(() => available(page)).toEqual({ back: true, forward: false });
    await openPalette();
    await emit(page, "rh://history", "back");
    await expect(page).toHaveURL(`${server.httpURL}/servers`);
    await expect(palette).toBeHidden();
    await expect.poll(() => available(page)).toEqual({ back: true, forward: true });
    await emit(page, "rh://history", "forward");
    await expect(page).toHaveURL(`${server.httpURL}/you`);

    await emit(page, "rh://fullscreen", true);
    await expect(page.locator("html")).toHaveClass(/rh-fullscreen/);
    await emit(page, "rh://fullscreen", false);
    await expect(page.locator("html")).not.toHaveClass(/rh-fullscreen/);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("native-menu-diagnostics", {
      body: JSON.stringify({ errors, server: server.logs() }, null, 2), contentType: "application/json",
    });
  }
});
