import { test, expect, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";
import { feedBurrow, localFeeds, restartWithFeeds } from "../fixtures/feeds";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST for real-server feed monitor tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ colorScheme: "light", serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

const errors = new WeakMap<Page, string[]>();
test.beforeEach(async ({ context, page }) => {
  const observed: string[] = [];
  errors.set(page, observed);
  page.on("pageerror", error => observed.push(error.message));
  await context.route("**/*", route => ["127.0.0.1", "localhost", "[::1]"].includes(new URL(route.request().url()).hostname)
    ? route.continue() : route.abort());
});
test.afterEach(async ({ page }) => { expect(errors.get(page)).toEqual([]); });

async function signIn(page: Page, server: TestBurrow, login = "feed-admin") {
  const password = page.getByRole("button", { name: "Use password instead", exact: true });
  if (await password.count()) await password.click();
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(login);
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.getByLabel("Save sign-in to bookmark").check();
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await expect(page.locator(".rh-header .rh-dot.on")).toBeVisible();
}
async function monitor(page: Page) {
  await page.getByRole("link", { name: "Admin", exact: true }).click();
  await page.getByRole("link", { name: "Federation & feeds", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Mapped feeds", exact: true })).toBeVisible();
}
const table = (page: Page) => page.getByRole("table", { name: "Mapped feeds", exact: true });
const row = (page: Page, board: string) => table(page).getByRole("row").filter({ has: page.getByRole("cell", { name: board, exact: true }) });
const refresh = (page: Page) => page.getByRole("button", { name: "Refresh feed monitor", exact: true }).click();
function mappingFrame(message: string | Buffer, kind: number, type: number): message is Buffer {
  return Buffer.isBuffer(message) && message[0] === 1 && message[1] === kind && message[2] === 7 && message[3] === type;
}
async function dispose(server: TestBurrow) {
  await server.dispose();
  await test.info().attach(`${server.name}-log`, { body: server.logs(), contentType: "text/plain" });
}
async function addBurrow(page: Page, server: TestBurrow) {
  await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
  await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
  await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(server.name);
  await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(server.wsURL);
  await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
  await page.locator(".rh-glass-row").filter({ hasText: server.name }).last().click();
  await page.getByRole("button", { name: "Connect…", exact: true }).click();
  await signIn(page, server);
}

test("configured feeds survive redaction collisions, real polls and a TOML restart", async ({ page }) => {
  const feeds = await localFeeds();
  const mappings = { [feeds.first]: "first-board", [feeds.second]: "second-board", [feeds.credential]: "rejected-board" };
  const server = await feedBurrow("Feed Monitor", mappings);
  try {
    await server.ctl("board-create", "moved-board", "Moved board");
    await page.goto(server.httpURL);
    await signIn(page, server); await monitor(page);
    await expect(table(page).locator("tbody tr")).toHaveCount(3);
    for (const board of Object.values(mappings)) {
      await expect(row(page, board)).toContainText("never polled");
      await expect(row(page, board).getByRole("cell").nth(0)).toHaveText(feeds.base);
    }
    expect(feeds.requests).toEqual({ first: 0, second: 0 });
    await expect(page.getByText("Read-only here:", { exact: false })).toContainText("burrow.toml and restart");
    await expect(table(page).locator("a, input, textarea, button")).toHaveCount(0);

    await restartWithFeeds(server, true, mappings);
    await expect(page.locator(".rh-header .rh-dot.on")).toBeVisible({ timeout: 30_000 });
    await expect.poll(() => feeds.requests.first).toBeGreaterThan(0);
    await expect.poll(() => feeds.requests.second).toBeGreaterThan(0);
    await expect(async () => {
      await refresh(page);
      await expect(row(page, "first-board")).toContainText("1 seen · 1 posted · 0 dupes");
      await expect(row(page, "second-board")).toContainText("2 seen · 2 posted · 0 dupes");
    }).toPass({ timeout: 15_000 });
    await expect(row(page, "rejected-board")).toContainText("error");
    const exposed = await page.locator("body").innerText() + server.logs();
    for (const secret of ["first-secret", "second-secret", "fixture-user", "credential-secret", "credential-query", "first-fragment", "second-fragment"]) expect(exposed).not.toContain(secret);

    await page.locator(".rh-toasts button").evaluateAll(buttons => buttons.forEach(button => (button as HTMLButtonElement).click()));
    for (const [size, width, height] of [["desktop", 1280, 900], ["narrow", 390, 844]] as const) {
      await page.setViewportSize({ width, height });
      await page.getByRole("heading", { name: "Mapped feeds", exact: true }).evaluate(heading => heading.scrollIntoView({ block: "start" }));
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const png = await page.screenshot({ path: test.info().outputPath(`feed-monitor-${size}.png`) });
      await test.info().attach(`feed-monitor-${size}`, { body: png, contentType: "image/png" });
    }
    await page.setViewportSize({ width: 1280, height: 900 });
    // Same raw URL/opaque row ID, changed metadata and zeroed runtime stats:
    // reconnect must replace the keyed row rather than retain its old cells.
    await restartWithFeeds(server, false, { ...mappings, [feeds.first]: "moved-board" });
    await expect(row(page, "moved-board")).toContainText("never polled", { timeout: 30_000 });
    await expect(row(page, "first-board")).toHaveCount(0);
    await expect(row(page, "second-board")).toContainText("never polled");
  } finally {
    await dispose(server); await feeds.dispose();
  }
});

test("an older feed API shows an honest fallback and clears previously loaded rows", async ({ context, page }) => {
  const server = await feedBurrow("Older Feed Peer", { "https://example.test/rss?token=hidden-token": "older-board" });
  let unsupported = false;
  let refused = 0;
  try {
    await context.routeWebSocket(server.wsURL, route => {
      const upstream = route.connectToServer();
      route.onMessage(message => {
        if (unsupported && mappingFrame(message, 0, 65)) {
          let end = 4;
          while (message[end++] & 0x80) { /* request-id varint */ }
          const prefix = Buffer.from(message.subarray(0, end)); prefix[1] = 1;
          refused++;
          route.send(Buffer.concat([prefix, Buffer.from([1, 8, 0])]));
        } else upstream.send(message);
      });
      upstream.onMessage(message => route.send(message));
    });
    await page.goto(server.httpURL); await signIn(page, server); await monitor(page);
    await expect(row(page, "older-board")).toBeVisible();
    unsupported = true; await refresh(page);
    await expect.poll(() => refused).toBe(1);
    await expect(page.getByText("This burrow cannot show its configured feed mappings here.", { exact: false })).toContainText("burrow.toml and restart");
    await expect(table(page)).toHaveCount(0);
    await expect(page.locator(".rh-toasts")).not.toContainText("Unsupported");
    await expect(page.locator("body")).not.toContainText("hidden-token");
  } finally { await dispose(server); }
});

test("late replies cannot cross focused burrows or account changes", async ({ context, page }) => {
  const a = await feedBurrow("Feeds Alpha", { "https://example.test/alpha?token=alpha-secret": "alpha-board" });
  const b = await feedBurrow("Feeds Beta", { "https://example.test/beta?token=beta-secret": "beta-board" });
  try {
    // Delay one real reply at the native message boundary, before the WASM
    // listener runs. Saving the exact socket avoids conflating this test with
    // bookmark resume's additional connections. No auth payload is recorded.
    await context.addInitScript(endpoint => {
      const Native = window.WebSocket;
      (window as any).__feedMappingReplies = 0;
      (window as any).__feedSockets = [] as WebSocket[];
      (window as any).__feedHold = false;
      (window as any).__feedHeld = undefined;
      window.WebSocket = class extends Native {
        constructor(url: string | URL, protocols?: string | string[]) {
          super(url, protocols);
          (window as any).__feedSockets.push(this);
          this.addEventListener("message", event => {
            if (!(event.data instanceof ArrayBuffer)) return;
            const prefix = new Uint8Array(event.data);
            if (prefix[0] !== 1 || prefix[1] !== 1 || prefix[2] !== 7 || prefix[3] !== 66) return;
            if ((window as any).__feedHold && new URL(this.url).href === new URL(endpoint).href) {
              (window as any).__feedHold = false;
              (window as any).__feedHeld = { socket: this, data: event.data.slice(0) };
              event.stopImmediatePropagation();
              return;
            }
            (window as any).__feedMappingReplies++;
          });
        }
      };
    }, a.wsURL);
    const received = () => page.evaluate(() => (window as any).__feedMappingReplies as number);
    const holdNextReply = async () => {
      await page.evaluate(() => {
        (window as any).__feedHeld = undefined;
        (window as any).__feedHold = true;
      });
      await refresh(page);
      await expect.poll(() => page.evaluate(() => !!(window as any).__feedHeld)).toBe(true);
    };
    await page.goto(a.httpURL); await signIn(page, a); await monitor(page);
    await expect(row(page, "alpha-board")).toBeVisible();
    // Finish both authentication/bookmark flows first; adding a bookmark can
    // resume a session and replace its socket. This seam isolates pure focus.
    await addBurrow(page, b); await monitor(page);
    await expect(row(page, "beta-board")).toBeVisible();
    await page.locator('.rh-rail-server[aria-label^="Feeds Alpha —"]').click();
    await monitor(page); await expect(row(page, "alpha-board")).toBeVisible();
    await holdNextReply();
    await page.locator('.rh-rail-server[aria-label^="Feeds Beta —"]').click();
    await monitor(page);
    await expect(row(page, "beta-board")).toBeVisible();
    const before = await received();
    expect(await page.evaluate(() => {
      const held = (window as any).__feedHeld as { socket: WebSocket; data: ArrayBuffer };
      if (held.socket.readyState !== WebSocket.OPEN) return false;
      held.socket.dispatchEvent(new MessageEvent("message", { data: held.data.slice(0) }));
      return true;
    })).toBe(true);
    await expect.poll(received).toBeGreaterThan(before);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await expect(row(page, "beta-board")).toBeVisible();
    await expect(row(page, "alpha-board")).toHaveCount(0);

    await page.locator('.rh-rail-server[aria-label^="Feeds Alpha —"]').click();
    await monitor(page); await expect(row(page, "alpha-board")).toBeVisible();
    // A pending refresh from the admin session must be discarded on Leave.
    await holdNextReply();
    await page.getByRole("button", { name: "Leave", exact: true }).click();
    await page.getByRole("alertdialog", { name: `Leave ${a.name}?`, exact: true }).getByRole("button", { name: "Leave", exact: true }).click();
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    await signIn(page, a, "theme-viewer");
    // Redeliver the captured old frame at the new socket boundary. No request
    // or global admin callback on this ordinary account may adopt its rows.
    const beforeAccount = await received();
    expect(await page.evaluate(endpoint => {
      const held = (window as any).__feedHeld as { socket: WebSocket; data: ArrayBuffer };
      const socket = ((window as any).__feedSockets as WebSocket[]).findLast(socket =>
        new URL(socket.url).href === new URL(endpoint).href && socket.readyState === WebSocket.OPEN);
      if (!socket || socket === held.socket) return false;
      socket.dispatchEvent(new MessageEvent("message", { data: held.data.slice(0) }));
      return true;
    }, a.wsURL)).toBe(true);
    await expect.poll(received).toBeGreaterThan(beforeAccount);
    await page.evaluate(() => { history.pushState(null, "", "/admin/federation"); window.dispatchEvent(new PopStateEvent("popstate")); });
    await expect(page.getByText("Operators only", { exact: true })).toBeVisible();
    await expect(table(page)).toHaveCount(0);
    await expect(page.locator("body")).not.toContainText("alpha-board");
    await expect(page.locator("body")).not.toContainText("beta-board");
  } finally { await dispose(b); await dispose(a); }
});
