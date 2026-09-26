import { test, expect, type Page, type BrowserContext } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST,
  "Set BURROW_BIN and SPA_DIST for isolated inter-burrow sends");
test.skip(process.platform === "win32", "The server's ctl surface is Unix-only");
test.use({ colorScheme: "light", serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(120_000);

async function signIn(page: Page, server: TestBurrow) {
  await page.locator("#rh-login-server").fill(server.wsURL);
  await page.locator("#rh-login-handle").fill("theme-viewer");
  await page.locator("#rh-login-password").fill("theme-e2e-password");
  await page.locator('.rh-login button[type="submit"]').click();
  await expect(page.locator(".rh-header .rh-title-text")).toHaveText(server.name);
  await expect(page.locator(".rh-header .rh-dot.on")).toBeVisible();
}

async function servers(context: BrowserContext) {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname),
    (route) => route.abort());
  const source = await TestBurrow.create("Send Source", "a34700");
  let dest: TestBurrow | undefined;
  try {
    dest = await TestBurrow.create("Send Destination", "235b96");
    for (const server of [source, dest]) {
      for (const key of ["s2s_grants_enabled", "s2s_grants_to_any", "s2s_pull_enabled", "s2s_pull_from_any", "s2s_private_addresses", "s2s_swarm", "s2s_swarm_sources"]) {
        await server.ctl("config-set", key, "true");
      }
      await server.ctl("file-area-create", "delivery", "Delivery");
    }
    return { source, dest };
  } catch (error) {
    await source.dispose();
    if (dest) await dest.dispose();
    throw error;
  }
}

async function openSend(page: Page, source: TestBurrow, dest: TestBurrow, name: string) {
  await page.goto(source.httpURL); await signIn(page, source);
  await page.getByRole("button", { name: "Add a burrow", exact: true }).click();
  await page.getByRole("button", { name: "Add a bookmark by address", exact: true }).click();
  await page.getByRole("textbox", { name: "Name for the bookmark (optional)", exact: true }).fill(dest.name);
  await page.getByRole("textbox", { name: "Burrow address", exact: true }).fill(dest.wsURL);
  await page.getByRole("button", { name: "Add bookmark", exact: true }).click();
  await page.locator(".rh-glass-row").filter({ hasText: dest.name }).click();
  await page.getByRole("button", { name: "Connect…", exact: true }).click();
  await signIn(page, dest);
  await page.locator('.rh-rail-server[aria-label^="Send Source —"]').click();
  await page.getByRole("link", { name: "Files", exact: true }).click();
  await page.getByRole("button", { name: "Delivery", exact: true }).click();
  await page.locator('input[type="file"]').setInputFiles({ name, mimeType: "application/octet-stream", buffer: Buffer.alloc(512 * 1024, 54) });
  await page.locator(".rh-file-link").filter({ hasText: name }).click();
  await page.getByRole("button", { name: "Send to another burrow…", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("radio", { name: dest.name, exact: true }).click();
  await dialog.getByRole("button", { name: "Delivery", exact: true }).click();
  await expect(dialog.getByText("No folders here. It can go right here.")).toBeVisible();
  return dialog;
}

async function dispose(servers: TestBurrow[]) {
  for (const server of servers) {
    await server.dispose();
    await test.info().attach(`${server.name}-log`, { body: server.logs(), contentType: "text/plain" });
  }
}

test("a per-send origin choice reaches the destination and stays visible through progress", async ({ context, page }) => {
  const { source, dest } = await servers(context);
  const choices: number[] = []; const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", (socket) => {
    if (new URL(socket.url()).href !== new URL(dest.wsURL).href) return;
    socket.on("framesent", ({ payload }) => {
      if (Buffer.isBuffer(payload) && payload[0] === 1 && payload[1] === 0 && payload[2] === 5 && payload[3] === 47) choices.push(payload.at(-1)!);
    });
  });
  try {
    const dialog = await openSend(page, source, dest, "origin-choice.bin");
    const choice = dialog.getByLabel("Sources for this send", { exact: true });
    await expect(choice).toHaveValue("1");
    await expect(dialog).toContainText("when both burrow operators allow it");
    await choice.selectOption("0");
    await expect(dialog).toContainText("without asking for or contacting its swarm peers");
    await page.screenshot({ path: test.info().outputPath("send-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(choice).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Send to Delivery", exact: true })).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: test.info().outputPath("send-mobile.png"), fullPage: true });
    await page.setViewportSize({ width: 1280, height: 720 });
    // Keep the real transfer active long enough to observe its chosen policy.
    await source.ctl("config-set", "transfer_rate_bytes_per_sec", "65536");
    await dialog.getByRole("button", { name: "Send to Delivery", exact: true }).click();
    await expect(dialog).toBeHidden();
    await page.getByRole("button", { name: "Transfers", exact: true }).click();
    const row = page.locator(".rh-xfer-item").filter({ hasText: "origin-choice.bin" }).filter({ hasText: "Send Destination" });
    await expect(row).toContainText("Origin only");
    await expect(row.locator(".rh-badge")).toHaveText(/Queued|Active/);
    await expect(row.locator(".rh-badge")).toHaveText("Done", { timeout: 30_000 });
    await expect(row).toContainText("Origin only");
    expect(choices).toEqual([0]);
    await source.ctl("config-set", "transfer_rate_bytes_per_sec", "0");
    await page.locator('.rh-rail-server[aria-label^="Send Destination —"]').click();
    await page.getByRole("link", { name: "Files", exact: true }).click();
    await page.getByRole("button", { name: "Delivery", exact: true }).click();
    await expect(page.locator(".rh-file-link").filter({ hasText: "origin-choice.bin" })).toBeVisible();
    await expect(page.locator(".rh-queue-item").filter({ hasText: "origin-choice.bin" })).toContainText("Origin only");
    // A fresh send starts from its own default; this is not a server setting.
    await page.locator('.rh-rail-server[aria-label^="Send Source —"]').click();
    await page.getByRole("link", { name: "Files", exact: true }).click();
    await page.locator(".rh-file-link").filter({ hasText: "origin-choice.bin" }).click();
    await page.getByRole("button", { name: "Send to another burrow…", exact: true }).click();
    await expect(page.getByLabel("Sources for this send", { exact: true })).toHaveValue("1");
    expect(errors).toEqual([]);
  } finally { await dispose([source, dest]); }
});

test("an older destination never broadens origin-only and safely retries the swarm choice", async ({ context, page }) => {
  const { source, dest } = await servers(context);
  const choices: number[] = []; let legacy = 0;
  try {
    // Model only the additive message being absent. Grants, authentication,
    // destination acceptance, QUIC transfer and status pushes remain real.
    await context.routeWebSocket(dest.wsURL, (route) => {
      const upstream = route.connectToServer();
      route.onMessage((message) => {
        if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 0 && message[2] === 5 && message[3] === 47) {
          choices.push(message.at(-1)!);
          let end = 4; while (message[end++] & 0x80) { /* request id */ }
          const prefix = Buffer.from(message.subarray(0, end)); prefix[1] = 1;
          route.send(Buffer.concat([prefix, Buffer.from([1, 8, 0])]));
        } else {
          if (Buffer.isBuffer(message) && message[0] === 1 && message[1] === 0 && message[2] === 5 && message[3] === 35) legacy++;
          upstream.send(message);
        }
      });
      upstream.onMessage((message) => route.send(message));
    });
    const dialog = await openSend(page, source, dest, "older-destination.bin");
    await dialog.getByLabel("Sources for this send", { exact: true }).selectOption("0");
    await dialog.getByRole("button", { name: "Send to Delivery", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("Nothing was sent");
    expect(choices).toEqual([0]); expect(legacy).toBe(0);
    await expect(dialog.getByLabel("Sources for this send", { exact: true })).toHaveValue("0");
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(dialog.getByRole("button", { name: "Send to Delivery", exact: true })).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: test.info().outputPath("send-mobile-unsupported.png"), fullPage: true });
    await dialog.getByLabel("Sources for this send", { exact: true }).selectOption("1");
    await dialog.getByRole("button", { name: "Send to Delivery", exact: true }).click();
    await expect(dialog).toBeHidden();
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.getByRole("button", { name: "Transfers", exact: true }).click();
    const row = page.locator(".rh-xfer-item").filter({ hasText: "older-destination.bin" }).filter({ hasText: "Send Destination" });
    await expect(row).toContainText("Swarm when available");
    await expect(row.locator(".rh-badge")).toHaveText("Done", { timeout: 30_000 });
    expect(choices).toEqual([0, 1]); expect(legacy).toBe(1);
    await expect(page.locator(".rh-toasts")).not.toContainText("Unsupported");
  } finally { await dispose([source, dest]); }
});
