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
| `npm run test:theme` | Signed themes, live updates, High Contrast, reconnect, account opt-out, full/minimal/off defaults and overrides, unsupported peers, and delayed preference replies. |
| `npm run test:routes` | Direct `/lobby`, actual hard reload and token resume, nested routes and real JS/WASM assets, missing-asset 404s. |
| `npx playwright test tests/auth.spec.ts` | Correlated authentication failures, delayed handshakes, early pane requests, corrected password retry, and explicit saved-sign-in resume. |
| `npx playwright test tests/bookmarks.spec.ts` | Opt-in account bookmarks, two users on one burrow, legacy migration, independent expiry/removal, and storage failures. |
| `npm run test:chat` | Recent chat history for a second client, room isolation, and reconnection. |
| `npm run test:keynav` | Focus recovery in Members after filtering, no focus stealing, route disposal/remount, and Boards Tab order with arrow, Enter, pointer and modified-click navigation. |
| `npm run test:radio` | Radio playback refusal, retry, stream errors, stale outcomes, optional chime ducking, and two-listener request/vote convergence with station isolation, reconnect, disappearing stations, and older peers. |
| `npm run test:pwa` | Real service-worker updates, explicit reload, cached offline navigation, connection recovery, and native exclusion. |
| `npm run test:keepalive` | Initial app load and immediate reload with every production connection/request rate limit unchanged. |

Theme and route fixtures use a generous connection budget to isolate their
behavior. `keepalive.spec.ts` explicitly requests default limits and disables
the browser cache through request interception; both document loads must fetch
real assets without connection resets or HTTP errors. It introduces no pause
or rate-budget reset between loads.

One account opt-out test seeds the existing preference in its temporary database;
the preference tests also exercise the shipped controls and verify the account
flag stored by the real server. An intercepted `Unsupported` reply models older
peers, and a held real preference reply exercises stale-read protection. Route
tests observe token resumption after reload without another password submission.

Radio queue tests seed isolated library metadata with audio delivery disabled,
then hold the mounts with real local source-ingest sessions. The server runtime
and WebSocket requests/votes drive both browser clients. They
do not verify uploads or media decoding. An intercepted watch refusal models an
older server, and redelivery of a captured queue reply checks station-switch
isolation. Reconnect closes an observed native WebSocket while offline emulation
holds subsequent attempts; another listener mutates the queue before real token
resume. Playback and ducking are exercised separately by `radio.spec.ts`.

Appearance offers a device-wide Full/Minimal/Off default and an override for the
current burrow/account. Overrides use the canonical endpoint and original login;
an unknown login or guest keeps its local choice only for the current session.
Full applies validated color and shared tokens; Minimal drops shared geometry and
type overrides; Off applies no server tokens. This rich-client renderer does not
fetch or play theme banner/icon assets, ANSI artwork, or server sounds, so their
current cap is zero in Minimal as well as the existing renderer. It does not
change personal profile pictures, radio artwork, or message chimes.

The current account protocol stores on/off only: explicit Full/Minimal enables
server themes, Off disables them, and Use default clears the account opt-out so
the local default can apply. The global default does not silently change remote
account preferences. On connection, an existing account opt-out is respected
unless an explicit local override exists; that override is reconciled to the
server. Minimal remains local. Unsupported or failed requests preserve the local
choice and display a fallback notice; blocked local storage is reported separately.

The radio fixture uses authenticated source ingestion and metadata on a real
burrow for station discovery, with generated WAV audio served from an isolated
HTTP endpoint. The playback-refusal case deliberately injects `NotAllowedError`;
it verifies client recovery without claiming to exercise Chromium’s autoplay
policy. Retry uses the native media decoder and advancing playback time, and a
real HTTP 503 verifies stream-failure recovery. Delayed promises and old media
events are separately injected to verify that obsolete attempts cannot change
the current player. Ducking tests retain native media decoding and Web Audio
nodes, advance a controlled JavaScript clock through the gain envelope, and
control the document-focus input and chime-resume refusal. Real messages arrive
from a second signed-in client. These tests verify gain changes and continuity,
not OS audibility or native autoplay-policy decisions. The radio engine and
encoders are outside these tests.

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

## Desktop navigation bridge

`tests/native-menu.spec.ts` runs the compiled SPA and native shim with an injected
Tauri IPC boundary. It checks menu navigation, palette dismissal, fragment
history, reload and fullscreen events. The separate Node shim suite runs in CI.
These checks do not claim native AppKit/WKWebView event delivery.

## Public profiles

`tests/profile.spec.ts` exercises the actual editor: draft navigation/revert,
text and icon publication, hard-reload persistence, and explicit clearing.
The server profile integration test separately verifies observer reads, restart,
burrow/persona isolation and guest refusal through real WebSocket clients.
