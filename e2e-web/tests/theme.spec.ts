import { test, expect, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

// Opt in with built artifacts. Normal smoke runs still use their existing
// already-running server. These tests create and stop their own real burrows.
test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the real-server theme regression tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
// Keep service-worker caching out of this socket/theme integration test.
// routes.spec.ts separately covers direct deep links and hard reloads.
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

async function signIn(page: Page, burrow: TestBurrow, login = "theme-viewer") {
  await page.locator("#rh-login-server").fill(burrow.wsURL);
  await page.locator("#rh-login-handle").fill(login);
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.getByLabel("Save sign-in to bookmark").check();
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
    await page.getByRole("button", { name: "High contrast", exact: true }).click();
    await expect.poll(async () => (await tokens(page))["--rh-accent"]).not.toBe("#0055aa");
    const highContrast = await tokens(page);
    expect(highContrast["--rh-accent"]).not.toBe("#0055aa");
    const beforeUpdate = replies.accepted;
    await a.ctl("config-set", "theme_accent", "704099");
    await expect.poll(() => replies.accepted).toBeGreaterThan(beforeUpdate);
    expect(await tokens(page)).toEqual(highContrast);
    await page.getByRole("button", { name: "Clean", exact: true }).click();
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

const defaultTheme = (page: Page) => page.getByLabel("Default burrow theme", { exact: true });
const burrowTheme = (page: Page) => page.getByLabel("This burrow’s theme", { exact: true });
async function preferences(page: Page) {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(burrowTheme(page)).toBeVisible();
}
async function accountTheme(server: TestBurrow, login = "theme-viewer") {
  const { DatabaseSync } = await import("node:sqlite");
  const { join } = await import("node:path");
  const db = new DatabaseSync(join(server.dataDir, "burrow.db"));
  try {
    return (db.prepare("SELECT theme_server_disabled AS disabled FROM accounts WHERE login = ?").get(login) as { disabled: number }).disabled;
  } finally { db.close(); }
}
async function addBurrow(page: Page, server: TestBurrow, login = "theme-viewer") {
  await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
  await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
  await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(server.name);
  await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(server.wsURL);
  await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
  await page.locator(".rh-glass-row").filter({ hasText: server.name }).last().click();
  await page.getByRole("button", { name: "Connect…", exact: true }).click();
  const password = page.getByRole("button", { name: "Use password instead", exact: true });
  if (await password.count()) await password.click();
  await signIn(page, server, login);
}
async function settingsPictures(page: Page) {
  await page.locator('.rh-toasts button').evaluateAll((buttons) => buttons.forEach((button) => (button as HTMLButtonElement).click()));
  for (const [size, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
    await page.setViewportSize({ width, height });
    await page.getByText("Minimal keeps burrow colors", { exact: false }).scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const png = await page.screenshot({ path: test.info().outputPath(`theme-preferences-${size}.png`) });
    await test.info().attach(`theme-preferences-${size}`, { body: png, contentType: "image/png" });
  }
  await page.setViewportSize({ width: 1280, height: 900 });
}

test("full, minimal and off remain scoped to accounts and burrows with a local default", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const a = await TestBurrow.create("Preference Alpha", "a34700");
  const b = await TestBurrow.create("Preference Beta", "235b96");
  try {
    await a.ctl("account-create", "second-account", "theme-e2e-password", "user");
    await page.goto(a.httpURL);
    const base = await tokens(page);
    await signIn(page, a); await preferences(page);
    await expect(defaultTheme(page)).toHaveValue("full");
    await expect(burrowTheme(page)).toHaveValue("default");
    await burrowTheme(page).selectOption("minimal");
    await expectAccent(page, "#a34700");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe(base["--rh-radius"]);
    await expect(page.getByText("For Preference Alpha as theme-viewer.", { exact: false })).toContainText("saved to your account");
    await defaultTheme(page).selectOption("off");
    await expectAccent(page, "#a34700"); // this account's override wins
    await settingsPictures(page);

    await addBurrow(page, b);
    await expect.poll(() => tokens(page)).toEqual(base); // same login, different burrow
    await preferences(page); await expect(burrowTheme(page)).toHaveValue("default");
    await burrowTheme(page).selectOption("full");
    await expectAccent(page, "#235b96");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe("0.75rem");
    await page.locator('.rh-rail-server[aria-label^="Preference Alpha —"]').click();
    await expectAccent(page, "#a34700");
    await preferences(page); await expect(burrowTheme(page)).toHaveValue("minimal");
    await burrowTheme(page).selectOption("off");
    await expect.poll(() => accountTheme(a)).toBe(1);
    await expect.poll(() => tokens(page)).toEqual(base);
    await page.reload(); await preferences(page);
    await page.locator('.rh-rail-server[aria-label^="Preference Alpha —"]').click();
    await preferences(page); await expect(burrowTheme(page)).toHaveValue("off");
    await defaultTheme(page).selectOption("full");
    await expect.poll(() => tokens(page)).toEqual(base); // account Off wins over a new default
    await burrowTheme(page).selectOption("default");
    await expect.poll(() => accountTheme(a)).toBe(0);
    await expectAccent(page, "#a34700");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe("0.75rem");
    await burrowTheme(page).selectOption("minimal");
    await page.locator('.rh-rail-server[aria-label^="Preference Alpha —"]').click();
    await page.getByRole("button", { name: "Leave", exact: true }).click();
    await page.getByRole("alertdialog", { name: `Leave ${a.name}?`, exact: true }).getByRole("button", { name: "Leave", exact: true }).click();
    await expect(page.locator('.rh-rail-server[aria-label^="Preference Alpha —"]')).toHaveCount(0);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    // A second account on Alpha must not inherit the first account's Minimal.
    await signIn(page, a, "second-account");
    await preferences(page); await expect(burrowTheme(page)).toHaveValue("default");
    await expectAccent(page, "#a34700");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe("0.75rem");
    await burrowTheme(page).selectOption("off");
    await expect.poll(() => accountTheme(a, "second-account")).toBe(1);
    expect(await accountTheme(a)).toBe(0);
    await page.locator('.rh-rail-server[aria-label^="Preference Beta —"]').click();
    await expectAccent(page, "#235b96");
    expect(errors).toEqual([]);
  } finally {
    for (const server of [b, a]) {
      await server.dispose();
      await test.info().attach(`${server.name}-log`, { body: server.logs(), contentType: "text/plain" });
    }
  }
});

// Only parse the public routing header; never inspect or log authentication
// payloads. The injected reply models an older peer's Unsupported response.
function unsupportedPreference(message: string | Buffer): Buffer | undefined {
  if (!Buffer.isBuffer(message) || message[0] !== 1 || message[1] !== 0 || message[2] !== 0 || ![57, 58].includes(message[3])) return;
  let end = 4;
  while (message[end++] & 0x80) { /* request-id varint */ }
  const prefix = Buffer.from(message.subarray(0, end)); prefix[1] = 1;
  return Buffer.concat([prefix, Buffer.from([1, 8, 0])]);
}

test("legacy opt-out migrates and unsupported account preferences retain an honest local fallback", async ({ context, page }) => {
  const server = await TestBurrow.create("Older Theme Peer", "a34700");
  const seen = { get: 0, set: 0 };
  try {
    await context.addInitScript(() => {
      if (!localStorage.getItem("rh.theme.preferences.v1")) localStorage.setItem("rh-server-theme-disabled", "1");
    });
    await context.routeWebSocket(server.wsURL, (route) => {
      const upstream = route.connectToServer();
      route.onMessage((message) => {
        const reply = unsupportedPreference(message);
        if (reply) {
          if ((message as Buffer)[3] === 57) seen.get++; else seen.set++;
          route.send(reply);
        } else upstream.send(message);
      });
      upstream.onMessage((message) => route.send(message));
    });
    await page.goto(server.httpURL); const base = await tokens(page);
    await signIn(page, server); await preferences(page);
    await expect(defaultTheme(page)).toHaveValue("off");
    await expect.poll(() => seen.get).toBe(1);
    await expect(page.getByText("For Older Theme Peer as theme-viewer.", { exact: false })).toContainText("could not save");
    await expect.poll(() => tokens(page)).toEqual(base);
    await burrowTheme(page).selectOption("minimal");
    await expect.poll(() => seen.set).toBe(1);
    await expectAccent(page, "#a34700");
    await expect.poll(async () => (await tokens(page))["--rh-radius"]).toBe(base["--rh-radius"]);
    await page.reload(); await preferences(page);
    await expect(burrowTheme(page)).toHaveValue("minimal");
    await expect(defaultTheme(page)).toHaveValue("off");
    await expectAccent(page, "#a34700");
    await expect(page.locator(".rh-toasts")).not.toContainText("Unsupported");
    // Block only this new preference write, leaving authentication and every
    // other local setting real. The UI must not claim durable local saving.
    await page.evaluate(() => {
      const set = Storage.prototype.setItem;
      Storage.prototype.setItem = function (key, value) {
        if (key === "rh.theme.preferences.v1") throw new DOMException("Injected storage refusal", "QuotaExceededError");
        return set.call(this, key, value);
      };
    });
    await defaultTheme(page).selectOption("full");
    await expect(page.getByRole("status").filter({ hasText: "Could not save theme choices" })).toBeVisible();
    expect(await accountTheme(server)).toBe(0);
  } finally {
    await server.dispose(); await test.info().attach("fallback-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("a delayed account preference read cannot undo a newer selection", async ({ context, page }) => {
  const server = await TestBurrow.create("Delayed Theme Preference", "a34700");
  let release: (() => void) | undefined;
  let states = 0;
  try {
    await server.disableAccountTheme();
    await context.routeWebSocket(server.wsURL, (route) => {
      const upstream = route.connectToServer();
      upstream.onMessage((message) => {
        if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 1 && message[2] === 0 && message[3] === 59 && ++states === 1) {
          release = () => route.send(message);
        } else route.send(message);
      });
    });
    await page.goto(server.httpURL);
    await signIn(page, server); await preferences(page);
    await expect.poll(() => !!release).toBe(true);
    await burrowTheme(page).selectOption("minimal");
    await expect.poll(() => accountTheme(server)).toBe(0);
    await expectAccent(page, "#a34700");
    await expect(page.getByText("For Delayed Theme Preference as theme-viewer.", { exact: false })).toContainText("saved to your account");
    release!();
    // A round trip after the delayed read provides an ordering sentinel.
    await server.ctl("config-set", "theme_accent", "235b96");
    await expectAccent(page, "#235b96");
    await expect(burrowTheme(page)).toHaveValue("minimal");
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("rh.theme.preferences.v1")!).overrides);
    expect(stored).toHaveLength(1); expect(stored[0].mode).toBe("minimal");
  } finally {
    await server.dispose(); await test.info().attach("delayed-preference-log", { body: server.logs(), contentType: "text/plain" });
  }
});
