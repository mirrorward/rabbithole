# Desktop + Mobile Shell — Tauri v2 Scaffold

> The concrete, version-verified scaffold for `apps/desktop` — a Tauri v2 crate that wraps the
> shipped `crates/ui-web` Leptos SPA verbatim (PLAN §2). From the Tauri scoping study
> (`workflows/scripts/scope-tauri-desktop-mobile-*.js`). This is the **Wave F** foundation in
> [`client-experience.md`](client-experience.md); build it after (or alongside) the WarrenState refactor.

## Verified toolchain (this machine)

`tauri-cli 2.9.6` · `tauri 2.11.5` · `tauri-build 2.6.3` · `trunk 0.21.14` · node 22 · Xcode 26.6.
Rust targets installed: `aarch64-apple-ios`, `-ios-sim`, `x86_64-apple-ios`, `aarch64-apple-darwin`, and
all Android ABIs. **Gap:** `ANDROID_HOME`/`ANDROID_NDK_HOME` unset → Android *compiling* needs the SDK/NDK;
iOS is fully buildable.

## Key decisions

- **Standalone crate, EXCLUDED from the root workspace.** `apps/desktop/Cargo.toml` carries its own empty
  `[workspace]` table (own `Cargo.lock`), so `cargo build --workspace` at the repo root stays lean and CI
  Linux jobs don't need `libwebkit2gtk`. Matches the existing `apps/gui/README` intent. It does **not**
  inherit `[workspace.package]`, so version/edition/rust-version are set explicitly.
- **The SPA is reused verbatim.** In dev, Tauri's webview points at `trunk serve`; in release it bundles
  `../../crates/ui-web/dist` produced by `trunk build`. No frontend fork.
- **The load-bearing config is the `beforeDevCommand`/`beforeBuildCommand` `cwd`** — trunk lives in
  `crates/ui-web`, not next to `tauri.conf.json`, so both use the object form with `cwd: "../../crates/ui-web"`.
- **An explicit CSP** (not `null`) that fits the SPA: `style-src 'unsafe-inline'` (the inline
  `<style>{STYLESHEET}</style>`), `script-src 'wasm-unsafe-eval'` (wasm), `connect-src ws: wss:` (the
  `WsClient` transport), `img-src data:` (data-URL avatars), `media-src http: https:` (Icecast radio). Tighten
  `connect-src`/drop `'unsafe-inline'` as the app hardens.
- **`withGlobalTauri: false`** — the SPA is a pure web client that calls no Tauri JS APIs yet, so
  `window.__TAURI__` stays off the page global. Revisit in Wave F when native features need an IPC bridge.
- **v2 lib+bin split** (`src/lib.rs` with `#[cfg_attr(mobile, tauri::mobile_entry_point)] pub fn run()`, a thin
  `src/main.rs`) so the *same* crate is mobile-ready for `cargo tauri ios/android init`.

## File layout

```
apps/desktop/
  Cargo.toml            # own [workspace]; [lib] name=rabbithole_desktop_lib (staticlib,cdylib,rlib) + [[bin]]
  tauri.conf.json       # v2 schema — the load-bearing file (below)
  build.rs              # tauri_build::build()
  src/lib.rs            # run() with #[cfg_attr(mobile, tauri::mobile_entry_point)]
  src/main.rs           # #![cfg_attr(not(debug_assertions), windows_subsystem="windows")] → run()
  capabilities/default.json   # { permissions: ["core:default"], windows: ["main"] }
  icons/                # 32/128/128@2x png + icon.icns + icon.ico (from icon_rgba / the PWA icons)
  .gitignore            # /target, /gen/schemas
```

## The load-bearing config (`tauri.conf.json`)

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "RabbitHole",
  "version": "0.104.0",
  "identifier": "com.mirrorward.rabbithole",
  "build": {
    "beforeDevCommand":  { "script": "trunk serve --address 127.0.0.1 --port 1420", "cwd": "../../crates/ui-web" },
    "beforeBuildCommand":{ "script": "trunk build", "cwd": "../../crates/ui-web" },
    "devUrl": "http://localhost:1420",
    "frontendDist": "../../crates/ui-web/dist"
  },
  "app": {
    "withGlobalTauri": false,
    "windows": [{ "label": "main", "title": "RabbitHole", "width": 1100, "height": 760, "minWidth": 720, "minHeight": 480 }],
    "security": { "csp": "default-src 'self'; connect-src 'self' ws: wss: https:; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self' 'wasm-unsafe-eval'; media-src 'self' http: https: blob:; font-src 'self'" }
  },
  "bundle": { "active": true, "targets": "all", "icon": ["icons/32x32.png","icons/128x128.png","icons/128x128@2x.png","icons/icon.icns","icons/icon.ico"] }
}
```

`src/lib.rs`:

```rust
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| Ok(()))   // later: tray + quick-status, notifications, rabbit:// deep links, updater, in-process core
        .run(tauri::generate_context!())
        .expect("error while running the RabbitHole desktop application");
}
```

## Run / verify

- Desktop dev: `cd apps/desktop && cargo tauri dev` (spawns `trunk serve`, opens the window on the SPA).
- Desktop bundle: `cargo tauri build` (runs `trunk build`, bundles `dist`). Verified: produces a 25 MB
  arm64 `RabbitHole.app` that embeds the live wasm SPA via `generate_context!`.
- iOS: `cargo tauri ios init` (done — `gen/apple`), then `cargo tauri ios build --debug --target aarch64-sim`.
  Verified the Rust core cross-compiles for iOS (`cargo build --lib --target aarch64-apple-ios-sim` →
  `librabbithole_desktop_lib.a`). A device build needs a signing team/provisioning.
- Android: needs `ANDROID_HOME` + `ANDROID_NDK_HOME` first; then `cargo tauri android init`.

## Toolchain gotchas (macOS, this machine) — READ before `tauri ios`

**Always run the `tauri ios` commands with this PATH:**

```bash
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/ruby/bin:/opt/homebrew/bin:$PATH"
```

Two independent traps, both solved by the ordering above:

1. **`pod install` fails: `gem list ... exited with code 126`.** asdf's `ruby` shim has no version
   selected, so `gem`/`pod` via `~/.asdf/shims` exit 126. Putting `/opt/homebrew/opt/ruby/bin` (a working
   `gem`) and `/opt/homebrew/bin` (homebrew cocoapods `pod` 1.16.2) ahead of the asdf shims fixes it.
2. **The frontend `trunk build` then fails `E0463: can't find crate for std` (wasm32).** `/opt/homebrew/bin`
   also contains a homebrew `rustc`/`cargo` that lists `wasm32-unknown-unknown` but has **no installed std**
   for it. If it shadows rustup, the wasm frontend won't compile. Keeping `$HOME/.cargo/bin` **first** makes
   `cargo`/`rustc` resolve to rustup (which has the wasm32 + iOS std), while gem/pod still come from homebrew.

**To actually run on a simulator:** the iOS 26.5 simulator SDK is installed, but only the iOS **26.0
runtime** is downloaded. Get the matching runtime with `xcodebuild -downloadPlatform iOS` (large, ~GBs),
then `cargo tauri ios dev`. Building the app does not need the runtime; running/booting a sim does.

## Native feature layers (subsequent slices, ranked)

1. **Notifications** (`tauri-plugin-notification`) — the SPA's toast/"you've got mail" moments fire a native OS
   notification via a small command / the plugin's JS API. Highest value, lowest effort.
2. **Tray / menubar presence + quick-status popover** (`TrayIconBuilder`, core) — set Online/Away without
   raising the window; a badge for unread across all burrows.
3. **`rabbit://` deep links** (`tauri-plugin-deep-link` + `tauri-plugin-single-instance`) — register the scheme,
   route the URL to the SPA's server browser / a swarm download.
4. **Auto-update** (`tauri-plugin-updater`) — needs a signed release endpoint; stub the config now.

## Deltas the design depends on (cross-refs `client-experience.md`)

- Native **peer swarming** + **background transfers** need the in-process Rust core (PLAN §15) rather than the
  WS transport — the `TransferBackend::NativeBackend`.
- **Mobile push** needs an APNs/FCM relay (owner infra decision).
- Web-client **seeding** needs the WebRTC data-channel gateway (deferred).
