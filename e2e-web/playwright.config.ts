import { defineConfig, devices } from "@playwright/test";

// The RabbitHole web SPA is a Leptos/wasm app. It is built with `trunk build`
// (output in `crates/ui-web/dist/`) and served by `burrow --http`. This harness
// does not build artifacts. Providing both artifact paths runs every suite
// against isolated servers. The smoke test alone also supports a manual server.
//
// BASE_URL points at the running burrow HTTP surface (default matches the
// README's manual example). PW_CHROMIUM overrides the browser binary;
// otherwise Playwright resolves the revision installed for its locked package.
const baseURL = process.env.BASE_URL ?? "http://127.0.0.1:8791";

// Developers may reuse an existing Chromium; CI installs the pinned revision.
const executablePath = process.env.PW_CHROMIUM || undefined;

if (!!process.env.BURROW_BIN !== !!process.env.SPA_DIST) {
  throw new Error("Provide both BURROW_BIN and SPA_DIST, or neither for manual smoke testing");
}
if (process.env.CI && (!process.env.BURROW_BIN || !process.env.SPA_DIST)) {
  throw new Error("CI must provide BURROW_BIN and SPA_DIST; isolated browser tests must not be skipped");
}

export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [["list"], ["html", { open: "never" }]],
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
