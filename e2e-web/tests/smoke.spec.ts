import { test, expect } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

const isolated = !!process.env.BURROW_BIN && !!process.env.SPA_DIST;
test.skip(isolated && process.platform === "win32", "The fixture's ctl surface is Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(60_000);

test("SPA boots and a real guest sign-in reaches the lobby", async ({ context, page }) => {
  // CI always supplies artifacts. Without them, preserve the manual smoke
  // against BASE_URL and optionally WS_URL from the developer's environment.
  const server = isolated ? await TestBurrow.create("Guest Smoke", "a34700") : undefined;
  const auth = { guest: 0, accepted: 0 };
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", (socket) => {
    socket.on("framesent", ({ payload }) => {
      if (Buffer.isBuffer(payload) && payload[0] === 1 && payload[1] === 0 && payload[2] === 0 && payload[3] === 11) {
        auth.guest += 1;
      }
    });
    socket.on("framereceived", ({ payload }) => {
      if (Buffer.isBuffer(payload) && payload[0] === 1 && payload[1] === 1 && payload[2] === 0 && payload[3] === 13) {
        auth.accepted += 1;
      }
    });
  });
  try {
    if (server) {
      await context.route("**/*", (route) => {
        const host = new URL(route.request().url()).hostname;
        return ["127.0.0.1", "localhost", "[::1]"].includes(host)
          ? route.continue() : route.abort();
      });
    }
    expect((await page.goto(server?.httpURL ?? "/"))?.status()).toBe(200);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    const endpoint = server?.wsURL ?? process.env.WS_URL;
    if (endpoint) await page.locator("#rh-login-server").fill(endpoint);
    await page.locator("#rh-login-handle").fill("guest-e2e");
    await page.locator("#rh-login-password").fill("");
    await page.locator('.rh-login button[type="submit"]').click();
    // Frame types prove a real server accepted AuthGuest; a seeded demo or
    // merely rendering the lobby cannot satisfy these assertions.
    await expect.poll(() => auth.guest).toBeGreaterThan(0);
    await expect.poll(() => auth.accepted).toBeGreaterThan(0);
    const nav = page.getByRole("navigation", { name: "Primary", exact: true });
    await expect(nav.getByRole("link", { name: "Lobby", exact: true })).toBeVisible();
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
    await expect(page.locator(".rh-header .rh-dot.on")).toBeVisible();
    if (server) await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    expect(errors).toEqual([]);
  } finally {
    await server?.dispose();
    await test.info().attach("guest-diagnostics", {
      body: JSON.stringify({ auth, errors, server: server?.logs() }, null, 2),
      contentType: "application/json",
    });
  }
});
