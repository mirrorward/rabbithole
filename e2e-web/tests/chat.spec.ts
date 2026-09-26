import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the real-server chat history regression");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

async function localOnly(context: BrowserContext) {
  await context.route("**/*", (route) => {
    const host = new URL(route.request().url()).hostname;
    return ["127.0.0.1", "localhost", "[::1]"].includes(host) ? route.continue() : route.abort();
  });
}

async function signIn(page: Page, server: TestBurrow, handle = "theme-viewer") {
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(handle);
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.getByLabel("Save sign-in to bookmark").check();
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
}

function lines(page: Page) {
  return page.getByRole("log", { name: "Chat messages" });
}

async function say(page: Page, room: string, text: string) {
  const compose = page.getByRole("textbox", { name: `Message #${room}`, exact: true });
  await compose.fill(text);
  await compose.press("Enter");
  await expect(lines(page).getByText(text, { exact: true }).first()).toBeVisible();
}

async function makeRoom(page: Page, room: string, privateRoom = false) {
  await page.getByRole("button", { name: "New room", exact: true }).click();
  await page.getByPlaceholder("What to call it", { exact: true }).fill(room);
  if (privateRoom) await page.getByRole("checkbox", { name: "Private — only people asked in" }).check();
  await page.getByRole("button", { name: "Make it", exact: true }).click();
  await expect(page.getByRole("tab", { name: room, exact: !privateRoom })).toHaveAttribute("aria-selected", "true");
}

test("a second client backfills rooms, reconciles live messages and reconnects without leaking burrows", async ({ browser, context, page }) => {
  const servers: TestBurrow[] = [];
  const extraContexts: BrowserContext[] = [];
  const errors: string[] = [];
  const events: string[] = [];
  let histories = 0;
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", (socket) => {
    events.push(`open ${socket.url()}`);
    socket.on("close", () => events.push(`close ${socket.url()}`));
    socket.on("framereceived", ({ payload }) => {
      if (!Buffer.isBuffer(payload)) return;
      // Inspect only frame type prefixes, never auth credentials or tokens.
      if (payload[0] === 1 && payload[1] === 1 && payload[2] === 2 && payload[3] === 4) {
        histories += 1;
        events.push(`history ${socket.url()}`);
      }
    });
  });
  // Close the actual socket to exercise the app's existing reconnect path.
  // The wrapper neither changes frames nor substitutes a transport/server.
  await page.addInitScript(() => {
    const sockets: WebSocket[] = [];
    const NativeSocket = window.WebSocket;
    window.WebSocket = class extends NativeSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        sockets.push(this);
      }
    };
    (window as unknown as { closeChatSockets: () => void }).closeChatSockets = () => {
      for (const socket of sockets) if (socket.readyState === WebSocket.OPEN) socket.close();
    };
  });

  try {
    const alpha = await TestBurrow.create("Chat Alpha", "a34700");
    servers.push(alpha);
    await alpha.ctl("account-create", "history-reader", "theme-e2e-password", "user");
    const writerContext = await browser.newContext({ serviceWorkers: "block" });
    extraContexts.push(writerContext);
    await localOnly(writerContext);
    const writer = await writerContext.newPage();
    writer.on("pageerror", (error) => errors.push(`writer: ${error.message}`));
    await writer.goto(alpha.httpURL);
    await signIn(writer, alpha);
    await say(writer, "lobby", "lobby before the second client");
    await say(writer, "lobby", "another earlier lobby line");
    await makeRoom(writer, "archive-room");
    await say(writer, "archive-room", "room before the second client");
    await say(writer, "archive-room", "same text sent twice");
    await say(writer, "archive-room", "same text sent twice");
    await expect(lines(writer).getByText("same text sent twice", { exact: true })).toHaveCount(2);
    await makeRoom(writer, "hidden-room", true);
    await say(writer, "hidden-room", "private room stays private");

    await localOnly(context);
    await page.goto(alpha.httpURL);
    await signIn(page, alpha, "history-reader");
    await expect.poll(() => histories).toBeGreaterThan(0);
    await expect(lines(page).locator(".rh-line-text")).toHaveText([
      "lobby before the second client", "another earlier lobby line",
    ]);
    await expect(lines(page)).not.toContainText("room before the second client");
    await expect(page.getByRole("tab", { name: /hidden-room/ })).toHaveCount(0);
    await expect(lines(page)).not.toContainText("private room stays private");

    const beforeRoom = histories;
    await page.getByRole("tab", { name: "archive-room", exact: true }).click();
    await expect.poll(() => histories).toBeGreaterThan(beforeRoom);
    await expect(lines(page).locator(".rh-line-text")).toHaveText([
      "room before the second client", "same text sent twice", "same text sent twice",
    ]);
    await expect(lines(page)).not.toContainText("lobby before the second client");
    await writer.getByRole("tab", { name: "archive-room", exact: true }).click();
    await say(writer, "archive-room", "live after the history");
    await expect(lines(page).getByText("live after the history", { exact: true })).toHaveCount(1);

    // Reopening refetches the same snapshot without multiplying old lines.
    await page.getByRole("tab", { name: "lobby", exact: true }).click();
    await expect(lines(page)).not.toContainText("live after the history");
    const beforeReopen = histories;
    await page.getByRole("tab", { name: "archive-room", exact: true }).click();
    await expect.poll(() => histories).toBeGreaterThan(beforeReopen);
    await expect(lines(page).locator(".rh-line-text")).toHaveCount(4);
    await expect(lines(page).getByText("same text sent twice", { exact: true })).toHaveCount(2);

    // The writer keeps the room alive while the reader misses a live push.
    // Authentication and successful rejoin must recover that gap from history.
    const beforeReconnect = histories;
    await context.setOffline(true);
    await page.evaluate(() => (window as unknown as { closeChatSockets: () => void }).closeChatSockets());
    await expect(page.locator(".rh-header .rh-dot.on")).toHaveCount(0);
    await say(writer, "archive-room", "written while the reader was offline");
    await context.setOffline(false);
    await expect.poll(() => histories, { timeout: 30_000 }).toBeGreaterThan(beforeReconnect + 1);
    await expect(lines(page).getByText("written while the reader was offline", { exact: true })).toHaveCount(1);
    await expect(lines(page).locator(".rh-line-text")).toHaveCount(5);

    // A fresh document has no in-memory lines; token resume also backfills.
    const beforeReload = histories;
    await page.reload();
    await expect.poll(() => histories).toBeGreaterThan(beforeReload);
    await expect(lines(page).locator(".rh-line-text")).toHaveText([
      "lobby before the second client", "another earlier lobby line",
    ]);

    const beta = await TestBurrow.create("Chat Beta", "235b96");
    servers.push(beta);
    const betaContext = await browser.newContext({ serviceWorkers: "block" });
    extraContexts.push(betaContext);
    await localOnly(betaContext);
    const betaWriter = await betaContext.newPage();
    await betaWriter.goto(beta.httpURL);
    await signIn(betaWriter, beta);
    await say(betaWriter, "lobby", "only the other burrow");
    await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
    await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
    await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(beta.name);
    await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(beta.wsURL);
    await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
    await page.locator(".rh-glass-row").filter({ hasText: beta.name }).click();
    await page.getByRole("button", { name: "Connect…", exact: true }).click();
    await signIn(page, beta);
    await expect(lines(page).locator(".rh-line-text")).toHaveText(["only the other burrow"]);
    await page.locator('.rh-rail-server[aria-label^="Chat Alpha —"]').click();
    await expect(lines(page).locator(".rh-line-text")).toHaveText([
      "lobby before the second client", "another earlier lobby line",
    ]);
    expect(errors).toEqual([]);
  } finally {
    await context.setOffline(false);
    await test.info().attach("chat-socket-events", { body: events.join("\n"), contentType: "text/plain" });
    for (const extra of extraContexts.reverse()) await extra.close();
    for (const server of servers.reverse()) {
      await server.dispose();
      await test.info().attach(`${server.name}-server-log`, { body: server.logs(), contentType: "text/plain" });
    }
  }
});
