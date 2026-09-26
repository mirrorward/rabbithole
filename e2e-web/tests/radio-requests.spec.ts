import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { TestBurrow } from "../fixtures/burrow";
import { RadioFixture } from "../fixtures/radio";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for queue integration tests");
test.skip(process.platform === "win32", "The server's ctl surface is Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

async function localOnly(context: BrowserContext) {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), (route) => route.abort());
}

// Seed library metadata with delivery disabled, then use real source-ingest
// sessions to announce/hold the mounts. These exercise real queue/runtime/
// protocol/UI behavior, not audio decoding or upload flows.
async function library(server: TestBurrow) {
  await server.stop();
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(join(server.dataDir, "burrow.db"));
  try {
    for (const [slug, base] of [["music", 0], ["other", 10]] as const) {
      db.prepare("INSERT INTO file_areas(slug, title, created_at) VALUES (?, ?, 1)").run(slug, slug);
      const area = db.prepare("SELECT id FROM file_areas WHERE slug = ?").get(slug)!.id;
      for (let id = 1; id <= 6; id++) {
        db.prepare("INSERT INTO file_nodes(id,area_id,kind,name,path,blob_id,size,mime,comment,created_at) VALUES (?,?,1,?,?,?,0,'audio/mpeg','The Lagomorphs',1)")
          .run(base + id, area, `song-${id}.mp3`, `song-${id}.mp3`, Buffer.alloc(32, base + id));
      }
    }
  } finally { db.close(); }
  const path = join(server.dataDir, "burrow.toml");
  await writeFile(path, `radio_library_areas = { jukebox = "music", quiet = "other" }\n${(await readFile(path, "utf8")).replace("radio_enabled = true", "radio_enabled = false")}`);
  await server.start();
  await server.ctl("account-create", "listener-two", "theme-e2e-password", "user");
  const source = await RadioFixture.create(server, [["jukebox", "jukebox (library)"], ["quiet", "quiet (library)"]]);
  await source.metadata("jukebox", "song-1.mp3");
  await source.metadata("quiet", "song-1.mp3");
  return source;
}

async function signIn(page: Page, server: TestBurrow, handle: string, navigate = true) {
  if (navigate) await page.goto(server.httpURL);
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(handle);
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.getByLabel("Save sign-in to bookmark").check();
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await page.locator('.rh-subnav a[href="/radio"]').click();
  await expect(page.locator(".rh-requests")).toContainText("Nothing asked for");
  await expect(page.getByRole("button", { name: "Ask for a song", exact: true })).toBeVisible();
}

const row = (page: Page, song: number) => page.locator(".rh-request").filter({ hasText: `song-${song}.mp3` });
async function ask(page: Page, song: number) {
  const button = page.getByRole("button", { name: "Ask for a song", exact: true });
  if (await button.isVisible()) await button.click();
  await page.getByRole("searchbox", { name: "Find a song to ask for", exact: true }).fill(`song-${song}`);
  await page.locator(".rh-requests-offer li").filter({ hasText: `song-${song}.mp3` }).getByRole("button").click();
  await expect(row(page, song)).toContainText("You asked for it");
}

// Observe only the routing header, never any authentication payload.
function radioFrames(page: Page) {
  const seen = { reads: 0, watches: 0, pushes: 0 };
  page.on("websocket", (socket) => {
    socket.on("framesent", ({ payload }) => {
      if (!Buffer.isBuffer(payload) || payload[0] !== 1 || payload[1] !== 0 || payload[2] !== 9) return;
      if (payload[3] === 7) seen.reads++;
      if (payload[3] === 13) seen.watches++;
    });
    socket.on("framereceived", ({ payload }) => {
      if (Buffer.isBuffer(payload) && payload[0] === 1 && payload[1] === 2 && payload[2] === 9 && payload[3] === 8) seen.pushes++;
    });
  });
  return seen;
}

test("two listeners see requests and votes live, resume their watch, and clear a disappeared station", async ({ browser, context, page }) => {
  const other = await browser.newContext({ serviceWorkers: "block" });
  const second = await other.newPage();
  await localOnly(context); await localOnly(other);
  await context.addInitScript(() => {
    const Native = window.WebSocket;
    (window as any).__queueSockets = [] as WebSocket[];
    window.WebSocket = class extends Native {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols); (window as any).__queueSockets.push(this);
      }
    };
  });
  const server = await TestBurrow.create("Queue Warren", "a34700", { radio: true });
  let source: RadioFixture | undefined;
  const errors: string[] = [];
  for (const client of [page, second]) client.on("pageerror", (error) => errors.push(error.message));
  const firstWire = radioFrames(page), secondWire = radioFrames(second);
  try {
    source = await library(server);
    await signIn(page, server, "theme-viewer");
    await signIn(second, server, "listener-two");
    const title = await page.locator(".rh-player-title").textContent();
    const firstReads = firstWire.reads, secondReads = secondWire.reads;
    await ask(page, 4);
    await expect(row(second, 4)).toContainText("1 person wants it");
    await row(second, 4).getByRole("button", { name: "Me too", exact: true }).click();
    for (const client of [page, second]) await expect(row(client, 4)).toContainText("2 want it, you included");
    expect(firstWire.reads).toBe(firstReads); expect(secondWire.reads).toBe(secondReads);
    expect(firstWire.pushes).toBeGreaterThan(0); expect(secondWire.pushes).toBeGreaterThan(0);
    await expect(page.locator(".rh-player-title")).toHaveText(title!);
    await expect(page).toHaveURL(/\/radio$/); await expect(second).toHaveURL(/\/radio$/);

    // Only the selected station is watched. A switch does not retain its queue.
    await second.getByRole("button", { name: /quiet \(library\)/ }).click();
    await expect(row(second, 4)).toHaveCount(0);
    const quietPushes = secondWire.pushes;
    await ask(page, 3);
    await expect(row(page, 3)).toBeVisible();
    // A server round trip after the mutation establishes ordering.
    await ask(second, 2);
    await expect(row(second, 3)).toHaveCount(0);
    expect(secondWire.pushes - quietPushes).toBeLessThanOrEqual(1);
    await second.getByRole("button", { name: /jukebox \(library\)/ }).click();
    await expect(row(second, 3)).toContainText("1 person wants it");

    // Drop one real socket while the other listener adds another request.
    await context.setOffline(true);
    // Chromium's offline emulation does not reliably close an established
    // WebSocket. Close the native connection, not a synthetic close event.
    await page.evaluate(() => {
      for (const socket of (window as any).__queueSockets as WebSocket[]) socket.close(1000, "fixture disconnect");
    });
    await expect(page.locator(".rh-request")).toHaveCount(0);
    await ask(second, 5);
    const watches = firstWire.watches;
    await context.setOffline(false);
    await expect.poll(() => firstWire.watches).toBeGreaterThan(watches);
    await expect(row(page, 5)).toContainText("1 person wants it");
    await expect(row(page, 4)).toContainText("2 want it, you included");

    // A real restart with the selected mount removed exercises reauth plus
    // a fresh listing with no such station; neither cached queue nor offers survive.
    await server.stop();
    await source.dispose(); source = undefined;
    const config = join(server.dataDir, "burrow.toml");
    await writeFile(config, (await readFile(config, "utf8")).replace('jukebox = "music", ', ""));
    await server.start();
    source = await RadioFixture.create(server, [["quiet", "quiet (library)"]]);
    await expect(page.getByRole("button", { name: /jukebox \(library\)/ })).toHaveCount(0);
    await expect(page.locator(".rh-request")).toHaveCount(0);
    await expect(page.locator(".rh-requests")).toContainText("Nothing asked for");
    expect(errors).toEqual([]);
  } finally {
    await other.close(); await source?.dispose(); await server.dispose();
    await test.info().attach("queue-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("late queue and sign-off messages from another burrow cannot replace the radio owner's station", async ({ context, page }) => {
  await localOnly(context);
  const alpha = await TestBurrow.create("Radio Alpha", "a34700", { radio: true });
  const beta = await TestBurrow.create("Radio Beta", "235b96", { radio: true });
  const sources: RadioFixture[] = [];
  let releaseQueue: (() => void) | undefined, signOff: (() => void) | undefined;
  try {
    sources.push(await library(alpha)); sources.push(await library(beta));
    await context.routeWebSocket(alpha.wsURL, (route) => {
      const upstream = route.connectToServer();
      // A bounded routing seam: a correctly encoded RADIO2 push on Alpha's
      // socket, while Beta owns the displayed station of the same name.
      const station = Buffer.from("jukebox");
      signOff = () => route.send(Buffer.concat([Buffer.from([1, 2, 9, 2, 0, 0, station.length + 1, station.length]), station]));
      upstream.onMessage((message) => {
        if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 1 && message[2] === 9 && message[3] === 8) {
          releaseQueue = () => route.send(message);
        }
        route.send(message);
      });
    });
    await signIn(page, alpha, "theme-viewer");
    await ask(page, 4);
    const oldQueue = releaseQueue!;
    await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
    await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
    await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(beta.name);
    await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(beta.wsURL);
    await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
    await page.locator(".rh-glass-row").filter({ hasText: beta.name }).last().click();
    await page.getByRole("button", { name: "Connect…", exact: true }).click();
    await signIn(page, beta, "theme-viewer", false);
    await ask(page, 3);
    oldQueue(); signOff!();
    await ask(page, 2); // a subsequent real Beta round trip
    await expect(row(page, 3)).toContainText("You asked for it");
    await expect(row(page, 4)).toHaveCount(0);
    await expect(page.getByRole("button", { name: /jukebox \(library\)/ })).toBeVisible();
    await expect(page.locator(".rh-player-title")).toHaveText("song-1.mp3");
    await expect(page.locator(".rh-header .rh-title-text")).toHaveText(beta.name);
  } finally {
    for (const source of sources) await source.dispose();
    for (const server of [alpha, beta]) {
      await server.dispose();
      await test.info().attach(`${server.name}-log`, { body: server.logs(), contentType: "text/plain" });
    }
  }
});

test("unsupported watches keep the ordinary queue path without error toasts; late station replies stay discarded", async ({ context, page }) => {
  await localOnly(context);
  const server = await TestBurrow.create("Older Radio Peer", "a34700", { radio: true });
  let source: RadioFixture | undefined;
  let rejected = 0, release: (() => void) | undefined;
  try {
    source = await library(server);
    await context.routeWebSocket(server.wsURL, (route) => {
      const upstream = route.connectToServer();
      route.onMessage((message) => {
        if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 0 && message[2] === 9 && message[3] === 13) {
          let end = 4; while (message[end++] & 0x80) { /* request id */ }
          const prefix = Buffer.from(message.subarray(0, end)); prefix[1] = 1;
          route.send(Buffer.concat([prefix, Buffer.from([1, 8, 0])])); rejected++;
        } else upstream.send(message);
      });
      upstream.onMessage((message) => {
        // Keep a copy of a real queue reply; release it only after switching
        // station. Re-delivery models an old pane's outstanding response.
        if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 1 && message[2] === 9 && message[3] === 8) {
          release = () => route.send(message);
        }
        route.send(message);
      });
    });
    await signIn(page, server, "theme-viewer");
    await expect.poll(() => rejected).toBeGreaterThan(0);
    await ask(page, 4);
    const oldReply = release!;
    await page.getByRole("button", { name: /quiet \(library\)/ }).click();
    await expect(page.locator(".rh-requests")).toContainText("Nothing asked for");
    oldReply();
    await ask(page, 2); // subsequent round-trip sentinel
    await expect(row(page, 4)).toHaveCount(0);
    await expect(page.locator(".rh-toasts")).not.toContainText("Unsupported");
  } finally {
    await source?.dispose(); await server.dispose();
    await test.info().attach("older-queue-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
