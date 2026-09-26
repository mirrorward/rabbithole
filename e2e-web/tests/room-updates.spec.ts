import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to test live room updates against an isolated server");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

async function localOnly(context: BrowserContext) {
  await context.route("**/*", (route) => {
    const host = new URL(route.request().url()).hostname;
    return ["127.0.0.1", "localhost", "[::1]"].includes(host) ? route.continue() : route.abort();
  });
}

async function signIn(page: Page, server: TestBurrow, handle: string) {
  await page.goto(server.httpURL);
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(handle);
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
}

async function makeRoom(page: Page, name: string, privateRoom = false) {
  await page.getByRole("button", { name: "New room", exact: true }).click();
  await page.getByPlaceholder("What to call it", { exact: true }).fill(name);
  if (privateRoom) await page.getByRole("checkbox", { name: "Private — only people asked in" }).check();
  await page.getByRole("button", { name: "Make it", exact: true }).click();
  await expect(page.getByRole("textbox", { name: `Message #${name}`, exact: true })).toBeVisible();
}

test("room lists and an open keeper roster follow another live client without refreshing", async ({ browser, context, page }) => {
  const server = await TestBurrow.create("Live Rooms", "a34700");
  let visitorContext: BrowserContext | undefined;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await server.ctl("account-create", "room-visitor", "theme-e2e-password", "user");
    visitorContext = await browser.newContext({ serviceWorkers: "block" });
    await localOnly(context);
    await localOnly(visitorContext);
    const visitor = await visitorContext.newPage();
    visitor.on("pageerror", (error) => errors.push(`visitor: ${error.message}`));
    await signIn(page, server, "theme-viewer");
    await signIn(visitor, server, "room-visitor");

    await makeRoom(page, "live-den");
    // This page stays in the lobby while another client creates the room.
    await expect(visitor.getByRole("tab", { name: "live-den", exact: true })).toBeVisible();
    await expect(visitor.getByRole("tab", { name: "lobby", exact: true })).toHaveAttribute("aria-selected", "true");
    await page.getByRole("button", { name: "Keep this room", exact: true }).click();
    const roster = page.locator(".rh-keeping-people .rh-request-title");
    await expect(roster).toHaveCount(0);

    await visitor.getByRole("tab", { name: "live-den", exact: true }).click();
    await expect(roster).toHaveText(["room-visitor"]);
    await expect(visitor.getByRole("button", { name: "Keep this room", exact: true })).toHaveCount(0);
    await visitor.getByRole("button", { name: "Leave this room", exact: true }).click();
    await expect(roster).toHaveCount(0);
    await expect(page.getByRole("tab", { name: "live-den", exact: true })).toHaveAttribute("aria-selected", "true");

    await page.getByRole("button", { name: "Leave this room", exact: true }).click();
    await expect(visitor.getByRole("tab", { name: "live-den", exact: true })).toHaveCount(0);
    await expect(page.getByRole("tab", { name: "live-den", exact: true })).toHaveCount(0);

    await makeRoom(page, "private-den", true);
    await page.getByRole("button", { name: "Keep this room", exact: true }).click();
    await expect(visitor.getByRole("tab", { name: /private-den/ })).toHaveCount(0);
    await page.getByPlaceholder("Ask somebody in (a handle)", { exact: true }).fill("room-visitor");
    await page.getByRole("button", { name: "Invite", exact: true }).click();
    await expect(visitor.getByRole("tab", { name: /private-den/ })).toBeVisible();
    await expect(visitor.getByRole("tab", { name: "lobby", exact: true })).toHaveAttribute("aria-selected", "true");
    await visitor.getByRole("tab", { name: /private-den/ }).click();
    // The creator keeps this panel open while the invited client joins.
    await expect(roster).toHaveText(["room-visitor"]);
    await expect(visitor.locator(".rh-keeping-people")).toHaveCount(0);
    await visitor.getByRole("button", { name: "Leave this room", exact: true }).click();
    await expect(roster).toHaveCount(0);
    await page.getByRole("button", { name: "Leave this room", exact: true }).click();
    await expect(visitor.getByRole("tab", { name: /private-den/ })).toHaveCount(0);

    await makeRoom(visitor, "vanishing-den");
    await expect(page.getByRole("tab", { name: "vanishing-den", exact: true })).toBeVisible();
    await visitorContext.close();
    visitorContext = undefined;
    await expect(page.getByRole("tab", { name: "vanishing-den", exact: true })).toHaveCount(0);
    await expect(page.getByRole("tab", { name: "lobby", exact: true })).toHaveAttribute("aria-selected", "true");
    expect(errors).toEqual([]);
  } finally {
    await visitorContext?.close();
    await server.dispose();
    await test.info().attach("live-rooms-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
