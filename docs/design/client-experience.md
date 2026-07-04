# RabbitHole Client Experience — Design Spec

> The multi-server RabbitHole client across web, desktop, and mobile. Hotline/Haxial
> heritage, a unified warren layer above many places, centralized identity + presence,
> and content-addressed swarming transfers. Synthesized from a 7-agent design study
> (see `workflows/scripts/design-rabbithole-apps-*.js`), grounded in the shipped
> `crates/ui-web` SPA and the real protocol/swarm primitives.

## 1. Concept

**RabbitHole is a warren, not a browser tab.** It takes the Hotline/Haxial soul — *a
server is a place you go*, with its own name, banner, agreement, town-square chat,
communal file library, and live who-list — and does the one thing the classic clients
never could: it holds **many places at once** under a single constant frame that is
unmistakably **you**.

One-line thesis: *A native multi-server BBS client where each server is still a place you
visit, but your identity, your friends, and your downloads live once — above all of them
— and every download swarms.*

## 2. Two-layer architecture

- **The PLACE layer** — the inside of each burrow: Lobby, Boards, Files, Users, Radio,
  Art, Admin. These are the shipped `ui-web` surfaces almost unchanged, each tinted with
  that server's published accent theme (`ServerOverlay`, §9.11). Every burrow still feels
  like somewhere you arrived.
- **The WARREN layer** — a persistent shell *above* all places carrying the three
  non-negotiables:
  - **one identity** — portable Ed25519; one presence you set once and fan to every server;
  - **one People view** — your friends coalesced across every burrow they're on;
  - **one Transfers manager** — content-addressed, multi-source, swarming: the same file
    pulling chunks simultaneously from several servers *and* peer clients, each 16 KiB Bao
    block verified.

The heritage and the vision cohere because **content-addressing (blake3) dissolves "the
server" as the unit of a download**, **portable identity dissolves "the server" as the
unit of you**, and **the persistent rail lets N places feel like one app**. We keep
everything Hotline made you love and drop what single-server obscurity forced on it:
per-server fragmentation, the leaked transaction/Tasks log, numeric icon-ID identity,
closed KDX crypto, the blocking connect modal.

## 3. Heritage — adopt / reinterpret / drop

| Hotline/Haxial element | Move | Reinterpretation |
|---|---|---|
| Server-as-place | **Adopt soul** | Each burrow keeps its name/banner/agreement/theme, but is a *tile in a left rail* swapped into the main pane — never a separate window. |
| Tracker + bookmarks | **Adopt + merge** | One "Looking Glass": *Your burrows* (saved/connected) over *Discover* (live tracker rows; `servers.rs::browse()` ranking). |
| News / message boards | **Reinterpret** | News = the server's non-modal welcome/front-page (MOTD + featured); Boards = the shipped threaded discussion; per-board unread rolls up cross-server. |
| Dense file browser | **Reinterpret** | A dense columnar table (name · size · kind · uploaded-by · date · **sources**), type-to-filter, drag-drop upload, Get-Info detail rail — not a card gallery. |
| Public chat + PMs | **Re-separate by scope** | Chat stays server-local (the place's town square + rooms); **DMs become identity-scoped** and live in the unified layer, following a friend across burrows. |
| Big iconic toolbar | **Reinterpret** | A clean per-server segmented control (Lobby · Boards · Files · Users · Radio · Art · Admin) with unread pips; ⌘K keyword teleport on top; native bottom tabs on mobile. |
| User list w/ status | **Adopt + aggregate** | Per-server Users rail *plus* a unified People panel aggregating buddies across all connected burrows. |
| Connect flow | **Reinterpret** | Authenticate once to your identity; "joining a burrow" is one tap; a non-modal welcome sheet slides in the banner/MOTD/agreement while other servers stay live. |

## 4. Information architecture

Two coordinate systems joined by **one persistent left rail (desktop) / one bottom tab
bar + drill-down (mobile)**.

- **Warren layer (unified, brand-ember):** Home · People · Transfers · You + Looking Glass
  + the server switcher.
- **Place layer (per-server, accent-tinted):** the existing `Nav` becomes a per-server
  segmented section switcher.

**Rail shape language encodes the split:** unified functions render as **circles**
(`--rh-radius-full`); server places render as **rounded-square burrow-holes** (squircles)
tinted with each server's accent. Active tile = brand-ember pill on the left edge; unread
= brand dot; mentions = numeric badge; per-tile connection health = the shipped `rh-dot`.

### Resolved conflicts

| Conflict | Resolution |
|---|---|
| Server switcher | **Persistent left rail** (5–15 connected burrows is normal); mobile collapses it to a Servers tab + switcher sheet. |
| DM scope | **Identity-scoped, unified "Messages"** — one thread with a friend follows them across burrows. |
| Launch landing | **Home** is the post-connect landing; switching *to* a server restores that place's last sub-route. |
| News vs Boards | Fold News into the welcome sheet + a pinned board; place-nav stays 7 sections. |
| People vs Directory | **Both** — People (my friends across all servers) ≠ per-server Directory (this server's roster). |
| Session map key | Key by normalized dial endpoint (`ServerId`); carry the Ed25519 fingerprint as a trust-pin attribute, not the key. |
| Status scope | **Global by default** (one control fans `PresenceSet` to all); per-server "Appear offline here" override. |

### Screen inventory

**Warren layer:** `/home` (cross-server activity feed + jump-back-in) · `/messages`
(unified E2EE DM inbox) · `/people` (aggregated presence, verified-identity de-dup) ·
`/transfers` (multi-source manager) · `/you` (identity, personas, global status, per-server
accounts, security, theme) · `/explore` (Looking Glass).

**Place layer:** `/s/:server` (welcome) · `/s/:server/{lobby,boards,files,users,radio,art,admin}`.

**Overlays:** ⌘K palette (cross-server + keyword teleport) · Add-server sheet · Toasts ·
Downloads dock.

### Desktop layout

```
┌────┬──────────────────────────────────────────────┬───────────────┐
│RAIL│  glassy header (tinted to focused accent)      │  PEOPLE (all) │  ← warren (brand ember)
│ ◎  │  ◍ The Warren  ● Online · 214 here  ♪ Rhubarb  │  ◉ alice  W·B │
│Home│  ─────────────────────────────────────  ⌘K ☾ │  ● bob   Briar│
│ ☺  │  Lobby [Boards] Files Users Radio Art          │  ◐ cara (away)│
│Ppl │  ─────────────────────────────────────────────│───────────────│
│ ⤓² │  BOARD TREE          │  THREAD                 │  USERS (this  │  ← place (server accent)
│Xfer│   rabbit.general      │  Re: ANSI aspect ratios │   server)     │
├────┤   rabbit.art    3 new │   alice · 2h  "9px VGA…"│  ● alice      │
│◍ W▐│   rabbit.swarm        │   bob   · 1h  "SAUCE…" │  ● bob   idle │
│◍ B●│                       │                        │               │
│ ⊕  │                       │                        │               │
├────┴──────────────────────────────────────────────┴───────────────┤
│ ▾ Downloads  ‖ moon.iso  ▓▓▓▓▓░░ 62%  3 peers+2 srv  ✓ verified  ↓8.4│  ← unified dock
└─────────────────────────────────────────────────────────────────────┘
```

### Mobile layout

```
┌───────────────────────────┐        ┌───────────────────────────┐
│ ● Online   Home       ◐   │  tap   │ ‹ Servers  The Warren   ⋮  │
│───────────────────────────│Servers │───────────────────────────│
│ ◉ alice → you  (Warren·DM)│  ───▶  │[Lobby][Boards][Files][♥][♪]│ ← per-server segmented
│ ✉ 3 new · rabbit.art       │        │ alice  has the blake3?     │
│ ⤓ done: burrow-set.zip     │        │ you    pulling from 3 srv  │
│   (2 servers + 4 peers)    │        │ [ message…            ▶ ]  │
│───────────────────────────│        │───────────────────────────│
│ ⌂    ◍     ☺     ⤓     ◐  │        │ ⌂    ◍     ☺     ⤓     ◐  │ ← bottom tabs, safe-area
│Home Servrs Ppl  Xfer  You │        │Home Servrs Ppl  Xfer  You │
└───────────────────────────┘        └───────────────────────────┘
```

## 5. Multi-server state model

The load-bearing refactor. Single-session `AppState` (`one ws`, `state: UiState`, `files`,
`is_admin`, `server_theme`) becomes:

```
WarrenState {
    sessions: IndexMap<ServerId, Session>,   // ServerId = normalized dial endpoint
    focused:  Option<ServerId>,
    identity: LocalIdentity,                 // portable Ed25519
    presence: GlobalStatus,                  // fanned to every session
    people:   PeopleIndex,                   // aggregated, verified-key de-duped
    transfers: TransfersManager,             // content-keyed, above all sessions
}
Session { conn: WsClient, ui: UiState, files: FilesState, conn_state: ConnState,
          is_admin: bool, server_theme: Option<ServerOverlay> }
```

`WsClient` is already `Clone`/Rc-backed with per-instance keepalive + reconnect — it was
**accidentally multi-instance-safe**, so N live sessions need no transport change. Views
change one line (`app.state` → `app.focused().ui`); the ~15 transport sinks re-bind to
`sessions[&id].ui.apply()`. Ship this *before* any unified feature, behind the existing
single-server look with just one focused session.

## 6. Identity + presence

- **One RabbitHole identity** (portable Ed25519; fingerprint = handle), many per-server
  **persona displays**. People keys on the identity key; the persona is display only.
- **Global status by default**: a single control (Online / Away / Cheshire-invisible /
  Custom) loops `PresenceSet` across all sessions; a newly-joined burrow inherits the
  current status on connect; per-server "Appear offline here" is an explicit override.
- **Aggregated People**: union the who-lists + directory across sessions, **de-duped by
  verified identity key only** (never by `screen_name` — two humans can both be "rabbit").
  Until a row is key-verified it stays separate.

**Proto deltas required** (additive, `#[non_exhaustive]`-safe): add `Option<PublicKey>` to
`presence::UserSummary` and `directory::ProfileCard`; upgrade the SPA roster from
`who: Vec<String>` to `who: Vec<UserSummary>` (stop flattening in `frame_to_who`) so
presence state / role / current-surface badges and the People↔Directory join have the data.
Add the one missing send verb: `WsClient::set_presence()`.

## 7. Transfers + swarming

**A transfer is keyed by CONTENT, never by server.** The unit is a blake3 `ContentId`
(`Blob(BlobId)` for one file, `Manifest([u8;32])` for a fileset) — never a per-server
`node_id`. The moment you ask for a file, the manager collapses every place that
byte-identical content exists (this server's blob store, other connected servers'
catalogs, and swarm peers) into ONE transfer with many **Sources**.

```
TransfersManager { transfers: Vec<Transfer>, seeding: SeedSummary, down_bps, up_bps }
Transfer  { content_id: ContentId, name, kind: File|Fileset, total, done,
            verified_chunks: BitVec, status, priority, sources: Vec<Source>, dest }
Source    { id, label, kind: Server|Peer|Relay, endpoint, cert_fp,
            bps, chunks_served, state: Pulling|Idle|Retired }
```

`kind/endpoint/cert_fp` come straight off `SourceInfo`; `chunks_served`/`bps` off
`FetchReport.per_source`. A `Server` source is a `SourceList` with `server_has=true`; a
`Peer` source is a `SourceInfo` with `Some(endpoint)`. **The source roster + chunk map are
literally `FetchReport.per_source` + `verified_chunks` — no new engine work, only event
plumbing** (`SourcesFound` / `SourceProgress` / `ChunkVerified`). Engine constants surfaced
in the UI: 1 MiB unit, 16 KiB Bao block, 4 MiB request cap.

**Placement (same component, different chrome):** desktop = a persistent bottom download
*shelf* (collapsed one-line summary → expands to the panel); web = a right-rail drawer
from the ⌘K palette / a header badge; mobile = a full-screen tab, swipe-to-pause/cancel.

**Backend seam** — a `TransferBackend` trait: `WebBackend` (RHP-over-WS, multi-*server*
concurrency) ships now; `NativeBackend` (core `fetch_swarm_resumable` + `PeerServer`) adds
true peer swarming in the Tauri wave. Browsers can't speak raw QUIC (PLAN §5.1), so
web-client *seeding* + peer chunks await the WebRTC data-channel gateway (deferred, flagged).

## 8. Visual design language

"Hotline/Haxial heritage × modern × ultra-clean × native," built on the shipped
Clean/Retro/HighContrast pack tokens + the CSS-drawn concentric **burrow-hole** mark,
evolved with **one decisive new idea: a hard warm/cool split.**

- **Warm brand ember** (`--rh-brand`: `#FF8A3D` dark / `#E86A1F` light) owns the unified
  shell — the rail, aggregate presence, Transfers, the home mark — signalling *"you,
  everywhere."*
- **Cool accent** (`--rh-accent`, indigo/blue) owns whatever place you're standing inside,
  retinted per burrow by its theme overlay.

**Type:** Space Grotesk for display/wordmark/server names (`-0.02em`) over the **native
system UI font** for all body (SF on macOS, Segoe on Windows), with JetBrains Mono
(tabular, slashed zero) for the technical layer — blake3 hashes, sizes, ports, chunk
counts, `rabbit://` links.

**Density:** Hotline-grade dense columnar lists (28–32px rows, hairline separators),
auto-relaxing to 44px touch targets on `pointer: coarse` via a `--rh-row-h` /
`[data-density=dense]` token.

**Signature element — the burrow-hole:** brand mark at rest; a live aggregate-presence
ring whose bands light per connected server; a single ~450ms concentric ripple when a
server connects — the only ambient motion beyond progress, all gated by
`prefers-reduced-motion`.

### Evolved Clean tokens (added to `PackTokens.shared`)

```
--rh-brand:  dark #FF8A3D | light #E86A1F      /* burrow ember, constant identity */
--rh-brand-ring: color-mix(in srgb, var(--rh-brand) 22%, transparent)
--rh-font-display: "Space Grotesk", var(--rh-font-sans)
--rh-font-mono:    "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, monospace
--rh-row-h: 2rem   --rh-row-h-dense: 1.75rem   --rh-rail-w: 3.25rem   --rh-tile: 2.75rem
--rh-status-online:#3FBF7F  --rh-status-idle:#E8B84B  --rh-status-dnd:#F2555A
```

Clean-dark colour deltas: `--rh-bg #12141A`, `--rh-surface #1B1E26`, `--rh-surface-2
#242833` (new elevated); `--rh-accent #6C9CFF` kept. Retro/HighContrast collapse
`--rh-brand` into their accent (single-voice by design). Spacing/radii/type-scale steps
unchanged; the server tile is `radius-lg` on a 2.75rem tile (a squircle).

## 9. Platform-native adaptations

Same shared `ui-web` core (per PLAN §2, the wasm SPA is reused verbatim in Tauri v2); only
the shell + chrome fork, behind two orthogonal axes:

- **`FormFactor`** (matchMedia → compact / medium / expanded) mounts the Chrome.
- **`PlatformCaps` trait** (WebCaps / TauriDesktop / TauriMobile) binds native hooks behind
  one interface with graceful web no-ops (the same MockClient↔WsClient seam discipline).

**Desktop (Tauri):** native menubar, a tray/menubar presence + quick-status popover,
keyboard-first (⌘K + a full shortcut map as native menu accelerators), native drag-drop to
upload/download, dock/taskbar badges, `rabbit://` deep links, background transfers, window
vibrancy + inset traffic lights, optional per-server window tear-out (⌘N).

**Mobile (Tauri v2):** a bottom tab bar (Home · Servers · People · Transfers · You),
per-server drill-down, gestures + long-press context menus, the share sheet (share a
`rabbit://` or a file *into* the app), background transfers + background radio audio,
push/local notifications, safe-area/one-hand ergonomics.

**Web:** the current responsive SPA; multi-*server* download concurrency; peer swarming +
seeding await the WebRTC gateway.

## 10. Build phasing

One tested slice per commit, per the repo cadence. **R** = reuse, **N** = new, **D** =
depends on an unbuilt primitive.

- **Wave A — WarrenState refactor** (foundation, no visible change). `AppState` →
  `sessions: IndexMap<ServerId, Session>` + `focused`; re-bind sinks; per-session
  `ConnState`. R: all reducers/views/transport. N: `ServerId`, `Session`, accessors.
- **Wave B — Rail + multi-session shell.** Left Burrow Rail; reshape `Nav` into the
  per-server segmented place-nav; non-modal welcome sheet; persist `rh.servers` for
  reconnect-on-launch. R: `browse()`, Login, theme tokens, burrow-hole mark. *First visibly
  multi-server release.*
- **Wave C — Identity + presence + People.** `LocalIdentity` bootstrap + per-server
  `KeyEnroll`; the You hub + status menu; `set_presence()`; the People view with
  `merge_people` (pure, host-tested); `who` → `Vec<UserSummary>`. D: verified de-dup needs
  the `Option<PublicKey>` proto delta.
- **Wave D — Unified Transfers + swarm made visible.** Hoist `TransfersManager` (content-
  keyed); FindSources fan-out; `SourcesFound`/`SourceProgress`/`ChunkVerified` events; the
  source roster + chunk map; `TransferBackend` trait (Web now, Native later); the bottom
  dock; `rabbit://` resolver. R: `crates/swarm`, `crates/blobs`, transfer/queue primitives.
  D: native peer swarming = Tauri in-process core; web seeding = WebRTC gateway.
- **Wave E — Unified Messages + cross-server unread + ⌘K evolution.** Hoist E2EE DMs to
  identity scope; two-tier palette (unified + per-server sections). D: identity-keyed DM
  routing.
- **Wave F — Native chrome (Tauri desktop + mobile).** `PlatformCaps` impls: badges,
  notifications, tray + quick-status, `rabbit://` resolver, drag-drop, share sheet,
  background transfers/audio, native menus, mobile bottom-tabs + gestures + entitlements.
  R: 100% of the SPA core. N: thin Rust glue in `apps/gui`. D: mobile push relay; native
  swarm core.

**Critical-path dependencies:** (1) the `Option<PublicKey>` proto delta blocks verified
People de-dup; (2) the Tauri in-process Rust core blocks true peer swarming + background
transfers; (3) the WebRTC gateway blocks web-client seeding; (4) identity-keyed DM routing
blocks unified Messages. Everything else composes from shipped primitives.

## 11. Open questions for the owner

- **Auto-reconnect-all vs. lazy-wake on launch** — cold-starting 8 saved burrows = 8
  handshakes + who-lists (painful on cellular). Lean: lazy-connect on first focus on mobile,
  eager on desktop.
- **Identity portability across the user's own devices** — ship a QR/recovery-phrase export
  in v1, or per-device key + server-side persona linkage?
- **Personas** — same persona to all servers, or per-burrow faces under one key? Lean:
  identity coalesces (People), persona is display.
- **Peer-connection privacy default** — server-relay-only (only the server sees your IP)
  with direct-peer opt-in, or the reverse?
- **Web download-only vs. WebRTC swarming now** — launch web as pull-from-servers-only
  (making peer swarming a desktop advantage), or invest in the WebRTC gateway day one?
- **Multi-window tear-out scope** — per-server windows only (Hotline-faithful), or also
  per-board/per-DM detachable panes? Lean: per-server first.
