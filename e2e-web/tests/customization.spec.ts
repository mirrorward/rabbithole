import { test, expect, type Page, type Locator } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for customization tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(90_000);
test.beforeEach(async ({ context }) => {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), (route) => route.abort());
});

async function signIn(page: Page, server: TestBurrow) {
  await page.goto(server.httpURL);
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill("theme-viewer");
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
}
async function settings(page: Page) {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.getByLabel("Chime voice", { exact: true })).toBeVisible();
}
async function range(input: Locator, value: string) {
  await input.evaluate((element: HTMLInputElement, value) => {
    element.value = value;
    element.dispatchEvent(new Event("input", { bubbles: true }));
  }, value);
}
const sounds = (page: Page) => page.getByRole("checkbox", { name: "Play a chime for new messages while I'm away", exact: true });
const preview = (page: Page) => page.getByRole("button", { name: "Preview chime", exact: true });
async function captures(page: Page) {
  await page.locator('.rh-toasts button').evaluateAll((buttons) => buttons.forEach((button) => (button as HTMLButtonElement).click()));
  for (const [size, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
    await page.setViewportSize({ width, height });
    for (const [section, target] of [["appearance", page.locator("#rh-appearance-title")], ["sound", preview(page)]] as const) {
      await target.scrollIntoViewIfNeeded();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const png = await page.screenshot({ path: test.info().outputPath(`${section}-${size}.png`) });
      await test.info().attach(`${section}-${size}`, { body: png, contentType: "image/png" });
    }
  }
  await page.setViewportSize({ width: 1280, height: 900 });
}

test("appearance and sound choices persist and affect the rendered app", async ({ page }) => {
  const server = await TestBurrow.create("Personal Settings", "a34700");
  try {
    await server.ctl("account-create", "nested-editor", "editor-test-password", "user");
    await page.emulateMedia({ colorScheme: "light" });
    await signIn(page, server);
    await page.locator('.rh-subnav a[href="/directory"]').click();
    const list = page.getByRole("list", { name: "Members", exact: true });
    const row = list.locator(".rh-member-link").filter({ hasText: "@nested-editor" });
    await expect(row).toBeVisible();
    // Current member rows have no editor. Insert explicitly synthetic markup
    // inside a real row to exercise the shipped list key handler, not a replica.
    await row.evaluate((row) => {
      const input = document.createElement("input");
      input.setAttribute("aria-label", "Nested test editor"); input.value = "editable text";
      row.append(input);
      (window as any).__editorKeys = [];
      document.addEventListener("keydown", (event) => {
        if (event.target === input) (window as any).__editorKeys.push({ key: event.key, prevented: event.defaultPrevented });
      });
    });
    const editor = page.getByRole("textbox", { name: "Nested test editor", exact: true });
    await editor.focus();
    await page.keyboard.press("ArrowLeft");
    expect(await editor.evaluate((input: HTMLInputElement) => input.selectionStart)).toBe("editable text".length - 1);
    // Home/End have different native caret semantics on macOS and Linux;
    // neither may be prevented or move focus into list navigation.
    for (const key of ["Home", "End", "ArrowUp", "ArrowDown"]) {
      await page.keyboard.press(key);
      await expect(editor).toBeFocused();
    }
    expect(await page.evaluate(() => (window as any).__editorKeys)).toEqual(
      ["ArrowLeft", "Home", "End", "ArrowUp", "ArrowDown"].map((key) => ({ key, prevented: false })),
    );
    await editor.evaluate((input) => input.remove());
    await list.focus(); await page.keyboard.press("ArrowDown");
    await expect(list.locator(".rh-member-link").first()).toBeFocused();
    await settings(page);
    await page.getByRole("group", { name: "Light & dark", exact: true }).getByRole("button", { name: "System", exact: true }).click();
    await page.getByRole("checkbox", { name: /Use each burrow/ }).uncheck();
    await page.getByRole("group", { name: "Accent color", exact: true }).getByRole("button", { name: "Forest", exact: true }).click();
    await page.getByLabel("Chat font", { exact: true }).selectOption("mono");
    await range(page.getByLabel("Chat text size", { exact: true }), "20");
    await page.getByLabel("Spacing", { exact: true }).selectOption("compact");
    await page.getByLabel("Message times", { exact: true }).selectOption("always");
    await page.getByRole("checkbox", { name: "Show profile icons in chat", exact: true }).uncheck();
    await page.getByRole("checkbox", { name: /Reduce motion/ }).check();
    await page.getByLabel("Chime voice", { exact: true }).selectOption("classic");
    await range(page.getByLabel("Chime volume", { exact: true }), "37");
    await sounds(page).uncheck();
    await page.reload(); await settings(page);
    await expect(page.getByLabel("Chat font", { exact: true })).toHaveValue("mono");
    await expect(page.getByLabel("Chat text size", { exact: true })).toHaveValue("20");
    await expect(page.getByLabel("Spacing", { exact: true })).toHaveValue("compact");
    await expect(page.getByLabel("Message times", { exact: true })).toHaveValue("always");
    await expect(page.getByRole("checkbox", { name: /Use each burrow/ })).not.toBeChecked();
    await expect(page.getByLabel("Chime voice", { exact: true })).toHaveValue("classic");
    await expect(page.getByLabel("Chime volume", { exact: true })).toHaveValue("37");
    await expect(sounds(page)).not.toBeChecked();
    const app = page.locator(".rh-app");
    for (const name of ["rh-compact", "rh-times-always", "rh-no-chat-icons", "rh-reduce-motion"]) await expect(app).toHaveClass(new RegExp(name));
    await expect(app).toHaveCSS("--rh-chat-size", "20px");
    await expect(app).toHaveCSS("--rh-chat-font", /monospace/);
    await expect(app).toHaveCSS("--rh-accent", "#256344");
    await expect(app).toHaveCSS("color-scheme", "light");
    await captures(page);
    await page.emulateMedia({ colorScheme: "dark" });
    await expect(app).toHaveCSS("color-scheme", "dark");
    await expect(app).toHaveCSS("--rh-accent", "#8dd8ae");
    await page.getByRole("group", { name: "Theme", exact: true }).getByRole("button", { name: "High contrast", exact: true }).click();
    await expect(page.getByRole("button", { name: "Forest", exact: true })).toBeDisabled();
    await expect(app).not.toHaveCSS("--rh-accent", "#8dd8ae");
    await expect(app).toHaveCSS("--rh-chat-size", "20px");
  } finally {
    await server.dispose();
    await test.info().attach("customization-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});

test("chime preview uses native audio and recovers from injected resume failures after navigation", async ({ context, page }) => {
  // Real AudioContext/oscillator/gain nodes remain in use. Only resume refusal
  // and pending state are injected, making browser-policy/lifecycle faults
  // deterministic without claiming to reproduce native autoplay policy.
  await context.addInitScript(() => {
    const Native = window.AudioContext;
    const probe = { mode: "native", contexts: 0, starts: [] as { frequency: number; type: string }[], peaks: [] as number[], disconnected: 0, reject: undefined as undefined | (() => void) };
    (window as any).__chimes = probe;
    window.AudioContext = class extends Native {
      constructor(options?: AudioContextOptions) { super(options); probe.contexts++; }
      get state(): AudioContextState { return probe.mode === "native" ? super.state : "suspended"; }
      resume() {
        if (probe.mode === "refuse") return Promise.reject(new DOMException("Injected resume refusal", "NotAllowedError"));
        if (probe.mode === "pending") return new Promise<void>((_, reject) => { probe.reject = () => reject(new DOMException("Injected late refusal", "NotAllowedError")); });
        return super.resume();
      }
      createOscillator() {
        const node = super.createOscillator(), start = node.start.bind(node), disconnect = node.disconnect.bind(node);
        node.start = (when) => { probe.starts.push({ frequency: node.frequency.value, type: node.type }); start(when); };
        node.disconnect = () => { probe.disconnected++; disconnect(); };
        return node;
      }
      createGain() {
        const node = super.createGain(), ramp = node.gain.linearRampToValueAtTime.bind(node.gain);
        node.gain.linearRampToValueAtTime = (value, when) => { probe.peaks.push(value); return ramp(value, when); };
        return node;
      }
    };
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.text().includes("already been disposed")) errors.push(message.text()); });
  const server = await TestBurrow.create("Chime Preview", "a34700");
  try {
    await signIn(page, server); await settings(page);
    await sounds(page).uncheck();
    await page.getByLabel("Chime voice", { exact: true }).selectOption("classic");
    await range(page.getByLabel("Chime volume", { exact: true }), "40");
    await page.evaluate(() => { (window as any).__chimes.mode = "refuse"; });
    await preview(page).click();
    await expect(page.locator('.rh-settings-note[role="status"]')).toContainText("Sound could not start");
    expect(await page.evaluate(() => (window as any).__chimes.starts.length)).toBe(0);
    await page.evaluate(() => { (window as any).__chimes.mode = "native"; });
    await preview(page).click();
    await expect.poll(() => page.evaluate(() => (window as any).__chimes.starts.length)).toBe(2);
    const first = await page.evaluate(() => ({ starts: (window as any).__chimes.starts, peaks: (window as any).__chimes.peaks }));
    expect(first.starts).toEqual([{ frequency: 784, type: "triangle" }, { frequency: 1046.5, type: "triangle" }]);
    expect(first.peaks).toHaveLength(2);
    for (const peak of first.peaks) expect(peak).toBeCloseTo(0.024, 5);
    await page.getByLabel("Chime voice", { exact: true }).selectOption("subtle");
    await range(page.getByLabel("Chime volume", { exact: true }), "60");
    await preview(page).click();
    await expect.poll(() => page.evaluate(() => (window as any).__chimes.starts.length)).toBe(4);
    const second = await page.evaluate(() => ({ contexts: (window as any).__chimes.contexts, starts: (window as any).__chimes.starts.slice(2), peaks: (window as any).__chimes.peaks.slice(2) }));
    expect(second.contexts).toBe(1);
    expect(second.starts[0]).toEqual({ frequency: 440, type: "sine" });
    expect(second.peaks).toHaveLength(2);
    for (const peak of second.peaks) expect(peak).toBeCloseTo(0.021, 5);
    await expect.poll(() => page.evaluate(() => (window as any).__chimes.disconnected)).toBe(4);
    await range(page.getByLabel("Chime volume", { exact: true }), "0");
    await expect(preview(page)).toBeDisabled();
    await range(page.getByLabel("Chime volume", { exact: true }), "60");
    await page.evaluate(() => { (window as any).__chimes.mode = "pending"; });
    await preview(page).click();
    await expect(page.getByRole("button", { name: "Starting…", exact: true })).toBeDisabled();
    await page.goBack();
    await expect(page.locator("#rh-sound-preset")).toHaveCount(0);
    await page.evaluate(async () => {
      (window as any).__chimes.reject();
      await new Promise(requestAnimationFrame);
    });
    await settings(page);
    await expect(page.locator('.rh-settings-note[role="status"]')).toHaveCount(0);
    await page.evaluate(() => { (window as any).__chimes.mode = "native"; });
    await preview(page).click();
    await expect.poll(() => page.evaluate(() => (window as any).__chimes.starts.length)).toBe(6);
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("chime-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
