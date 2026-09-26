import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the real-server authentication regressions");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
// Authentication frames contain credentials. Keep traces off and record only
// the protocol header, even though every account here belongs to a fixture.
test.use({ serviceWorkers: "block", actionTimeout: 15_000, trace: "off", screenshot: "off" });
test.setTimeout(60_000);

type Header = { kind: number; family: number; type: number; id: string; error: number | null };

/** Read the postcard frame header and stop before its opaque payload. */
function header(message: string | Buffer): Header {
  if (!Buffer.isBuffer(message)) throw new Error("Expected a binary RHP frame");
  let offset = 0;
  function unsigned(): bigint {
    let value = 0n;
    for (let shift = 0n; shift < 70n; shift += 7n) {
      if (offset === message.length) throw new Error("Truncated frame header");
      const byte = message[offset++];
      value |= BigInt(byte & 0x7f) << shift;
      if (!(byte & 0x80)) return value;
    }
    throw new Error("Oversized frame header integer");
  }
  if (unsigned() !== 1n) throw new Error("Expected RHP version 1");
  const kind = Number(unsigned());
  const family = Number(unsigned());
  const type = Number(unsigned());
  const id = unsigned().toString();
  const error = unsigned() === 0n ? null : Number(unsigned());
  return { kind, family, type, id, error };
}

// Reply to an unrelated RoomList request: version 1, Reply, CHAT, type 10,
// id 777 (postcard varint), Some(Unauthenticated), empty opaque payload.
const unrelatedError = Buffer.from([1, 1, 2, 10, 0x89, 0x06, 1, 1, 0]);

async function isolate(context: BrowserContext) {
  await context.route("**/*", (route) => {
    const host = new URL(route.request().url()).hostname;
    return ["127.0.0.1", "localhost", "[::1]"].includes(host)
      ? route.continue() : route.abort();
  });
}

async function signIn(page: Page, server: TestBurrow, password: string, save = false) {
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill("theme-viewer");
  await page.locator("#rh-login-password").fill(password);
  if (save) await page.getByRole("checkbox", { name: "Save sign-in to bookmark", exact: true }).check();
  await page.locator('.rh-login button[type="submit"]').click();
}

async function expectSignedIn(page: Page, server: TestBurrow) {
  // The title and green transport dot can precede AuthOk. A real roster row
  // proves the authenticated requests were accepted and rendered.
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await expect(page.locator(".rh-present .rh-who-name").filter({ hasText: /^theme-viewer$/ }))
    .toBeVisible();
  await expect(page.locator("#rh-login-handle")).toHaveCount(0);
}

test("an unrelated early error cannot reject password login or erase a saved session", async ({ context, page }, info) => {
  const server = await TestBurrow.create("Authentication with agreement", "704099");
  const frames: { direction: string; connection: number; header: Header }[] = [];
  const gates: { release: () => void }[] = [];
  let connections = 0;
  try {
    await server.ctl("config-set", "agreement", "Fixture agreement: be kind to one another.");
    await isolate(context);
    await context.routeWebSocket(server.wsURL, (route) => {
      const connection = ++connections;
      const upstream = route.connectToServer();
      const held: (string | Buffer)[] = [];
      let holding = false;
      route.onMessage((message) => {
        frames.push({ direction: "sent", connection, header: header(message) });
        upstream.send(message);
      });
      upstream.onMessage((message) => {
        const metadata = header(message);
        frames.push({ direction: "received", connection, header: metadata });
        if (metadata.kind === 1 && metadata.family === 0 && metadata.type === 13 && metadata.error === null) {
          holding = true;
          held.push(message);
          gates.push({ release: () => {
            holding = false;
            for (const queued of held.splice(0)) route.send(queued);
          } });
          // The server really accepted the account. Delay that success and
          // deliver a different request's failure first, as an early pane can.
          route.send(unrelatedError);
        } else if (holding) {
          held.push(message);
        } else {
          route.send(message);
        }
      });
    });

    // Mark completion of the injected message's browser task, after every
    // application listener and its microtasks. This avoids sleeping or merely
    // checking that the test proxy sent bytes the application has not seen.
    await context.addInitScript(({ bytes }) => {
      const Native = window.WebSocket;
      window.WebSocket = class extends Native {
        constructor(url: string | URL, protocols?: string | string[]) {
          super(url, protocols);
          this.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer)) return;
            const received = new Uint8Array(event.data);
            if (received.length !== bytes.length || !bytes.every((byte, i) => received[i] === byte)) return;
            setTimeout(() => { (window as any).__rhAuthErrorProcessed = true; }, 0);
          });
        }
      } as typeof WebSocket;
    }, { bytes: [...unrelatedError] });

    await page.goto(server.httpURL);
    await signIn(page, server, "theme-e2e-password", true);
    await expect.poll(() => gates.length).toBe(1);
    await page.waitForFunction(() => (window as any).__rhAuthErrorProcessed === true);
    await expect(page.locator("#rh-login-handle")).toHaveCount(0);
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Send", exact: true })).toBeDisabled();
    gates[0].release();
    await expectSignedIn(page, server);
    await page.getByRole("button", { name: "Accept & enter", exact: true }).click();
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeEnabled();

    await page.reload();
    await expect.poll(() => gates.length).toBe(2);
    await page.waitForFunction(() => (window as any).__rhAuthErrorProcessed === true);
    await expect(page.locator("#rh-login-handle")).toHaveCount(0);
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Send", exact: true })).toBeDisabled();
    // Return only whether the fixture token survived; never read it into test
    // output or inspect an authentication payload.
    expect(await page.evaluate((endpoint) => {
      const bookmarks = JSON.parse(localStorage.getItem("rh.bookmarks.v1") ?? "[]");
      return bookmarks.some((entry: any) => new URL(entry.endpoint).href === new URL(endpoint).href
        && entry.login === "theme-viewer" && !!entry.token);
    }, server.wsURL)).toBe(true);
    gates[1].release();
    await expectSignedIn(page, server);
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeEnabled();
    expect(frames.filter((frame) => frame.direction === "sent" && frame.header.family === 0
      && frame.header.type === 10)).toHaveLength(1);
    expect(frames.filter((frame) => frame.direction === "sent" && frame.header.family === 0
      && frame.header.type === 12)).toHaveLength(1);
  } finally {
    for (const gate of gates) gate.release();
    await info.attach("frame-headers-only", { body: JSON.stringify(frames, null, 2), contentType: "application/json" });
    await server.dispose();
  }
});

test("a genuine bad password is rejected and a corrected retry signs in", async ({ context, page }, info) => {
  const server = await TestBurrow.create("Password retry fixture", "235b96");
  const frames: { direction: string; header: Header }[] = [];
  try {
    await isolate(context);
    page.on("websocket", (socket) => {
      if (new URL(socket.url()).href !== new URL(server.wsURL).href) return;
      for (const direction of ["framesent", "framereceived"] as const) {
        socket.on(direction, ({ payload }) => frames.push({ direction, header: header(payload) }));
      }
    });
    await page.goto(server.httpURL);
    await signIn(page, server, "fixture-intentionally-wrong");
    await expect(page.locator(".rh-login-notice")).toContainText("didn’t accept that handle and password");
    expect(frames.some((frame) => frame.direction === "framereceived" && frame.header.family === 0
      && frame.header.type === 10 && frame.header.error === 1)).toBe(true);
    await signIn(page, server, "theme-e2e-password");
    await expectSignedIn(page, server);
    expect(frames.some((frame) => frame.direction === "framereceived" && frame.header.family === 0
      && frame.header.type === 13 && frame.header.error === null)).toBe(true);
  } finally {
    await info.attach("frame-headers-only", { body: JSON.stringify(frames, null, 2), contentType: "application/json" });
    await server.dispose();
  }
});

for (const pane of ["boards", "threads"] as const) test(`${pane} opened during the handshake waits for authentication and then refreshes`, async ({ context, page }, info) => {
  const server = await TestBurrow.create("Pending handshake fixture", "1d6b58");
  const frames: { direction: string; header: Header }[] = [];
  let release: (() => void) | undefined;
  try {
    await server.ctl("board-create", "auth-board", "Authenticated board");
    if (pane === "threads") {
      await server.ctl("board-post", "auth-board", "theme-viewer", "Thread loaded after authentication", "Fixture thread body.");
    }
    await isolate(context);
    await context.routeWebSocket(server.wsURL, (route) => {
      const upstream = route.connectToServer();
      const held: (string | Buffer)[] = [];
      let holding = false;
      route.onMessage((message) => {
        frames.push({ direction: "sent", header: header(message) });
        upstream.send(message);
      });
      upstream.onMessage((message) => {
        const metadata = header(message);
        frames.push({ direction: "received", header: metadata });
        if (metadata.family === 0 && metadata.type === (pane === "threads" ? 13 : 2)) {
          holding = true;
          held.push(message);
          release = () => {
            holding = false;
            for (const queued of held.splice(0)) route.send(queued);
            release = undefined;
          };
        } else if (holding) {
          held.push(message);
        } else {
          route.send(message);
        }
      });
    });
    await page.goto(server.httpURL);
    await signIn(page, server, "theme-e2e-password");
    await expect.poll(() => !!release).toBe(true);
    if (pane === "threads") {
      // Exercise the nested route without reloading the document: startup
      // intentionally restores saved sessions to /lobby. The pending socket
      // and held AuthOk must survive this client-side history navigation.
      await page.evaluate(() => {
        history.pushState(null, "", "/boards/auth-board");
        dispatchEvent(new PopStateEvent("popstate"));
      });
      await expect(page.getByRole("region", { name: "Threads", exact: true })).toBeVisible();
      await expect(page.getByRole("button", { name: "New thread", exact: true })).toBeDisabled();
    } else {
      await page.locator('.rh-subnav a[href="/boards"]').click();
      await expect(page.getByRole("heading", { name: "Boards", exact: true })).toBeVisible();
    }
    // Allow the mounted pane's render effects to finish while its handshake
    // reply stays held. Reads must wait for account authentication.
    await page.evaluate(() => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    }));
    expect(frames.filter((frame) => frame.direction === "sent" && frame.header.family === 4))
      .toHaveLength(0);
    await expect(page.locator("#rh-login-handle")).toHaveCount(0);
    release!();
    if (pane === "threads") {
      await expect(page.getByRole("heading", { name: "Authenticated board", exact: true })).toBeVisible();
      await expect(page.locator(".rh-thread-title")).toHaveText(["Thread loaded after authentication"]);
      await expect(page).toHaveURL(`${server.httpURL}/boards/auth-board`);
      expect(frames.some((frame) => frame.direction === "received" && frame.header.family === 4
        && frame.header.type === 4 && frame.header.error === null)).toBe(true);
    } else {
      await expect(page.locator(".rh-board-name").filter({ hasText: /^Authenticated board$/ }))
        .toBeVisible();
    }
    expect(frames.some((frame) => frame.direction === "received" && frame.header.family === 0
      && frame.header.type === 13 && frame.header.error === null)).toBe(true);
    expect(frames.some((frame) => frame.direction === "received" && frame.header.error !== null))
      .toBe(false);
  } finally {
    release?.();
    await info.attach("frame-headers-only", { body: JSON.stringify(frames, null, 2), contentType: "application/json" });
    await server.dispose();
  }
});
