# RabbitHole: Cross-Platform Rust Architecture Brief

## Overview

RabbitHole targets an unusually wide matrix: CLI + TUI + embedded web + native GUI, each as both client and server, across desktop (macOS/Windows/Linux) and mobile (iOS/iPadOS/Android), plus a radio/audio feature. The only way this stays sane is a **shared-core, thin-frontend** architecture: a single `rabbithole-core` crate that owns all domain logic, protocol, state, networking, storage, and audio, with each frontend reduced to a rendering/input adapter over that core.

The central architectural decision is: **the core must be UI-framework-agnostic and expose a message-passing (command/event) API**, not framework-specific types. Every frontend — whether a webview, a wasm SPA, a native GUI, or a TUI — drives the same `Command → Core → Event` loop. Get that boundary right and the frontend choice becomes swappable and low-risk; get it wrong and you couple your business logic to a rendering library and repeat yourself four times.

My headline recommendation: **Tauri v2 for the native GUI on all six platforms**, with the UI written once as a **Rust→wasm SPA (Leptos)** that is reused verbatim for the embedded web client and server admin panel; **ratatui** for the TUI; and a **QUIC (quinn) + length-delimited postcard** protocol from the shared core. Details and the reasoning for each below.

## Key Features / Capabilities (target matrix)

| Frontend | Desktop | Mobile | Delivery |
|---|---|---|---|
| CLI | ✅ | (dev/ops) | binary via `dist` |
| TUI (ratatui) | ✅ | n/a | binary via `dist` |
| Native GUI (Tauri v2) | ✅ macOS/Win/Linux | ✅ iOS/iPadOS/Android | bundler + app stores |
| Embedded web client | served by any device | served by any device | wasm bundle served over HTTP |
| Server admin web | served by server | served by server | wasm bundle served over HTTP |
| Server (headless) | ✅ | — | Docker + `dist` binaries |
| Radio/audio | ✅ | ✅ (native backends) | in-app |

Cross-cutting: shared protocol, shared auth/session, shared state model, shared migrations, offline-capable local DB, streaming audio.

## Technical Notes

### Native GUI — Tauri v2 (recommended)

**Mobile maturity (as of 2026):** Tauri 2.0 went stable Oct 2024 with iOS and Android as first-class targets. The IPC/command layer, plugin system, and bundler are production-grade on desktop. Mobile is stable-API but *thinner*: the built-in plugin set (notifications, dialogs, deep-link, biometric, clipboard, NFC, barcode) works, but not every desktop plugin has a mobile implementation, and you will occasionally write platform-native glue (Swift/Kotlin) via Tauri's mobile plugin model for anything exotic. For a client that is mostly "render our UI + call into our Rust core," this is a good fit.

- **How it bundles UI:** Tauri ships a system webview (WKWebView on macOS/iOS, WebView2 on Windows, WebKitGTK on Linux, Android WebView) and loads your frontend assets into it. Critically, **your Rust core links into the same binary** and is reachable from the webview through IPC — so on mobile you are not spawning a separate process; the core runs in-process.
- **IPC/commands:** `#[tauri::command]` functions are the RPC surface; the webview calls them via `invoke()`. Events (`app.emit`) push from Rust to the UI. This maps cleanly onto our `Command`/`Event` core API — Tauri commands become thin translators.
- **Sidecar:** Tauri can bundle and supervise external binaries (e.g. ship the CLI or a helper as a sidecar) on desktop. **Note: sidecar/external-process is not usable on mobile** (app-store sandboxing), which is *why* the core must be a linkable library, not a spawned process.
- **Plugin model:** Rust-side plugins with optional per-platform native code; this is where you'd wrap anything the core can't do portably.

**Why Tauri over a pure-Rust GUI here:** it's the only option that credibly covers *all six* targets today with one UI codebase, gives you the mature web layout/text/accessibility engine for free, and lets you reuse the exact same UI as the embedded web client (see below).

### Pure-Rust GUI alternatives (evaluated, not primary)

- **egui** — immediate-mode, superb for tools/dashboards/overlays and dead-simple to embed. Desktop + wasm are solid; **mobile is unofficial/rough** (no first-class iOS story). Best fit for RabbitHole as the **TUI's richer sibling or an internal debug/ops GUI**, not the shipping consumer app.
- **iced** — Elm-style, retained, wgpu-rendered. Desktop good; web via wasm; **mobile is experimental** (recent Android demos exist but it's not production). Nice architecture (its message model mirrors our core's), but mobile risk is disqualifying for the primary GUI.
- **Slint** — declarative `.slint` markup, native/wgpu rendering, very light (great even on MCUs). **Android (Rust) supported; iOS was in-progress/NLnet-funded and maturing through 2025** — verify current iOS status before committing. Strong contender if you want a *truly native* (non-webview) look and are willing to accept iOS being newer.
- **Dioxus 0.6** — React-like, single codebase for web (wasm), desktop, and mobile; `dx` CLI does `bundle`/`serve` for iOS/Android. Mobile rendering is **webview-based (like Tauri)**, so it's essentially "Tauri-shaped" with a nicer unified DX and RSX components; an experimental native (WGPU/Blitz) renderer is advancing but not yet the default. **Dioxus is the strongest pure-Rust alternative** and worth a spike — its appeal is that the same RSX components serve web, desktop, and mobile without the JS/wasm boundary Tauri implies.

**Bottom line:** Tauri (mature, webview) vs Dioxus (Rust-native components, webview under the hood on mobile) is the real decision. Tauri wins on maturity and store track record today; Dioxus wins on Rust-purity and unified DX. Slint is the pick only if native (non-webview) rendering is a hard requirement.

### TUI — ratatui + crossterm

Uncontested. **ratatui** (immediate-mode widgets) over **crossterm** (portable backend: Windows/macOS/Linux, raw mode, events). Structure the TUI as another adapter over the core's `Command`/`Event` loop; use `tokio::select!` to merge crossterm's event stream with core events. Desktop-only by nature (mobile has no terminal), which is fine.

### Web frontend — Rust→wasm SPA (recommended: Leptos)

For **both** the embedded web client and the server admin panel, compile a Rust SPA to wasm rather than maintaining a separate TS/JS app:

- **Leptos** (recommended) — fine-grained reactivity, small wasm, strong SSR/hydration, mature router. Best-in-class runtime performance and the cleanest "share Rust types with core over the wire" story.
- **Dioxus** — viable and lets you *literally reuse the same components* if you also pick Dioxus for the GUI. This is the compelling case for going all-Dioxus.
- **Yew** — mature but heavier VDOM; less compelling in 2026 than Leptos/Dioxus.
- **TS/JS SPA over websockets** — only justified if you need to hire JS talent or leverage a JS component ecosystem. It re-introduces a second language, duplicate DTOs, and a serde/TS type-sync problem. **Avoid** unless there's an org reason.

Reuse mechanics: the wasm SPA talks to the core via **websockets** (browsers can't do raw QUIC/TCP). Because the SPA is Rust, it can `serde`-share the exact `Command`/`Event` types with the core crate — no schema drift.

**Tauri synergy:** if the GUI is Tauri, the *same* Leptos/Dioxus wasm UI can be the webview content on desktop/mobile *and* the served embedded-web client. One UI, three deployment modes (webview, served-to-browser client, served-to-browser admin). This is the single biggest code-reuse win available and the reason I lean Tauri+Leptos or all-Dioxus.

### Shared core crate

- **Async runtime:** `tokio` (multi-thread on server/desktop, current-thread where appropriate on mobile). On wasm, gate out tokio's net/time and use `wasm-bindgen-futures`; keep core logic runtime-agnostic where possible so the wasm SPA can reuse types/logic without a full tokio dependency.
- **Networking:** primary transport **QUIC via `quinn`** (multiplexed streams, built-in TLS 1.3, great for a radio/streaming + control-channel app, mobile-friendly connection migration). Provide a **websocket transport (`tokio-tungstenite`)** as the second, mandatory transport because **browsers/wasm can't speak raw QUIC** and some corporate networks block UDP — the admin/web clients ride websockets, native clients prefer QUIC and fall back to WS. `rustls` everywhere (no OpenSSL headaches on mobile/cross-compile). Abstract both behind a `Transport` trait in core.
- **Serialization:** `serde` + **`postcard`** as the wire format for the native/QUIC protocol — it's `no_std`-friendly, compact, extremely fast, and pure-Rust (ideal since both ends are Rust). Use **length-delimited framing** on streams. For the websocket/web path, postcard-over-binary-frames works too since the wasm client is also Rust. Choose **prost/protobuf only if** you need cross-language clients or schema-governed evolution; skip `bincode` (postcard is smaller/more stable) and reserve `rmp`/MessagePack for cases needing self-describing/dynamic payloads. Recommendation: **postcard**, with an explicit protocol-versioning byte and enums marked `#[non_exhaustive]` for forward-compat.
- **Embedded DB:** two tiers.
  - **Server:** **SQLite via `sqlx`** (async, compile-time-checked queries, `sqlx migrate`) or Postgres if you outgrow single-node — keep the repository layer behind a trait so the backend is swappable.
  - **Client/mobile local store:** **`rusqlite`** (bundled SQLite, no async runtime needed, trivial to cross-compile to iOS/Android, `refinery`/`rusqlite_migration` for migrations) is the safe default. **`redb`** (pure-Rust embedded KV, no C toolchain) is an attractive alternative if you want to avoid the SQLite C dependency entirely on mobile and your access patterns are key/value-ish. **Avoid `sled`** — it's effectively unmaintained/in-limbo; use `redb` instead if you want pure-Rust.
  - Migrations: version the schema in-code and run migrations on startup for embedded stores.

### Audio / radio feature

- **Playback device I/O:** `cpal` — cross-platform (CoreAudio/WASAPI/ALSA + Android AAudio/Oboe path and iOS). The portable base layer.
- **Decoding:** `symphonia` — pure-Rust decoders (Ogg/Opus, FLAC, MP3, AAC, etc.), no C deps, cross-compiles cleanly to mobile.
- **Higher-level playback:** `rodio` (built on cpal) for mixing/volume/queueing if you want batteries-included playback rather than driving cpal directly.
- **Encode/stream (radio):** Opus in Ogg. Use `opus` (libopus binding) or `audiopus` for encoding; `ogg`/symphonia for containerization. Stream Opus frames over a **dedicated QUIC stream** (or WS for web listeners) separate from the control channel — QUIC's independent streams shine here (control latency isn't blocked by audio backpressure).
- **Mobile caveat:** background audio, audio-session/interruption handling, and store policies require per-platform native hooks — implement these as Tauri mobile plugins (or JNI/Swift shims) layered over the portable cpal/symphonia core.

### Packaging & distribution

- **CLI/TUI/server binaries:** **`dist`** (formerly `cargo-dist`, actively maintained, ~v0.32) generates cross-platform archives, shell/PowerShell/MSI/Homebrew installers, checksums, and GitHub Releases CI. (Minor caveat: keep an eye on maintenance funding, but it's healthy and the generated CI is standard-enough to maintain independently if needed.)
- **Native GUI:** **Tauri bundler** — `.dmg`/`.app`, `.msi`/`.exe` (NSIS), `.deb`/`.rpm`/AppImage on desktop; `.ipa`/`.aab` for iOS/Android. Handles code-signing/notarization hooks.
- **App stores:** iOS via App Store Connect / TestFlight, Android via Play Console (`.aab`). Budget for review cycles, background-audio entitlements, and privacy manifests.
- **Server:** multi-stage **Docker** image (build with cargo-chef for layer caching → distroless/`debian-slim` runtime), plus raw binaries from `dist` for bare-metal.

### Recommended workspace layout

```
rabbithole/                      # cargo workspace root
├─ Cargo.toml                    # [workspace], shared deps in [workspace.dependencies]
├─ crates/
│  ├─ core/                      # rabbithole-core: domain logic, state, Command/Event API
│  │                             #  (no UI, no tokio-net in the pure-logic modules)
│  ├─ proto/                     # wire types + postcard framing + protocol version
│  ├─ net/                       # Transport trait; quinn + tungstenite impls, rustls
│  ├─ store-server/              # sqlx repo impls + migrations
│  ├─ store-client/              # rusqlite/redb local store + migrations
│  ├─ audio/                     # cpal/symphonia/rodio + opus encode/stream
│  └─ ui-web/                    # Leptos (or Dioxus) SPA — used by webview, web client, admin
├─ apps/
│  ├─ cli/                       # clap-based CLI over core
│  ├─ tui/                       # ratatui + crossterm over core
│  ├─ server/                    # headless server (axum for HTTP/WS + admin static, quinn listener)
│  └─ gui/                       # Tauri v2 app; embeds ui-web; links core; desktop+mobile
└─ dist-workspace / Dockerfile / tauri.conf.json
```

Rules that keep reuse high: **core depends on nothing UI**; frontends depend on core, never each other; the `Transport` and `Repository` traits live in core so platform-specific impls plug in; the wasm SPA compiles the same `proto` types the server uses.

## Pitfalls & Lessons

- **Don't couple business logic to a GUI framework.** Everything reusable lives in `core`; frontends are adapters. This is the difference between one platform matrix and four.
- **Tauri mobile is stable-API but plugin-incomplete.** Assume you'll write some Swift/Kotlin glue; don't rely on a desktop plugin existing on mobile. No sidecars on mobile — core *must* be a linked library.
- **Browsers can't do QUIC/TCP.** You cannot have a single transport; the web/admin clients force a websocket path. Design the `Transport` trait for this from day one.
- **wasm ≠ native tokio.** Feature-gate net/time/fs in core so the wasm SPA reuses your logic and types without dragging in a full runtime. Keep core logic sync/`async`-runtime-agnostic where feasible.
- **`sled` is a trap** (maintenance limbo) — use SQLite (`rusqlite`) or `redb`.
- **Serialization forward-compat:** version your protocol explicitly, use `#[non_exhaustive]` enums and optional fields; postcard is *not* self-describing, so schema discipline matters (add a version byte).
- **Cross-compilation toolchains** for iOS/Android with C-linked deps (SQLite, libopus) are the #1 build-time headache — prefer pure-Rust (`symphonia`, bundled `rusqlite`, `rustls`, `redb`) to minimize NDK/C pain; test the mobile build in CI early, not late.
- **Audio on mobile** needs native audio-session/background handling and store entitlements — portable cpal/symphonia gets you 80%, the last 20% is per-platform.
- **Text/layout/accessibility** are effectively free in a webview (Tauri/Dioxus) and *expensive* to build well in egui/iced/Slint — a real factor if your UI is content-heavy.

## Implications for RabbitHole — Recommended Stack

**Primary recommendation (maturity-first):**
- **UI (once):** **Leptos** wasm SPA — reused as (a) Tauri webview content on desktop+mobile, (b) the embedded web client, (c) the server admin panel.
- **Native GUI:** **Tauri v2** on all six platforms; core linked in-process; Tauri commands are thin translators to core's `Command`/`Event` API; native glue via Tauri mobile plugins as needed.
- **TUI:** **ratatui + crossterm**, adapter over core.
- **CLI:** **clap**, adapter over core.
- **Core:** **tokio**; **quinn (QUIC)** primary + **tokio-tungstenite (WS)** mandatory second transport, both `rustls`; **serde + postcard** length-delimited wire format with a version byte.
- **Storage:** **sqlx/SQLite** server-side; **rusqlite** (or **redb** if going pure-Rust) client/mobile; in-code migrations.
- **Audio:** **cpal + symphonia + rodio**, **Opus/Ogg** streamed over a dedicated QUIC (or WS) stream.
- **Server:** **axum** (HTTP + WS + serve admin/web wasm) alongside the quinn listener; **Docker** (cargo-chef → distroless) + `dist` binaries.
- **Packaging:** **Tauri bundler** (GUI + app stores) and **`dist`** (CLI/TUI/server).

**Strong alternative worth a 1–2 week spike:** **all-Dioxus** (Dioxus for web SPA *and* the GUI). If the spike shows its mobile webview + tooling meets your bar, you gain literal component reuse across web/desktop/mobile in one Rust codebase and drop the Tauri/JS-boundary conceptual overhead. Choose **Slint** only if non-webview *native* rendering is a hard product requirement (and confirm current iOS maturity first).

Either way, the durable decision is the **shared `core` with a `Command`/`Event` API and pluggable `Transport`/`Repository` traits** — that's what makes the frontend choice reversible and the platform matrix affordable.

**Sources:** [Tauri 2.0 Stable](https://v2.tauri.app/blog/tauri-20/) · [Tauri Mobile Plugin Dev](https://v2.tauri.app/develop/plugins/develop-mobile/) · [Dioxus 0.6 release](https://dioxuslabs.com/blog/release-060/) · [Dioxus mobile renderers](https://deepwiki.com/DioxusLabs/dioxus/5.5-mobile-renderers-(iosandroid)) · [Slint on iOS (NLnet)](https://nlnet.nl/project/SlintiOS/) · [iced on Android (HN)](https://news.ycombinator.com/item?id=46350641) · [Rust GUI state (LogRocket)](https://blog.logrocket.com/state-rust-gui-libraries/) · [dist / cargo-dist](https://opensource.axo.dev/cargo-dist/) · [dist installers](https://opensource.axo.dev/cargo-dist/book/installers/index.html)
