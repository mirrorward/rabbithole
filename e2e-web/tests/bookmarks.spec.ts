import { test, expect, type Page } from "@playwright/test";
import { join } from "node:path";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for bookmark tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);
test.beforeEach(async ({ context }) => {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), (route) => route.abort());
});

const alice = "theme-viewer";
const alicePassword = "theme-e2e-password";
const bob = "bookmark-second";
const bobPassword = "bookmark-second-password";

// Return only metadata and booleans: no bearer or password is included in a
// diagnostic assertion, even though these credentials belong to test fixtures.
async function bookmarks(page: Page) {
  return page.evaluate(() => {
    const rows = JSON.parse(localStorage.getItem("rh.bookmarks.v1") ?? "[]");
    return rows.map((row: { id?: string; endpoint: string; name: string; login?: string; token?: string }) => ({
      id: row.id ?? "", endpoint: row.endpoint, name: row.name, login: row.login ?? null, saved: !!row.token,
    })) as { id: string; endpoint: string; name: string; login: string | null; saved: boolean }[];
  });
}

async function noPasswordsStored(page: Page) {
  expect(await page.evaluate((passwords) => {
    const values = Object.keys(localStorage).map((key) => localStorage.getItem(key) ?? "");
    return values.some((value) => passwords.some((password) => value.includes(password)));
  }, [alicePassword, bobPassword, "deliberately-wrong-password"])).toBe(false);
}

async function noRecentBearer(page: Page) {
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("rh.recent.burrows") ?? "[]")
    .some((row: { token?: string }) => !!row.token))).toBe(false);
}

async function fillSignIn(page: Page, server: TestBurrow, login: string, password: string, save: boolean) {
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(login);
  await page.locator("#rh-login-password").fill(password);
  await page.getByLabel("Save sign-in to bookmark").setChecked(save);
  await page.locator('.rh-login button[type="submit"]').click();
}

async function expectAccount(page: Page, login: string) {
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "You", exact: true }).click();
  // A server-confirmed PersonaList response proves which account is active;
  // a green transport dot or remembered input would not establish identity.
  await expect(page.getByRole("region", { name: "Your burrow profile", exact: true })
    .getByText(`@${login}`, { exact: true })).toBeVisible();
}

function accountRow(page: Page, login: string) {
  return page.locator(".rh-glass-item").filter({
    has: page.locator(".rh-glass-as").filter({ hasText: new RegExp(`^as ${login}$`) }),
  });
}

async function openSavedAccount(page: Page, login: string) {
  await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
  await expect(page).toHaveURL(/\/servers$/);
  const row = accountRow(page, login);
  await row.locator(".rh-glass-row").click();
  await row.getByRole("button", { name: "Connect…", exact: true }).click();
  await expect(page.locator("#rh-login-handle")).toHaveValue(login);
}

// Expire only the isolated fixture account's sessions. The next resume still
// travels over the real protocol and receives the real SessionExpired reply.
async function expireAccount(server: TestBurrow, login: string) {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(join(server.dataDir, "burrow.db"));
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    const result = db.prepare("UPDATE sessions SET expires_at = 0 WHERE account_id = (SELECT id FROM accounts WHERE login = ?)")
      .run(login);
    expect(result.changes).toBeGreaterThan(0);
  } finally { db.close(); }
}

test("account bookmarks migrate, opt in, resume and recover without crossing sign-ins", async ({ context, page }) => {
  const server = await TestBurrow.create("Bookmark Workshop", "704099");
  const errors: string[] = [];
  const auth = { password: 0, resume: 0 };
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", (socket) => {
    if (new URL(socket.url()).href !== new URL(server.wsURL).href) return;
    socket.on("framesent", ({ payload }) => {
      if (!Buffer.isBuffer(payload) || payload[0] !== 1 || payload[1] !== 0 || payload[2] !== 0) return;
      if (payload[3] === 10) auth.password += 1;
      if (payload[3] === 12) auth.resume += 1;
    });
  });
  try {
    await server.ctl("account-create", bob, bobPassword, "user");
    // Seed only the old public bookmark schema; authentication remains real.
    await context.addInitScript(({ origin, endpoint }) => {
      if (location.origin === origin && localStorage.getItem("rh.bookmarks.v1") === null) {
        localStorage.setItem("rh.bookmarks.v1", JSON.stringify([{ endpoint, name: "Legacy workshop" }]));
      }
    }, { origin: server.httpURL, endpoint: server.wsURL });
    await page.goto(server.httpURL);
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();
    const legacy = page.locator(".rh-glass-item").filter({ hasText: "Legacy workshop" });
    await legacy.locator(".rh-glass-row").click();
    await legacy.getByRole("button", { name: "Rename", exact: true }).click();
    await legacy.getByRole("textbox", { name: "Bookmark name", exact: true }).fill("Alice's workshop");
    await legacy.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(async () => (await bookmarks(page))[0]?.name).toBe("Alice's workshop");
    const legacyID = (await bookmarks(page))[0].id;
    expect(legacyID).not.toBe("");

    await fillSignIn(page, server, alice, alicePassword, false);
    await expectAccount(page, alice);
    expect(await bookmarks(page)).toHaveLength(1);
    expect((await bookmarks(page))[0]).toMatchObject({ id: legacyID, login: null, saved: false });
    await noRecentBearer(page);
    await page.reload();
    // You is a public local-identity route, so an opted-out reload may stay
    // there. Its profile area must be signed out and no resume should occur.
    await expect(page.getByRole("region", { name: "Your burrow profile", exact: true }))
      .toContainText("Connect to a burrow and sign in with an account to make a profile.");
    await expect(page.locator(".rh-header").getByRole("status")).toHaveText("Offline");
    expect(auth.resume).toBe(0);
    await page.goto(server.httpURL);
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();

    await fillSignIn(page, server, alice, alicePassword, true);
    await expectAccount(page, alice);
    await expect.poll(async () => (await bookmarks(page)).find((b) => b.login === alice)?.saved).toBe(true);
    expect((await bookmarks(page))[0]).toMatchObject({ id: legacyID, name: "Alice's workshop", login: alice });
    const passwordsAfterAlice = auth.password;
    await page.reload();
    await expectAccount(page, alice);
    expect(auth.password).toBe(passwordsAfterAlice);
    expect(auth.resume).toBeGreaterThan(0);

    // Leaving preserves the bookmark but stops launch-time resume. A direct
    // server URL must not inherit Alice's saved-sign-in consent on first mount.
    await page.goto(`${server.httpURL}/lobby`);
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeEnabled();
    await page.getByRole("button", { name: "Leave", exact: true }).click();
    await page.getByRole("alertdialog", { name: `Leave ${server.name}?`, exact: true })
      .getByRole("button", { name: "Leave", exact: true }).click();
    await expect(page.locator("#rh-login-handle")).toBeVisible();
    await noRecentBearer(page);
    const otherEndpoint = `${server.wsURL}/another-burrow`;
    await page.goto(`${server.httpURL}/?server=${encodeURIComponent(otherEndpoint)}`);
    await expect(page.locator("#rh-login-server")).toHaveValue(otherEndpoint);
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();
    await expect(page.locator("#rh-login-password")).toHaveValue("");

    // Adding an address also changes the connect target: discard any password
    // typed for Alice and require fresh saving consent for the new address.
    await accountRow(page, alice).locator(".rh-glass-row").click();
    await expect(page.getByLabel("Save sign-in to bookmark")).toBeChecked();
    await page.getByRole("button", { name: "Use password instead", exact: true }).click();
    await page.locator("#rh-login-password").fill(alicePassword);
    await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
    await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill("Other address");
    await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(otherEndpoint);
    await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
    await expect(page.locator("#rh-login-server")).toHaveValue(otherEndpoint);
    await expect(page.locator("#rh-login-password")).toHaveValue("");
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();
    const otherAddress = page.locator(".rh-glass-item").filter({ hasText: "Other address" });
    await otherAddress.locator(".rh-glass-row").click();
    await otherAddress.getByRole("button", { name: "Remove bookmark", exact: true }).click();
    await accountRow(page, alice).locator(".rh-glass-row").click();
    await expect(page.getByText("Saved sign-in for", { exact: false })).toBeVisible();
    await page.getByRole("button", { name: "Use another account", exact: true }).click();
    await expect(page.locator("#rh-login-handle")).toHaveValue("");
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();
    await fillSignIn(page, server, bob, bobPassword, true);
    await expectAccount(page, bob);
    await expect.poll(async () => (await bookmarks(page)).length).toBe(2);
    const bobID = (await bookmarks(page)).find((b) => b.login === bob)!.id;
    expect(bobID).not.toBe(legacyID);
    await page.reload();
    await expectAccount(page, bob);

    await openSavedAccount(page, alice);
    await expect(page.locator("#rh-login-password")).toHaveCount(0);
    await expect(page.locator('.rh-login button[type="submit"]')).toHaveText("Connect to Alice's workshop");
    for (const [name, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
      await page.setViewportSize({ width, height });
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.screenshot({ path: test.info().outputPath(`bookmarks-${name}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1280, height: 900 });
    const passwordsBeforeResume = auth.password;
    await page.locator('.rh-login button[type="submit"]').click();
    await expectAccount(page, alice);
    expect(auth.password).toBe(passwordsBeforeResume);

    await expireAccount(server, bob);
    await openSavedAccount(page, bob);
    await page.locator('.rh-login button[type="submit"]').click();
    await expect(page.locator(".rh-login-notice")).toContainText("expired");
    await expect(page.locator("#rh-login-handle")).toHaveValue(bob);
    await expect(page.locator("#rh-login-password")).toHaveValue("");
    expect((await bookmarks(page)).find((b) => b.login === bob)).toMatchObject({ id: bobID, saved: false });
    expect((await bookmarks(page)).find((b) => b.login === alice)?.saved).toBe(true);
    await fillSignIn(page, server, bob, bobPassword, true);
    await expectAccount(page, bob);
    expect((await bookmarks(page)).find((b) => b.login === bob)).toMatchObject({ id: bobID, saved: true });

    await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
    let row = accountRow(page, bob);
    await row.locator(".rh-glass-row").click();
    await row.getByRole("button", { name: "Rename", exact: true }).click();
    await row.getByRole("textbox", { name: "Bookmark name", exact: true }).fill("Side account");
    await row.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(async () => (await bookmarks(page)).find((b) => b.login === bob)?.name).toBe("Side account");
    row = accountRow(page, bob);
    await row.getByRole("button", { name: "Forget saved sign-in", exact: true }).click();
    await expect.poll(async () => (await bookmarks(page)).find((b) => b.login === bob)?.saved).toBe(false);
    expect((await bookmarks(page)).find((b) => b.login === alice)?.saved).toBe(true);
    await row.getByRole("button", { name: "Connect…", exact: true }).click();
    await expect(page.locator("#rh-login-handle")).toHaveValue(bob);
    await expect(page.locator("#rh-login-password")).toHaveValue("");
    await expect(page.getByLabel("Save sign-in to bookmark")).not.toBeChecked();
    await accountRow(page, bob).getByRole("button", { name: "Remove bookmark", exact: true }).click();
    await expect.poll(async () => (await bookmarks(page)).length).toBe(1);
    expect((await bookmarks(page))[0]).toMatchObject({ id: legacyID, name: "Alice's workshop", login: alice, saved: true });
    await noPasswordsStored(page);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("bookmark-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("failed authentication and refused bookmark storage do not claim a saved sign-in", async ({ page }) => {
  const server = await TestBurrow.create("Bookmark Refusal", "704099");
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(server.httpURL);
    await fillSignIn(page, server, alice, "deliberately-wrong-password", true);
    await expect(page.locator(".rh-login-notice")).toContainText(/didn.t accept/);
    await expect(page.locator("#rh-login-handle")).toHaveValue(alice);
    expect(await bookmarks(page)).toEqual([]);
    await noRecentBearer(page);

    // Deliberately inject a browser quota refusal for only the bookmark key.
    // Authentication and the resulting profile load use the real server.
    await page.evaluate(() => {
      const original = Storage.prototype.setItem;
      Storage.prototype.setItem = function (key, value) {
        if (key === "rh.bookmarks.v1") throw new DOMException("Fixture storage refusal", "QuotaExceededError");
        return original.call(this, key, value);
      };
    });
    await fillSignIn(page, server, alice, alicePassword, true);
    await expectAccount(page, alice);
    await expect(page.getByText("This browser could not save your bookmarks. Free some storage and try again.", { exact: true })).toBeVisible();
    expect(await bookmarks(page)).toEqual([]);
    await noRecentBearer(page);
    await noPasswordsStored(page);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("bookmark-refusal-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
