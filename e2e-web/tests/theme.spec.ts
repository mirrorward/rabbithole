import { test, expect, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

// Opt in with built artifacts. Normal smoke runs still use their existing
// already-running server. These tests create and stop their own real burrows.
test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the real-server theme regression tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
// Keep service-worker caching out of this socket/theme integration test. The
// HTTP fixture serves '/' and the production worker owns deep-link fallback.
test.use({ colorScheme: "light", serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

const diagnostics = new WeakMap<Page, string[]>();
test.beforeEach(async ({ context, page }) => {
  const events: string[] = [];
  diagnostics.set(page, events);
  page.on("pageerror", (error) => events.push(`pageerror: ${error.message}`));
  page.on("websocket", (socket) => {
    events.push(`socket opened: ${socket.url()}`);
    socket.on("close", () => events.push(`socket closed: ${socket.url()}`));
    socket.on("socketerror", (error) => events.push(`socket error: ${error}`));
    for (const direction of ["framesent", "framereceived"] as const) {
      socket.on(direction, ({ payload }) => {
        if (Buffer.isBuffer(payload)) {
          events.push(`${direction}: ${socket.url()} prefix=${payload.subarray(0, 4).toString("hex")} bytes=${payload.length}`);
        }
      });
    }
  });
  // The app's Looking Glass discovery is unrelated to this test. No external
  // host should receive traffic from the isolated local integration fixture.
  await context.route("**/*", (route) => {
    const host = new URL(route.request().url()).hostname;
    return ["127.0.0.1", "localhost", "[::1]"].includes(host)
      ? route.continue() : route.abort();
  });
});

test.afterEach(async ({ page }, info) => {
  if (info.status !== info.expectedStatus) {
    await info.attach("socket-events", {
      body: diagnostics.get(page)?.join("\n") ?? "",
      contentType: "text/plain",
    });
  }
});

async function tokens(page: Page) {
  return page.locator(".rh-app").evaluate((root) => {
    const style = getComputedStyle(root);
    return Object.fromEntries(["--rh-accent", "--rh-bg", "--rh-text", "--rh-radius"]
      .map((name) => [name, style.getPropertyValue(name).trim()]));
  });
}

async function expectAccent(page: Page, accent: string) {
  await expect.poll(async () => (await tokens(page))["--rh-accent"]).toBe(accent);
}

async function signIn(page: Page, burrow: TestBurrow) {
  await page.locator("#rh-login-server").fill(burrow.wsURL);
  await page.locator("#rh-login-handle").fill("theme-viewer");
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(burrow.name);
  await expect(page.locator(".rh-header .rh-dot.on")).toBeVisible();
}

// Observe only the stable RHP v1 frame prefix (postcard version, kind, family,
// type; all one-byte values here), not the opaque signed payload. A successful
// ThemeGet reply is SESSION45, an error reply echoes SESSION44. Waiting for the
// real reply avoids mistaking a pre-login default palette for cleared state.
function themeReplies(page: Page, endpoint: string) {
  const seen = { accepted: 0, refused: 0 };
  page.on("websocket", (socket) => {
    if (new URL(socket.url()).href !== new URL(endpoint).href) return;
    socket.on("framereceived", ({ payload }) => {
      if (!Buffer.isBuffer(payload)) return;
      if (payload[0] !== 1 || payload[1] !== 1 || payload[2] !== 0) return;
      if (payload[3] === 45) seen.accepted += 1;
      if (payload[3] === 44) seen.refused += 1;
    });
  });
  return seen;
}

test("signed themes update, clear, reconnect and stay scoped to their burrow", async ({ page }) => {
  const servers: TestBurrow[] = [];
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    const a = await TestBurrow.create("Theme Alpha", "a34700");
    servers.push(a);
    const b = await TestBurrow.create("Theme Beta", "235b96");
    servers.push(b);
    const replies = themeReplies(page, a.wsURL);
    await page.goto(a.httpURL);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    const base = await tokens(page);
    await signIn(page, a);
    await expectAccent(page, "#a34700");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe("0.75rem");
    await expect.poll(() => replies.accepted).toBeGreaterThan(0);

    const connectedURL = page.url();
    await a.ctl("config-set", "theme_accent", "0055aa");
    await expectAccent(page, "#0055aa");
    expect(page.url()).toBe(connectedURL);

    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByRole("radio", { name: "High contrast", exact: true }).click();
    await expect.poll(async () => (await tokens(page))["--rh-accent"]).not.toBe("#0055aa");
    const highContrast = await tokens(page);
    expect(highContrast["--rh-accent"]).not.toBe("#0055aa");
    const beforeUpdate = replies.accepted;
    await a.ctl("config-set", "theme_accent", "704099");
    await expect.poll(() => replies.accepted).toBeGreaterThan(beforeUpdate);
    expect(await tokens(page)).toEqual(highContrast);
    await page.getByRole("radio", { name: "Clean", exact: true }).click();
    await expectAccent(page, "#704099");
    await page.keyboard.press("Escape");

    const beforeClear = replies.refused;
    await a.ctl("theme-clear");
    await expect.poll(() => replies.refused).toBeGreaterThan(beforeClear);
    await expect.poll(() => tokens(page)).toEqual(base);
    await a.ctl("config-set", "theme_accent", "1d6b58");
    await expectAccent(page, "#1d6b58");

    await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
    await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
    await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(b.name);
    await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(b.wsURL);
    await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
    await page.locator(".rh-glass-row").filter({ hasText: b.name }).click();
    await page.getByRole("button", { name: "Connect…", exact: true }).click();
    await signIn(page, b);
    await expectAccent(page, "#235b96");
    const beforeBackgroundUpdate = replies.accepted;
    await a.ctl("config-set", "theme_accent", "854019");
    await expect.poll(() => replies.accepted).toBeGreaterThan(beforeBackgroundUpdate);
    await expectAccent(page, "#235b96");
    await page.locator('.rh-rail-server[aria-label^="Theme Alpha —"]').click();
    await expectAccent(page, "#854019");
    await page.locator('.rh-rail-server[aria-label^="Theme Beta —"]').click();
    await expectAccent(page, "#235b96");
    await page.locator('.rh-rail-server[aria-label^="Theme Alpha —"]').click();

    // Change the stored theme while offline, preserving the server identity.
    // A reconnect must fetch the new signed content instead of keeping an old
    // content-hash cache entry or another burrow's palette.
    const beforeReconnect = replies.accepted;
    await a.restartWithAccent("4154a1");
    await expect.poll(() => replies.accepted, { timeout: 30_000 }).toBeGreaterThan(beforeReconnect);
    await expectAccent(page, "#4154a1");
    const beforeReload = replies.accepted;
    await page.goto(a.httpURL);
    await expect.poll(() => replies.accepted, { timeout: 30_000 }).toBeGreaterThan(beforeReload);
    await page.locator('.rh-rail-server[aria-label^="Theme Alpha —"]').click();
    await expectAccent(page, "#4154a1");
    expect(errors).toEqual([]);
  } finally {
    for (const server of servers.reverse()) {
      await server.dispose();
      await test.info().attach(`${server.name}-server-log`, { body: server.logs(), contentType: "text/plain" });
    }
  }
});

test("an account opting out on the server clears its previously displayed theme", async ({ page }) => {
  const server = await TestBurrow.create("Theme Preference", "a34700");
  try {
    const replies = themeReplies(page, server.wsURL);
    await page.goto(server.httpURL);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    const base = await tokens(page);
    await signIn(page, server);
    await expectAccent(page, "#a34700");
    // This fixture seeds the existing persisted account preference directly;
    // it does not invent a local opt-out control or test an absent UI flow.
    await server.disableAccountTheme();
    await page.goto(server.httpURL);
    await expect.poll(() => replies.refused, { timeout: 30_000 }).toBeGreaterThan(0);
    await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    await expect.poll(() => tokens(page)).toEqual(base);
  } finally {
    await server.dispose();
    await test.info().attach("server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
