# RabbitHole web SPA — Playwright E2E smoke test

A real-browser end-to-end test of the RabbitHole web SPA (Leptos/wasm, in
`crates/ui-web`) as served by the `burrow` server's embedded HTTP surface.

The test boots the actual wasm app in headless Chromium, asserts the
connect/login view renders, performs a **guest login** through the real UI
(fills the handle, clicks *Connect*), and asserts the app routes to the lobby.

This harness does **not** build or launch the SPA or the server for you — it
drives an already-running `burrow`. The three manual steps below are the whole
recipe.

## Prerequisites

- **Rust** with the `wasm32-unknown-unknown` target
  (`rustup target add wasm32-unknown-unknown`).
- **[`trunk`](https://trunkrs.dev/)** — `cargo install trunk --locked`. Trunk
  auto-fetches a matching `wasm-bindgen-cli` on first build; if it reports a
  version mismatch, install the version pinned in `Cargo.lock`
  (`cargo install wasm-bindgen-cli --version <that-version> --locked`).
- **Node 22** and npm.
- **Chromium** for Playwright. In the standard dev/CI image it is
  pre-installed under `PLAYWRIGHT_BROWSERS_PATH`; point `PW_CHROMIUM` at the
  binary (see step 3). Otherwise run `npx playwright install chromium` once.

## 1. Build the SPA

```sh
cd crates/ui-web
trunk build            # add --release for an optimized build; debug is fine
```

Output lands in `crates/ui-web/dist/` (gitignored). Note its absolute path for
the next step.

## 2. Run burrow serving that dist

From the workspace root:

```sh
cargo build -p burrow

DATA=$(mktemp -d)
target/debug/burrow \
  --data-dir "$DATA" \
  --http \
  --http-addr 127.0.0.1:8791 \
  --web-root "$(pwd)/crates/ui-web/dist" \
  run &
```

`--web-root`/`--http-addr` each imply `--http`. Confirm the SPA index is being
served:

```sh
curl -sSf http://127.0.0.1:8791/ | grep -q '<title>RabbitHole</title>' && echo OK
```

The guest login in the test is a client-side connect (the connect view signs in
with just a handle), so no account seeding is required for the smoke test. If
you later add tests that need a password account:
`target/debug/burrow --data-dir "$DATA" ctl account-create <login> <password>`.

## 3. Install deps and run the test

```sh
cd e2e-web
npm install            # set PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 to reuse a
                       # pre-installed Chromium

# Point at the pre-installed Chromium (skip if you ran `playwright install`):
export PW_CHROMIUM=/opt/pw-browsers/chromium-1194/chrome-linux/chrome

# BASE_URL defaults to http://127.0.0.1:8791 (matches step 2). Override if you
# ran burrow on a different address:
# export BASE_URL=http://127.0.0.1:8791

npm test               # == npx playwright test
```

### Configuration knobs

| Env var       | Default                     | Purpose                                   |
| ------------- | --------------------------- | ----------------------------------------- |
| `BASE_URL`    | `http://127.0.0.1:8791`     | Where the running burrow serves the SPA.  |
| `PW_CHROMIUM` | (Playwright's bundled path) | Absolute path to the Chromium binary.     |

## What the test proves

`tests/smoke.spec.ts` (single test):

1. **The wasm app boots.** `index.html` ships an empty `<body>`; the connect
   view's *Connect* button and `#rh-login-handle` input exist only after the
   wasm mounts, so their visibility is the "app is alive" assertion.
2. **A real interaction works.** It fills the handle field and clicks *Connect*.
3. **Post-login routing works.** It asserts the primary nav
   (`nav.rh-nav[aria-label="Primary"]`, only rendered once signed in), its
   *Lobby* link, and the lobby compose box
   (`aria-label="Message the lobby"`) all become visible.

All waits are deterministic (`expect(...).toBeVisible()`); there are no fixed
sleeps.
