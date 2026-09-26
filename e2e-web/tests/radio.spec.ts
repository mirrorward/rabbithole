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
async function signIn(page: Page, server: TestBurrow, handle = "theme-viewer") {
  await page.goto(server.httpURL);
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill(handle);
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

const ducking = (page: Page) => page.getByRole("checkbox", { name: "Lower radio volume during message chimes", exact: true });
const previewChime = (page: Page) => page.getByRole("button", { name: "Preview chime", exact: true });
const automaticChimes = (page: Page) => page.getByRole("checkbox", { name: "Play a chime for new messages while I'm away", exact: true });
async function soundSettings(page: Page) {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.clock.runFor(32);
  await expect(ducking(page)).toBeVisible();
}
async function setRange(page: Page, name: string, value: string) {
  await page.getByRole("slider", { name, exact: true }).evaluate((input: HTMLInputElement, value) => {
    input.value = value; input.dispatchEvent(new Event("input", { bubbles: true }));
  }, value);
}
async function audioVolume(page: Page) {
  return page.evaluate(() => (window as any).__radioProbe.elements.at(-1).volume as number);
}
async function chimeStarts(page: Page) {
  return page.evaluate(() => (window as any).__duckProbe.starts as number);
}
// Native media decoding and Web Audio nodes remain in use. Only the JS clock,
// focus policy input, and explicit resume refusal are controlled. This tests
// the shipped envelope against a real stream, without claiming OS audibility
// or a particular browser's native autoplay policy.
async function observeDucking(context: BrowserContext) {
  await observeAudio(context);
  await context.addInitScript(() => {
    const NativeContext = window.AudioContext;
    const probe = { starts: 0, mode: "native", focused: true, volumes: [] as number[] };
    (window as any).__duckProbe = probe;
    document.hasFocus = () => probe.focused;
    const volume = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "volume")!;
    Object.defineProperty(HTMLMediaElement.prototype, "volume", {
      ...volume,
      set(value: number) { probe.volumes.push(value); volume.set!.call(this, value); },
    });
    window.AudioContext = class extends NativeContext {
      get state(): AudioContextState { return probe.mode === "refuse" ? "suspended" : super.state; }
      resume() {
        return probe.mode === "refuse"
          ? Promise.reject(new DOMException("Injected chime resume refusal", "NotAllowedError"))
          : super.resume();
      }
      createOscillator() {
        const node = super.createOscillator(), start = node.start.bind(node);
        node.start = (when) => { probe.starts++; start(when); };
        return node;
      }
    };
  });
}
async function freezeEnvelopeClock(page: Page) {
  // Install before app startup so performance.now() never moves backward when
  // switching clocks. Pausing changes JS timers, not native media/audio time.
  await page.clock.pauseAt(new Date(await page.evaluate(() => Date.now() + 1000)));
}
async function burrowRoute(page: Page, server: TestBurrow, route: "radio" | "lobby") {
  if (await page.locator(`.rh-subnav a[href="/${route}"]`).count() === 0) {
    await page.getByRole("navigation", { name: "Burrows", exact: true }).getByRole("button", { name: new RegExp(`^${server.name} —`) }).click();
    await page.clock.runFor(32);
  }
  await page.locator(`.rh-subnav a[href="/${route}"]`).click();
  await page.clock.runFor(32);
}
async function expectBaseVolume(page: Page, volume = 0.8) {
  await page.clock.runFor(600);
  expect(await audioVolume(page)).toBeCloseTo(volume, 5);
}
async function duckingPictures(page: Page) {
  await page.locator('.rh-toasts button').evaluateAll((buttons) => buttons.forEach((button) => (button as HTMLButtonElement).click()));
  for (const [size, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
    await page.setViewportSize({ width, height });
    await ducking(page).scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await expect(ducking(page)).toBeVisible();
    const png = await page.screenshot({ path: test.info().outputPath(`radio-ducking-settings-${size}.png`) });
    await test.info().attach(`radio-ducking-settings-${size}`, { body: png, contentType: "image/png" });
  }
  await page.setViewportSize({ width: 1280, height: 900 });
}

test("radio ducking is opt-in, persists, and envelopes previews without restarting the stream", async ({ context, page }) => {
  await localOnly(context); await observeDucking(context);
  await page.clock.install();
  // Upgrade an actual old preference record: it must retain the old choices
  // while the newly introduced option starts off. Do not reseed on reload.
  await context.addInitScript(() => {
    if (!localStorage.getItem("rh-radio")) localStorage.setItem("rh-radio", JSON.stringify({ enabled: false, volume: 0.8, muted: false, station: "first" }));
  });
  const server = await TestBurrow.create("Radio Ducking", "a34700", { radio: true });
  let fixture: RadioFixture | undefined;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    fixture = await RadioFixture.create(server);
    await signIn(page, server); await soundSettings(page);
    await expect(ducking(page)).not.toBeChecked();
    await ducking(page).check();
    await automaticChimes(page).uncheck();
    await expect.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("rh-radio")!).ducking)).toBe(true);
    await page.reload(); await soundSettings(page);
    await expect(ducking(page)).toBeChecked();
    await expect(automaticChimes(page)).not.toBeChecked();
    await duckingPictures(page);
    await burrowRoute(page, server, "radio");
    await player(page).getByRole("button", { name: "Listen", exact: true }).click();
    await expect(status(page)).toHaveText("Listening");
    await expect.poll(() => page.evaluate(() => (window as any).__radioProbe.elements.at(-1).currentTime)).toBeGreaterThan(0);
    const playbackCalls = await calls(page);
    await soundSettings(page); await freezeEnvelopeClock(page);
    await page.evaluate(() => { (window as any).__duckProbe.volumes = []; });
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(2);
    await page.clock.runFor(64);
    expect(await audioVolume(page)).toBeCloseTo(0.2, 5);
    expect(await page.evaluate(() => (window as any).__duckProbe.volumes.some((v: number) => v > 0.2 && v < 0.8))).toBe(true);
    // A second audible chime extends the quiet interval, including beyond the
    // first one's release deadline; it never adds another audio element.
    await page.clock.runFor(100);
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(4);
    await page.clock.runFor(100);
    expect(await audioVolume(page)).toBeCloseTo(0.2, 5);
    // A system clock correction must not stall gain recovery: the envelope
    // follows elapsed performance time instead of Date's wall clock.
    await page.clock.setSystemTime(new Date(await page.evaluate(() => Date.now() - 3_600_000)));
    await expectBaseVolume(page);
    expect(await calls(page)).toBe(playbackCalls);

    // User edits while ducked are authoritative. They must not restart the
    // stream or be overwritten by the eventual recovery callback.
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(6);
    await page.clock.runFor(64);
    await burrowRoute(page, server, "radio");
    await setRange(page, "Volume", "40");
    expect(await audioVolume(page)).toBeCloseTo(0.1, 5);
    await player(page).getByRole("button", { name: "Mute", exact: true }).click();
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.at(-1).muted)).toBe(true);
    await expectBaseVolume(page, 0.4);
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.at(-1).muted)).toBe(true);
    await player(page).getByRole("button", { name: "Unmute", exact: true }).click();
    expect(await audioVolume(page)).toBeCloseTo(0.4, 5);

    await soundSettings(page);
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(8);
    await page.clock.runFor(64);
    expect(await audioVolume(page)).toBeCloseTo(0.1, 5);
    await ducking(page).uncheck();
    await expectBaseVolume(page, 0.4);
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(10);
    await page.clock.runFor(64);
    expect(await audioVolume(page)).toBeCloseTo(0.4, 5);
    expect(await calls(page)).toBe(playbackCalls);
    expect(await page.evaluate(() => new Set((window as any).__radioProbe.elements).size)).toBe(1);
    expect(await page.evaluate(() => JSON.parse(localStorage.getItem("rh-radio")!))).toMatchObject({ ducking: false, volume: 0.4, muted: false });
    await burrowRoute(page, server, "radio");
    await player(page).getByRole("button", { name: "Stop", exact: true }).click();
    await page.reload(); await soundSettings(page);
    await expect(ducking(page)).not.toBeChecked();
    expect(errors).toEqual([]);
  } finally {
    await fixture?.dispose(); await server.dispose();
    await test.info().attach("radio-ducking-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("real incoming messages duck only when their chime is audible and playback is active", async ({ browser, context, page }) => {
  await localOnly(context); await observeDucking(context);
  await page.clock.install();
  const server = await TestBurrow.create("Message Ducking", "a34700", { radio: true });
  const senderContext = await browser.newContext({ serviceWorkers: "block" });
  await localOnly(senderContext);
  let fixture: RadioFixture | undefined;
  try {
    fixture = await RadioFixture.create(server);
    await server.ctl("account-create", "chime-sender", "theme-e2e-password", "user");
    const sender = await senderContext.newPage();
    await signIn(sender, server, "chime-sender");
    await signIn(page, server); await soundSettings(page);
    await ducking(page).check();
    // Prime real Web Audio with a user gesture before messages arrive.
    await previewChime(page).click();
    await expect.poll(() => chimeStarts(page)).toBe(2);
    await burrowRoute(page, server, "radio");
    await player(page).getByRole("button", { name: "Listen", exact: true }).click();
    await expect(status(page)).toHaveText("Listening");
    const playbackCalls = await calls(page);
    await freezeEnvelopeClock(page);
    const send = async (text: string) => {
      const input = sender.getByRole("textbox", { name: "Message #lobby", exact: true });
      await input.fill(text); await input.press("Enter");
      await expect(page.getByRole("log", { name: "Chat messages" }).getByText(text, { exact: true })).toBeVisible();
    };
    await burrowRoute(page, server, "lobby");
    await page.evaluate(() => { (window as any).__duckProbe.focused = false; });
    await send("An audible incoming room message");
    await expect.poll(() => chimeStarts(page)).toBe(3);
    await page.clock.runFor(64);
    expect(await audioVolume(page)).toBeCloseTo(0.2, 5);
    await expectBaseVolume(page);

    await page.evaluate(() => { (window as any).__duckProbe.mode = "refuse"; (window as any).__duckProbe.volumes = []; });
    await send("A message whose chime cannot resume");
    await page.clock.runFor(600);
    expect(await chimeStarts(page)).toBe(3);
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);
    expect(await page.evaluate(() => (window as any).__duckProbe.volumes.every((v: number) => Math.abs(v - 0.8) < 0.00001))).toBe(true);
    await page.evaluate(() => { (window as any).__duckProbe.mode = "native"; });

    await soundSettings(page); await setRange(page, "Chime volume", "0");
    await expect(previewChime(page)).toBeDisabled();
    await burrowRoute(page, server, "lobby");
    await send("Zero chime volume is silent");
    await page.clock.runFor(64);
    expect(await chimeStarts(page)).toBe(3);
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);

    await soundSettings(page); await setRange(page, "Chime volume", "60");
    await automaticChimes(page).uncheck();
    await burrowRoute(page, server, "lobby");
    await send("Disabled message sounds are silent");
    await page.clock.runFor(64);
    expect(await chimeStarts(page)).toBe(3);
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);
    await soundSettings(page); await automaticChimes(page).check();
    await burrowRoute(page, server, "lobby");
    await page.evaluate(() => { (window as any).__duckProbe.focused = true; });
    await send("Focused messages are silent");
    await page.clock.runFor(64);
    expect(await chimeStarts(page)).toBe(3);
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);

    // A muted radio remains muted when a real chime plays; unmuting later
    // must reveal the base volume rather than a stale attenuation.
    await burrowRoute(page, server, "radio");
    await player(page).getByRole("button", { name: "Mute", exact: true }).click();
    await burrowRoute(page, server, "lobby");
    await page.evaluate(() => { (window as any).__duckProbe.focused = false; });
    await send("A message while the radio is muted");
    await expect.poll(() => chimeStarts(page)).toBe(4);
    await page.clock.runFor(64);
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.at(-1).muted)).toBe(true);
    await expectBaseVolume(page);
    await burrowRoute(page, server, "radio");
    await player(page).getByRole("button", { name: "Unmute", exact: true }).click();
    expect(await audioVolume(page)).toBeCloseTo(0.8, 5);
    expect(await calls(page)).toBe(playbackCalls);
    await player(page).getByRole("button", { name: "Stop", exact: true }).click();
    await burrowRoute(page, server, "lobby");
    await send("A message while the radio is stopped");
    await expect.poll(() => chimeStarts(page)).toBe(5);
    await page.clock.runFor(600);
    expect(await calls(page)).toBe(playbackCalls);
    expect(await page.evaluate(() => (window as any).__radioProbe.elements.every((audio: HTMLMediaElement) => audio.paused && !audio.hasAttribute("src")))).toBe(true);
  } finally {
    await senderContext.close(); await fixture?.dispose(); await server.dispose();
    await test.info().attach("message-ducking-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
