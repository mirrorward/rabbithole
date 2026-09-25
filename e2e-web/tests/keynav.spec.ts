import { test, expect, type Locator } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST to run the rendered keyboard navigation regression");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(90_000);

// Change the real component's filter without moving DOM focus. This models a
// list update while its user is navigating rows; Leptos performs every removal.
async function filterWithoutFocus(input: Locator, value: string) {
  await input.evaluate((element: HTMLInputElement, text) => {
    element.value = text;
    element.dispatchEvent(new Event("input", { bubbles: true }));
  }, value);
}

test("dynamic lists recover removed rows without stealing focus", async ({ context, page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await context.route("**/*", (route) => {
    const host = new URL(route.request().url()).hostname;
    return ["127.0.0.1", "localhost", "[::1]"].includes(host) ? route.continue() : route.abort();
  });
  const server = await TestBurrow.create("Keyboard Focus", "a34700");
  try {
    for (const name of ["nav-a-keep", "nav-m-drop", "nav-z-keep"]) {
      await server.ctl("account-create", name, "keynav-e2e-password", "user");
    }
    await page.goto(server.httpURL);
    await page.locator("#rh-login-server").fill(server.wsURL);
    await page.locator("#rh-login-handle").fill("theme-viewer");
    await page.locator("#rh-login-password").fill("theme-e2e-password");
    await page.locator('.rh-login button[type="submit"]').click();
    await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
    await page.locator('.rh-subnav a[href="/directory"]').click();

    const search = page.getByRole("searchbox", { name: "Search members" });
    const list = page.getByRole("list", { name: "Members", exact: true });
    const rows = list.locator(".rh-member-link");
    const first = rows.filter({ hasText: "@nav-a-keep" });
    const middle = rows.filter({ hasText: "@nav-m-drop" });
    const last = rows.filter({ hasText: "@nav-z-keep" });
    await search.fill("nav-");
    await expect(rows).toHaveCount(3);

    // Normal keyboard behavior remains native: enter, clamp, and Home/End.
    await list.focus();
    await page.keyboard.press("ArrowDown");
    await expect(first).toBeFocused();
    await page.keyboard.press("ArrowUp");
    await expect(first).toBeFocused();
    await page.keyboard.press("End");
    await expect(last).toBeFocused();
    await page.keyboard.press("Home");
    await page.keyboard.press("ArrowDown");
    await expect(middle).toBeFocused();

    // The middle disappears: prefer its next neighbor. No manual focus call
    // follows the update, so this observes recovery by the shipped WASM code.
    await filterWithoutFocus(search, "keep");
    await expect(rows).toHaveCount(2);
    await expect(last).toBeFocused();
    // The last disappears: recover to the preceding surviving row.
    await filterWithoutFocus(search, "a-keep");
    await expect(rows).toHaveCount(1);
    await expect(first).toBeFocused();
    // Empty lists remain a single reachable Tab stop.
    await filterWithoutFocus(search, "no-member-matches");
    await expect(rows).toHaveCount(0);
    await expect(list).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(list).toBeFocused();
    await filterWithoutFocus(search, "nav-");
    await expect(rows).toHaveCount(3);
    await expect(list).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(first).toBeFocused();

    // Removing some other row must not move the focused row, even when its
    // index changes. Track the surviving node's current position afterward.
    await last.focus();
    await filterWithoutFocus(search, "keep");
    await expect(rows).toHaveCount(2);
    await expect(last).toBeFocused();
    await filterWithoutFocus(search, "a-keep");
    await expect(first).toBeFocused();

    // Filtering normally means focus belongs to the search field. Do not
    // pull it back into a list just because its former row then disappears.
    await search.fill("nav-");
    await expect(rows).toHaveCount(3);
    await middle.focus();
    await search.fill("keep");
    await expect(rows).toHaveCount(2);
    await expect(search).toBeFocused();

    // Focus explicitly handed to another control in the same update wins
    // over the pending MutationObserver callback.
    await filterWithoutFocus(search, "nav-");
    await middle.focus();
    await search.evaluate((input: HTMLInputElement) => {
      input.value = "keep";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.focus();
    });
    await expect(rows).toHaveCount(2);
    await expect(search).toBeFocused();

    // A deliberate blur while the row is still present is not a removal.
    await first.focus();
    await first.evaluate((row: HTMLElement) => row.blur());
    await expect(page.locator("body")).toBeFocused();
    await filterWithoutFocus(search, "z-keep");
    await expect(rows).toHaveCount(1);
    await expect(page.locator("body")).toBeFocused();

    // Route disposal must release the old observer and respect the new
    // view's focus. Returning mounts a fresh observer without stale rows.
    await last.focus();
    await page.locator('.rh-subnav a[href="/lobby"]').click();
    await expect(list).toHaveCount(0);
    const compose = page.getByRole("textbox", { name: "Message #lobby", exact: true });
    await compose.focus();
    await expect(compose).toBeFocused();
    await page.locator('.rh-subnav a[href="/directory"]').click();
    await search.fill("nav-");
    await expect(rows).toHaveCount(3);
    await middle.focus();
    await filterWithoutFocus(search, "keep");
    await expect(last).toBeFocused();
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("keynav-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
