import { test, expect } from "@playwright/test";
import { TestBurrow } from "../fixtures/burrow";

test.skip(!process.env.BURROW_BIN || !process.env.SPA_DIST, "Set BURROW_BIN and SPA_DIST for profile tests");
test.skip(process.platform === "win32", "The server's ctl surface is currently Unix-only");
test.use({ serviceWorkers: "block", actionTimeout: 15_000 });
test.setTimeout(60_000);
test.beforeEach(async ({ context }) => {
  await context.route((url) => !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname), (route) => route.abort());
});

test("profile drafts survive navigation and public icons and cleared fields survive reload", async ({ page }) => {
  const server = await TestBurrow.create("Profile Workshop", "a34700");
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(server.httpURL);
    await page.locator("#rh-login-server").fill(server.wsURL);
    await page.locator("#rh-login-handle").fill("theme-viewer");
    await page.locator("#rh-login-password").fill("theme-e2e-password");
    await page.locator('.rh-login button[type="submit"]').click();
    await expect(page.getByRole("textbox", { name: "Message #lobby", exact: true })).toBeVisible();
    const you = () => page.getByRole("button", { name: "You", exact: true }).click();
    await you();
    const editor = page.getByRole("region", { name: "Your burrow profile", exact: true });
    const quote = editor.getByLabel("A few words about you", { exact: true });
    const save = editor.getByRole("button", { name: "Save profile", exact: true });
    await expect(quote).toBeVisible();
    await quote.fill("A draft for the night shift");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await you();
    await expect(quote).toHaveValue("A draft for the night shift");
    await editor.getByRole("button", { name: "Revert changes", exact: true }).click();
    await expect(quote).toHaveValue("");
    await expect(save).toBeDisabled();

    await quote.fill("Drawing icons and listening to radio.");
    await editor.getByLabel("Pronouns", { exact: true }).fill("they/them");
    await editor.getByLabel("Your .plan", { exact: true }).fill("Making a little space of my own.");
    await editor.getByRole("button", { name: "Choose icon", exact: true }).click();
    await editor.getByRole("group", { name: "Profile icon", exact: true }).getByRole("button", { name: "owl", exact: true }).click();
    await editor.getByRole("button", { name: "Profile colour 3", exact: true }).click();
    await save.click();
    await expect(editor.getByText("Profile saved.", { exact: true })).toBeVisible();
    const icon = editor.getByRole("img", { name: "Your published profile icon", exact: true });
    await expect(icon).toHaveAttribute("src", /^data:image\/png;base64,/);
    const publishedIcon = await icon.getAttribute("src");
    await page.reload();
    await you();
    await expect(quote).toHaveValue("Drawing icons and listening to radio.");
    await expect(editor.getByLabel("Pronouns", { exact: true })).toHaveValue("they/them");
    await expect(icon).toHaveAttribute("src", publishedIcon!);
    await expect(save).toBeDisabled();

    for (const [name, width, height] of [["desktop", 1280, 900], ["mobile", 390, 844]] as const) {
      await page.setViewportSize({ width, height });
      await editor.scrollIntoViewIfNeeded();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.screenshot({ path: test.info().outputPath(`profile-${name}.png`), fullPage: true });
    }
    await page.setViewportSize({ width: 1280, height: 900 });

    await quote.fill("");
    await editor.getByRole("button", { name: "Remove icon", exact: true }).click();
    await save.click();
    await expect(editor.getByText("Profile saved.", { exact: true })).toBeVisible();
    await page.reload();
    await you();
    await expect(quote).toHaveValue("");
    await expect(icon).toHaveCount(0);
    await expect(editor.getByLabel("Pronouns", { exact: true })).toHaveValue("they/them");
    expect(errors).toEqual([]);
  } finally {
    await server.dispose();
    await test.info().attach("profile-server-log", { body: server.logs(), contentType: "text/plain" });
  }
});
