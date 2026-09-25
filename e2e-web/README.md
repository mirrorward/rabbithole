# RabbitHole web SPA — Playwright E2E smoke test

A real-browser end-to-end test of the RabbitHole web SPA (Leptos/wasm, in
`crates/ui-web`) as served by the `burrow` server's embedded HTTP surface.

The test boots the actual wasm app in headless Chromium, asserts the
connect/login view renders, performs a **guest login** through the real UI
(fills the handle, clicks *Connect*), and asserts the app routes to the lobby.

The smoke test drives an already-running `burrow`. The three manual steps below
are its recipe. The separate theme regression suite launches isolated servers
from existing build artifacts; see its instructions at the end.

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

## Signed-theme regression suite

`tests/theme.spec.ts` boots the actual SPA against temporary local burrows. Build
the SPA and `burrow` as above, install npm dependencies, then run from `e2e-web`:

```sh
BURROW_BIN="$(pwd)/../target/debug/burrow" \
SPA_DIST="$(pwd)/../crates/ui-web/dist" \
npm run test:theme
```

Set `PW_CHROMIUM` if reusing a browser installed outside Playwright's default
location. The suite requires Unix (the server control socket is Unix-only) and
Node **22.13 or newer** for the built-in SQLite fixture helper. It does not build
artifacts or install browsers. Without both artifact variables, normal smoke
runs skip these tests.

Each test seeds an account, binds HTTP/WebSocket listeners on random loopback
ports, disables public discovery, and deletes its temporary data after stopping
the servers. Its connection budget allows rapid asset reloads without testing
production rate limits. Browser requests to external hosts are blocked. The opt-out test
seeds the existing account preference in the temporary SQLite database; it does
not assume a client-side opt-out control exists.

The assertions cover signed theme application after login, live publication and
clear without navigating, High Contrast precedence, independent themes while
switching burrows, background updates, reconnect after an offline theme change,
session restoration after loading a fresh document, and server-side account opt-out. They inspect
computed CSS variables and observe real protocol replies so a default palette
shown before authentication cannot masquerade as a successful clear.
Service workers are blocked to isolate socket/theme behavior from shell caching;
fresh-document checks load `/`, since deep-link fallback belongs to the worker.
