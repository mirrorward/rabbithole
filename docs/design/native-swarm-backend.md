# Native Swarm Backend — Build Plan

> Wiring the multi-source swarming download engine (`crates/swarm`) into the Tauri
> desktop core and exposing it to the `ui-web` SPA, so a download pulls chunks from
> many servers **and** peer clients at once, each Bao-verified. From a 6-agent
> scoping study (`workflows/scripts/scope-native-swarm-backend-*.js`), grounded in the
> real crates. This is the design's `NativeBackend` (see `client-experience.md`, Wave D).

## Headline judgments

- **The native RHP client is NOT a large pre-req.** `rabbithole-core` (feature `native`)
  already gives `Client::connect` + auth + `swarm_find`/`swarm_ticket`/`swarm_contact`;
  `rabbithole-swarm` gives `fetch_swarm_resumable` + `PeerServer`/`SeedStore`. The full
  find→ticket→fetch path is loopback-tested today (`apps/server/tests/e2e_w53.rs`,
  `crates/swarm/tests/sim.rs`, `apps/cli` `SwarmAction::Fetch`). Everything below the Tauri
  boundary is **reuse + orchestration**, ~150 lines, not construction.
- **The one genuine crate change** is an additive scheduler progress hook (`crates/swarm/
  src/scheduler.rs`): `fetch_swarm*` return only a terminal `FetchReport`; add an optional
  `mpsc::Sender`/`Fn` sink emitted under the lock right after `s.persist()` for *live*
  per-unit progress. ~10–30 lines; existing fns become thin wrappers. Gates live progress only.
- **The single biggest risk is the wasm↔Tauri IPC seam** (with `withGlobalTauri=false`) — the
  one thing that **cannot be verified headlessly** (no webview to screenshot). De-risk it
  *first and separately* as a throwaway hello-world spike, before the real command surface.
  Note: `#[cfg(target_arch="wasm32")]` is true in **both** web and Tauri, so backend selection
  must be a **runtime** `window.__RH_IS_NATIVE__` detect, not a `cfg`.

## Key APIs (read from the code)

```
scheduler::fetch_swarm_resumable(sources: &[SourcePeer], token: &[u8], root: [u8;32],
    size: u64, dest: &Path) -> Result<FetchReport, PeerError>
    // persists <dest>.rhstate per verified unit, re-hashes the whole file vs root,
    // removes the state on success. A COMPLETED fetch caches nothing (a re-fetch starts fresh);
    // .rhstate exists only to resume an INTERRUPTED fetch.
struct SourcePeer { endpoint: String /* ip:port */, cert_fp: [u8;32] }
struct FetchReport { bytes: u64, per_source: Vec<(String /*endpoint*/, u64 /*UNITS, not bytes*/)> }
                    // terminal only — no live/streaming data without the progress hook
peer::{PeerServer::start(addr, server_pubkey, SeedStore), SeedStore::new()/.add(root, path)}
cap::CapToken::issue(key, root, fetcher, expires_unix).to_bytes()  // server-signed cap
rabbithole_core::Client::{swarm_find(root)->SourceList, swarm_ticket(root)->SourceTicket}
proto::swarm::{SourceList{root, server_has, server_size, sources}, SourceInfo{endpoint:Option, cert_fp:Option, size, …}}
    // SourceInfo -> SourcePeer only when BOTH endpoint & cert_fp are Some
UNIT_SIZE = 1 MiB · PEER_REQUEST_MAX = 4 MiB · Bao block = 16 KiB
```

## Build order (shippable slices)

- **Slice 1 — in-process real fetch + dep-tree gate (DONE, headless).** Added
  `rabbithole-swarm` + `tokio` path deps to `apps/desktop` (its own `[workspace]`), proving
  swarm/quinn/rustls links alongside tauri/wry/webkit (the bloat risk — falsified by one
  `cargo build`). A `#[tokio::test]` in `apps/desktop/tests/swarm_core.rs` stands up N
  localhost `PeerServer`s + a dead endpoint and fetches a multi-unit blob byte-exact through
  the real desktop build. **This retires the dep-tree risk and proves the engine works.**
- **Slice 2 — source discovery in-process (headless).** Factor a Tauri-free
  `async fn run_swarm_download(client, root, size, dest, emit: impl Fn(SwarmEvent))`:
  `client.swarm_find(root)` → filter `SourceInfo`→`SourcePeer` (both endpoint+cert_fp Some) →
  `swarm_ticket` → `fetch_swarm_resumable`. Handle empty-sources + `server_has` → single-stream
  origin fallback. `root` = `FileNodeView.blob_id` (no resolve RPC). Verify with a live-Burrow +
  seeding-PeerServer integration test (the `e2e_w53.rs` style). Ship **single-server** discovery
  first: a `CapToken` verifies against one server's key, so true cross-server unit-stealing needs
  a per-`SourcePeer` token change in `crates/swarm` (flagged, deferred).
- **Slice 3 — IPC seam hello-world (risk spike; needs a real webview).** Independent of swarm:
  build the window via `WebviewWindowBuilder` with an `initialization_script` injecting
  `window.__RH_NATIVE__ = {invoke, listen}` over `window.__TAURI_INTERNALS__` +
  `window.__RH_IS_NATIVE__`. Grant the `core:event` capability. Prove a `ping()->pong` invoke and
  a `test://tick` event reach a wasm listener via `cargo tauri dev`. Ships nothing user-facing but
  retires the only non-headless unknown.
- **Slice 4 — real Tauri command/event surface.** Prereq: the scheduler progress hook. A
  `TransfersManager` in Tauri `State` (`Mutex<Option<Client>>` — `Client` is `!Sync`; take the
  lock only for find/ticket, drop before the lock-free fetch; `HashMap<u64, JoinHandle>` for
  cancel via `JoinHandle::abort()` — safe with resumable's per-unit persist). Commands:
  `native_available`, `connect_native`, `swarm_start_download`, `swarm_abort`. Event contract:
  `#[serde(tag="kind")] enum SwarmEvent { Opened{…}, Chunk{…}, Failed{…}, Done{per_source} }`.
- **Slice 5 — ui-web `TransferBackend` wiring (last).** Host-tested
  `wire::swarm_event_to_file_events(&SwarmEvent) -> Vec<FileEvent>` (mirrors `frame_to_file_events`);
  a `TransferBackend` trait with `WebBackend` (today's WS path) + `NativeBackend` (`rh_invoke` +
  event listener — keep the `Closure` alive in a `StoredValue` or it silently dies). Branch
  `AppState::download` on the runtime `native_available()`. The native listener reuses the existing
  `files.update(|f| f.apply(&event))` closure verbatim — **zero view/reducer forks.**

## Verifying Slice 3 (the IPC spike) — needs a GUI

Slice 3 is the one piece that can't be verified headlessly. To confirm the round-trip:

```
cd apps/desktop
cargo tauri dev            # opens the RabbitHole window on your screen
```

Then open the webview devtools (right-click → Inspect, or the app menu) and watch the
**console**. The `NATIVE_SHIM` init script self-tests both directions; you should see:

```
[rh-native] invoke ping -> pong: slice-3        # JS -> Rust command works
[rh-native] listening for test://tick
[rh-native] event test://tick -> hello from the native core   # Rust -> JS event works (~1s after launch)
```

Also confirm `window.__RH_IS_NATIVE__ === true` and `window.__TAURI__ === undefined` (the
global stayed off; only the low-level internals bridge is used). If `listen` errors, the
`plugin:event|listen` payload shape or the `core:event` capability needs adjustment — the
`invoke` line proves the core IPC regardless, and the fix is localized to the shim.
Once both lines print, the last non-headless unknown is retired and Slices 4b/5 (the real
swarm command surface + ui-web `TransferBackend`) proceed on proven ground.

## Deferred (explicitly)

Per-unit Bao verify-error surfacing (roster health column); per-`SourcePeer` token scheduler
change (unlocks single-fetch cross-server swarming); a slimmer `core` feature (`native-net`
without `store-client`/rusqlite) to cut desktop build weight — optimization, not a blocker.
