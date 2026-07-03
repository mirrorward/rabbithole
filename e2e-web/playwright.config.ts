import { defineConfig, devices } from "@playwright/test";

// The RabbitHole web SPA is a Leptos/wasm app. It is built with `trunk build`
// (output in `crates/ui-web/dist/`) and served by `burrow --http`. This harness
// does NOT build or launch anything itself: it drives an already-running burrow.
// See README.md for the launch recipe.
//
// BASE_URL points at the running burrow HTTP surface (default matches the
// README's `--http-addr`). PW_CHROMIUM overrides the browser binary; it
// defaults to the Chromium pre-installed in this environment under
// PLAYWRIGHT_BROWSERS_PATH, so `playwright install` is never needed.
const baseURL = process.env.BASE_URL ?? "http://127.0.0.1:8791";

// Resolve the pre-installed Chromium. The environment ships it at
// /opt/pw-browsers/chromium-<rev>/chrome-linux/chrome; PW_CHROMIUM lets CI or a
// dev override it. When unset we let Playwright use its bundled resolution via
// PLAYWRIGHT_BROWSERS_PATH (channel/executablePath both omitted).
const executablePath = process.env.PW_CHROMIUM || undefined;

export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  timeout: 30_000,
  expect: { timeout: 15_000 },
  use: {
    baseURL,
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        launchOptions: executablePath ? { executablePath } : {},
      },
    },
  ],
});
