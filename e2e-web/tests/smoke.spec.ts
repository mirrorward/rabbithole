import { test, expect } from "@playwright/test";

// End-to-end smoke test for the RabbitHole web SPA.
//
// This drives a REAL browser (Chromium) against the real wasm app that
// `burrow --http` serves out of the `trunk build` dist. It proves three things:
//   1. the wasm app boots and mounts (the login/connect view renders);
//   2. a real user interaction works (fill the handle, click Connect);
//   3. the app routes to the post-login lobby surface after that interaction.
//
// Selectors are anchored to stable markup from crates/ui-web/src/components.rs:
//   - the connect form is `<form class="rh-login">` with `#rh-login-handle`;
//   - the lobby's primary nav is `<nav class="rh-nav" aria-label="Primary">`;
//   - the lobby compose box is `<input aria-label="Message the lobby">`.

test("SPA boots, renders the connect view, and a guest login reaches the lobby", async ({
  page,
}) => {
  await page.goto("/");

  // (1) The wasm app mounts and the connect/login view renders. `<body>` ships
  // empty in index.html — this element exists only after the wasm boots and
  // Leptos mounts, so it doubles as a "the app is alive" assertion.
  const connectButton = page.getByRole("button", { name: "Connect" });
  await expect(connectButton).toBeVisible();

  const handle = page.locator("#rh-login-handle");
  await expect(handle).toBeVisible();

  // (2) Real interaction: sign in as a guest through the actual UI controls.
  await handle.fill("guest-e2e");
  await connectButton.click();

  // (3) The post-login lobby surface appears: the primary nav (only rendered
  // once signed in) and the lobby compose box.
  const primaryNav = page.locator('nav.rh-nav[aria-label="Primary"]');
  await expect(primaryNav).toBeVisible();
  await expect(primaryNav.getByRole("link", { name: "Lobby" })).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Message the lobby" }),
  ).toBeVisible();
});
