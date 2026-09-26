import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";
import { RadioFixture } from "../fixtures/radio";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for radio playback tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(90_000);

async function localOnly(context: BrowserContext) {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), (route) => route.abort());
}
async function signIn(page: Page, server: TestBurrow) {
  await page.goto(server.httpURL);
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill("theme-viewer");
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.getByLabel("Save sign-in to bookmark").check();
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
  await expect.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("rh.recent.burrows") ?? "[]").some((burrow: { token?: string }) => !!burrow.token))).toBe(true);
}
// Record native promises/elements, with explicit fault injection for permission
// refusal and deferred results. Successful retries still use native playback.
async function observeAudio(context: BrowserContext) {
  await context.addInitScript(() => {
    const native = HTMLMediaElement.prototype.play;
    const probe = { calls: 0, outcomes: [] as string[], elements: [] as HTMLMediaElement[],
      blockNext: false, deferNext: false, pending: [] as { resolve: () => void; reject: (error: unknown) => void }[] };
    (window as any).__radioProbe = probe;
    HTMLMediaElement.prototype.play = function () {
      probe.calls++; probe.elements.push(this);
      if (probe.blockNext) {
        probe.blockNext = false;
        probe.outcomes.push("NotAllowedError");
        return Promise.reject(new DOMException("Injected playback permission refusal", "NotAllowedError"));
      }
      if (probe.deferNext) {
        probe.deferNext = false;
        return new Promise<void>((resolve, reject) => probe.pending.push({ resolve, reject }));
      }
      return native.call(this).then(() => { probe.outcomes.push("playing"); }, (error) => {
        probe.outcomes.push(error.name); throw error;
      });
    };
  });
}
const status = (page: Page) => page.locator("[data-radio-playback]");
const player = (page: Page) => page.getByRole("region", { name: "Radio player" });
async function calls(page: Page) { return page.evaluate(() => (window as any).__radioProbe.calls); }
async function pictures(page: Page, name: string) {
  await page.locator('.rh-toasts button').evaluateAll((buttons) => buttons.forEach((button) => (button as HTMLButtonElement).click()));
  for (const [size, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
    await page.setViewportSize({ width, height });
    await status(page).scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    for (const button of await player(page).locator('.rh-player-controls button').all()) await expect(button).toBeVisible();
    const png = await page.screenshot({ path: test.info().outputPath(`${name}-${size}.png`) });
    await test.info().attach(`${name}-${size}`, { body: png, contentType: "image/png" });
  }
  await page.setViewportSize({ width: 1280, height: 900 });
}

test("injected permission refusal offers a native gesture retry; real stream failures remain actionable", async ({ context, page }) => {
  await localOnly(context); await observeAudio(context);
  const server = await TestBurrow.create("Radio Playback", "a34700", { radio: true });
  let fixture: RadioFixture | undefined;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    fixture = await RadioFixture.create(server);
    await signIn(page, server);
    await page.locator('.rh-subnav a[href="/radio"]').click();
    await expect(page.getByRole("button", { name: /First station/ })).toBeVisible();
    // Inject only the browser permission refusal. Chromium automation may grant
    // media activation; the recovery below calls the real play() from a click.
    await page.evaluate(() => { (window as any).__radioProbe.blockNext = true; });
    await player(page).getByRole("button", { name: "Listen", exact: true }).click();
    await expect(status(page)).toContainText("browser paused automatic playback");
    expect(await page.evaluate(() => (window as any).__radioProbe.outcomes)).toContain("NotAllowedError");
    expect(await calls(page)).toBe(1);
    await fixture.metadata("first", "A metadata update while blocked");
    await expect(player(page).locator('.rh-player-title')).toHaveText("A metadata update while blocked");
    await page.getByRole("slider", { name: "Volume", exact: true }).evaluate((input: HTMLInputElement) => { input.value = "35"; input.dispatchEvent(new Event("input", { bubbles: true })); });
    await player(page).getByRole("button", { name: "Mute", exact: true }).click();
    await player(page).getByRole("button", { name: "Unmute", exact: true }).click();
    expect(await calls(page)).toBe(1);
    await expect(status(page)).toContainText("browser paused automatic playback");
    await pictures(page, "blocked");
    await player(page).getByRole("button", { name: "Start listening", exact: true }).click();
    await expect(status(page)).toHaveText("Listening");
    expect(await calls(page)).toBe(2);
    await expect.poll(() => page.evaluate(() => (window as any).__radioProbe.elements.at(-1).currentTime)).toBeGreaterThan(0);

    // A real unsuccessful media response after an explicit stop/start.
    await player(page).getByRole("button", { name: "Stop", exact: true }).click();
    fixture.broken.add("/first");
    await player(page).getByRole("button", { name: "Listen", exact: true }).click();
    await expect(status(page)).toContainText("station couldn’t play");
    await pictures(page, "failed");
    const failedCalls = await calls(page);
    await fixture.metadata("first", "A metadata update after failure");
    await expect(player(page).locator('.rh-player-title')).toHaveText("A metadata update after failure");
    expect(await calls(page)).toBe(failedCalls);
    fixture.broken.delete("/first");
    await player(page).getByRole("button", { name: "Retry playback", exact: true }).click();
    await expect(status(page)).toHaveText("Listening");
    await page.getByRole("button", { name: /Second station/ }).click();
    await expect(status(page)).toHaveText("Listening");
    await expect.poll(() => page.evaluate(() => (window as any).__radioProbe.elements.at(-1).src)).toContain("/second");
    await player(page).getByRole("button", { name: "Stop", exact: true }).click();
    await expect(status(page)).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.every((audio: HTMLMediaElement) => audio.paused && !audio.hasAttribute("src")))).toBe(true);
    expect(errors).toEqual([]);
  } finally {
    await fixture?.dispose(); await server.dispose();
    await test.info().attach("radio-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("injected late playback results and old media events cannot overwrite a newer attempt", async ({ context, page }) => {
  await localOnly(context); await observeAudio(context);
  const server = await TestBurrow.create("Radio Attempt Lifecycle", "a34700", { radio: true });
  let fixture: RadioFixture | undefined;
  try {
    fixture = await RadioFixture.create(server);
    await signIn(page, server);
    await page.locator('.rh-subnav a[href="/radio"]').click();
    await expect(page.getByRole("button", { name: /First station/ })).toBeVisible();
    await page.evaluate(() => { (window as any).__radioProbe.deferNext = true; });
    await player(page).getByRole("button", { name: "Listen", exact: true }).click();
    await expect(status(page)).toHaveText("Connecting to the station…");
    await page.getByRole("button", { name: /Second station/ }).click();
    await expect(status(page)).toHaveText("Listening");
    await page.evaluate(() => {
      const probe = (window as any).__radioProbe;
      probe.pending.shift().reject(new DOMException("Late old refusal", "NotAllowedError"));
      probe.elements[0].dispatchEvent(new Event("error"));
      probe.elements[0].dispatchEvent(new Event("ended"));
    });
    await expect(status(page)).toHaveText("Listening");
    expect(await page.evaluate(() => (window as any).__radioProbe.elements[0].paused && !(window as any).__radioProbe.elements[0].hasAttribute("src"))).toBe(true);
    // A current stream's media error after Playing must surface, even though
    // its play promise resolved earlier (fault injection is intentional).
    await page.evaluate(() => (window as any).__radioProbe.elements.at(-1).dispatchEvent(new Event("error")));
    await expect(status(page)).toContainText("station couldn’t play");
    await page.evaluate(() => { (window as any).__radioProbe.deferNext = true; });
    await player(page).getByRole("button", { name: "Retry playback", exact: true }).click();
    await expect(status(page)).toHaveText("Connecting to the station…");
    await player(page).getByRole("button", { name: "Cancel", exact: true }).click();
    await page.evaluate(() => {
      const probe = (window as any).__radioProbe;
      probe.pending.shift().resolve();
      probe.elements.at(-1).dispatchEvent(new Event("error"));
    });
    await expect(status(page)).toHaveCount(0);
    await expect(player(page).getByRole("button", { name: "Listen", exact: true })).toBeVisible();
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.every((audio: HTMLMediaElement) => audio.paused && !audio.hasAttribute("src")))).toBe(true);
  } finally {
    await fixture?.dispose(); await server.dispose();
    await test.info().attach("radio-lifecycle-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
