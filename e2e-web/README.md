# RabbitHole browser integration tests

These tests run the real Leptos/WASM application against real `burrow` servers
in Chromium. CI builds both artifacts, installs the browser revision selected
by the locked Playwright package, and runs the complete suite.

## Run the isolated suite

Prerequisites: Unix, Rust with the `wasm32-unknown-unknown` target, Trunk
**0.21.14**, and Node **22.13 or newer**. The server control socket is Unix-only;
the account-preference fixture uses Node's built-in SQLite module.

From the repository root:

```sh
cargo build --locked -p burrow --bin burrow
(cd crates/ui-web && trunk build --locked --public-url /)
cd e2e-web
npm ci
./node_modules/.bin/playwright install chromium

BURROW_BIN="$(pwd)/../target/debug/burrow" \
SPA_DIST="$(pwd)/../crates/ui-web/dist" \
npm test
```

Trunk needs a `wasm-bindgen` CLI matching the `wasm-bindgen` package version in
`Cargo.lock`. It can resolve the matching helper itself. CI reads that exact
version from the lockfile, installs it explicitly, checks the executable's
version, and supplies `TRUNK_TOOLS_WASM_BINDGEN` to the build. Neither build
enables the seeded `demo` feature.

`npm ci` uses `package-lock.json`; Playwright and its Chromium revision are
pinned together. Set `PW_CHROMIUM` to an existing compatible browser executable
to reuse a local installation instead of downloading one. Linux machines may
also need `playwright install --with-deps chromium`, as used in CI.

Providing `BURROW_BIN` and `SPA_DIST` starts isolated loopback servers with
temporary databases and public discovery disabled. Each test stops its servers
and removes its data in `finally`. Browser directory requests to external hosts
are blocked. Service workers are disabled in the transport/theme/navigation suites so cached
shells cannot hide an HTTP or WebSocket failure. The PWA suite explicitly enables
real service workers and gives each fixture its own writable copy of the app. No credentials or tokens are recorded by protocol observers.

## What runs

| Command | Checks |
| --- | --- |
| `npm run test:smoke` | App mount, real guest sign-in (`AuthGuest`/`AuthOk`), connected lobby and composer. |
| `npm run test:theme` | Signed theme loading, live publish/clear, High Contrast, multiple burrows, reconnect, session restoration, and account opt-out (two tests). |
| `npm run test:routes` | Direct `/lobby`, actual hard reload and token resume, nested routes and real JS/WASM assets, missing-asset 404s. |
| `npm run test:chat` | Recent chat history for a second client, room isolation, and reconnection. |
| `npm run test:keynav` | Focus recovery in Members after filtering, no focus stealing, route disposal/remount, and Boards Tab order with arrow, Enter, pointer and modified-click navigation. |
| `npm run test:radio` | Radio playback refusal, explicit retry, stream errors, and stale outcomes after station changes or stopping. |
| `npm run test:pwa` | Real service-worker updates, explicit reload, cached offline navigation, connection recovery, and native exclusion. |
| `npm run test:keepalive` | Initial app load and immediate reload with every production connection/request rate limit unchanged. |

Theme and route fixtures use a generous connection budget to isolate their
behavior. `keepalive.spec.ts` explicitly requests default limits and disables
the browser cache through request interception; both document loads must fetch
real assets without connection resets or HTTP errors. It introduces no pause
or rate-budget reset between loads.

The account opt-out test seeds the existing preference in its temporary database;
it does not assume an absent client-side opt-out control exists. Route tests
observe token resumption after reload without another password submission.

The radio fixture uses authenticated source ingestion and metadata on a real
burrow for station discovery, with generated WAV audio served from an isolated
HTTP endpoint. The playback-refusal case deliberately injects `NotAllowedError`;
it verifies client recovery without claiming to exercise Chromium’s autoplay
policy. Retry uses the native media decoder and advancing playback time, and a
real HTTP 503 verifies stream-failure recovery. Delayed promises and old media
events are separately injected to verify that obsolete attempts cannot change
the current player. The radio engine and encoders are outside these tests.

## Manual guest smoke

The guest smoke also supports an already-running server. Omit both artifact
variables and provide its HTTP origin and WebSocket address:

```sh
BASE_URL=http://127.0.0.1:8791 \
WS_URL=ws://127.0.0.1:4654 \
npm run test:smoke
```

`BASE_URL` defaults to `http://127.0.0.1:8791`; absent `WS_URL`, the form's
address is used. The other suites skip when build artifacts are absent. CI
rejects missing artifact variables before test discovery, so a passing CI job
cannot silently omit its isolated browser tests.

## Failure evidence and cleanup

Playwright retains failure traces and screenshots in `test-results/`, and its
HTML report in `playwright-report/` includes attached server/protocol diagnostics.
Open it with `./node_modules/.bin/playwright show-report`. CI uploads those
directories plus tool-install, WASM/server-build, and browser-test logs on failure.

CI sets `BURROW_E2E_ROOT` to an owned temporary directory. Fixtures store their
PID and a bounded server log there until disposal. An `always()` cleanup step
checks Linux `/proc` against the exact executable and data-directory arguments
before stopping any surviving fixture, preserving its log, and removing its
temporary data. This is a backstop for interrupted workers; ordinary local
tests clean up without it. `npm run cleanup` invokes that Linux-only backstop
when both `BURROW_E2E_ROOT` and `BURROW_BIN` are set.

## Personal settings

`tests/customization.spec.ts` exercises persisted appearance and chime settings,
live OS appearance, High Contrast, and nested text-editing keys in the real SPA.
The audio case uses native Web Audio nodes with explicitly injected resume
refusal/pending promises for recovery and route-disposal coverage; it does not
claim to reproduce a browser's autoplay policy.
