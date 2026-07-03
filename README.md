# RabbitHole

> Down the rabbit hole: a modern Rust revival of the golden age of online
> communities — Hotline, KDX, BBSes, and AOL — one server, many doors.

A **Burrow** (server) hosts chat rooms, message boards, file libraries, direct
messages, a swarm file-distribution layer (**the Warren**), pirate radio, and a
request board (**the Wishing Well**) — reachable from native apps, terminals,
browsers, telnet BBS clients, newsreaders, offline mail readers, and (yes) real
classic Hotline clients. Servers federate over **Tunnels** and are discoverable
through **Looking Glass** directories.

**Status: 0.61.0 — waves 0–14 largely landed.** The native server and its
surfaces are feature-complete for a public flagship; remaining work is 1.0
hardening (E2EE wiring, cross-server flood-fill, GUI/mobile shells) and the
post-1.0 Reticulum mesh. See [`TODO.md`](TODO.md) for the exact per-wave state.

- [`PLAN.md`](PLAN.md) — the full-vision phased roadmap
- [`TODO.md`](TODO.md) — the authoritative wave-by-wave tracker
- [`docs/protocol/`](docs/protocol/) — the RabbitHole Protocol (RHP) spec
- [`docs/legacy-surfaces.md`](docs/legacy-surfaces.md) — the operator matrix for every legacy listener
- [`docs/research/`](docs/research/) — the research briefs behind the design
- [`examples/flagship-burrow.toml`](examples/flagship-burrow.toml) — a fully-commented sample config

## What works today

The **native RHP server** (`burrow`) is the heart of it: QUIC (primary) and
WebSocket (fallback) transports, accounts with Argon2id passwords, TOTP 2FA and
recovery codes, multiple personas per account, and an ACL evaluator (roles +
classes + capability bitmask, nearest-ancestor / deny-wins) governing every
surface.

| Subsystem | State at 0.61.0 |
|---|---|
| **Accounts & identity** | Ed25519 identity keys, Argon2id passwords, hashed session tokens + resume, TOTP + recovery codes, key enrollment, registration gating (open/invite/closed) |
| **Personas & presence** | Multiple personas per account, profiles/.plan/avatars+banners, buddy lists, presence states (away/idle/invisible), member directory + locate |
| **Chat** | Lobby + categorized public rooms, ad-hoc and private (invite-only) rooms, topics/subjects, kick/ban, **per-room mute + slow-mode** (W13) |
| **Direct messages** | Threads, offline queueing, quoting, away auto-response, privacy-gated receipts, content-addressed attachments |
| **Boards** | Category/bundle/board tree, posts as dual-signed blake3 events, threading, edit-as-event + tombstones, retention, read pointers; **offline sync** (cache, delta download, outbox) |
| **Wishing Well** | Request board: CRUD, voting, claim → fulfilled linkage, requester notifications |
| **File libraries** | Areas + folder trees, metadata/icons/ratings, drop boxes, aliases, background indexer + search, quotas + per-transfer bandwidth caps |
| **Transfers** | Ticketed **resumable** transfers on dedicated QUIC bulk streams (WS ranged-chunk fallback), whole-file + per-chunk **Bao** verification, folder pipelining, persistent client queue |
| **The Warren (swarm)** | Manifests + `rabbit://` links, advertise-without-upload catalogs, server-signed capability tokens, Bao-verified peer wire, multi-source work-stealing scheduler with `.rhstate` resume, LRU/mirror cache policies *(NAT hole-punching + WebRTC deferred)* |
| **Federation (Tunnels)** | S2S QUIC endpoint, nonce-bound Ed25519 handshake + admin approval, signed file catalogs with pull fan-out + blake3 dedupe **shipped**; flood-fill, redaction propagation, and `persona@server` attestation are model-only crates awaiting the S2S service wiring |
| **Moderation** | Report queues, quarantine-for-review, blake3 hash-deny lists, audit trail; enforced on native RHP file/board/DM paths |
| **Rate limiting** | Token buckets across six classes (conn/auth/msg/post/transfer/legacy), per-IP + per-account, live-tunable, on by default |
| **Backups** | Consistent snapshot (`ctl backup`), verify, and **offline** `burrow restore` (stop → restore → start), migration notes in module docs |
| **E2EE** | Crypto core complete (`rabbithole-e2ee`: Double Ratchet, X3DH-lite, sealed sender, sender-key groups); per-thread DM/room wiring is pending *(model-only)* |

**Clients & tooling**

- **SPA web client** (`crates/ui-web`, Leptos): files, boards, chat, DMs,
  directory, admin console, theme editor (live light/dark/retro preview), and a
  radio player — an installable **PWA** with an offline app-shell, served by
  `burrow --http`.
- **TUI clients** (`rabbit-tui`): chat/who/DMs, a radio now-playing panel with
  external-player handoff, and a **Looking Glass** server browser
  (INDEX/CATEGORIES/HEALTH with an uptime sparkline).
- **`rabbit` CLI**: login (password/guest, QUIC or WS), chat, boards, files,
  swarm, transfer queue, wishing well, `--json` mode.
- **`looking-glass` tracker**: signed self-certifying descriptors, UDP gossip
  anti-entropy, categories, and a verifiable (not authoritative) health INDEX.
- **`warren-stampede`** (`apps/loadgen`): a load harness driving real `burrow`
  sessions (idle/chat/mixed scenarios, p50/p95/p99, `--json`).

**Legacy surfaces** — every one **off by default**, gated by `*_enabled` and a
`*_min_role`, and rate-limited:

- **Telnet BBS** — CP437/ANSI art pipeline, welcome screen, boards/chat/DM
  shells, `/go` teleport, a **files** browser (HTTP-link handoff or **ZMODEM**
  in/out with resume), and **door games** (DOOR32.SYS/DOOR.SYS/DORINFO1.DEF).
- **Finger** (RFC 1288), **NNTP** (reader + peer-feed, NNTPS + STARTTLS),
  **Hotline** (private rooms, admin transactions, HTXF upload/download with
  fork-offset resume), **FidoNet/binkp** (tosser/scanner/AreaFix/nodelist),
  **RSS/Atom** inbound syndication, and **Icecast radio** (ICY delivery + DJ
  SOURCE ingest + `updinfo` metadata).

**Model-only / deferred** (present as tested crates, not yet wired to the wire):
`rabbithole-reticulum` (RNS/LXMF data model + crypto, **99 tests**, no transport
— see [`docs/research/reticulum-decision.md`](docs/research/reticulum-decision.md));
E2EE thread wiring; federation flood-fill/attestation; Tauri desktop + mobile
shells. QWK offline-mail delivery is wired to the telnet `[M]` menu and `ctl`,
not to a standalone listener.

## Surfaces & default ports

Native transports and rate limiting are the only things on out of the box;
**every legacy listener is off by default**. Listener addresses need a restart;
`*_min_role` gates apply live. Exact keys and semantics live in
[`docs/legacy-surfaces.md`](docs/legacy-surfaces.md).

| Surface | Default port | Enable key | Default |
|---|---|---|---|
| Native RHP over QUIC | 4653 (`quic_addr`) | (always on) | **on** |
| Native RHP over WebSocket | 4654 (`ws_addr`) | (always on) | **on** |
| S2S federation (Tunnels) | 4655 (`federation_addr`) | `federation_enabled` | off |
| Embedded HTTP + PWA | 8080 (`http_addr`) | `http_enabled` / `--http` | off |
| Telnet BBS (+ doors, files) | 2323 (`telnet_addr`) | `telnet_enabled` | off |
| Finger (RFC 1288) | 7979 (`finger_addr`) | `finger_enabled` | off |
| NNTP reader | 1119 (`nntp_addr`) | `nntp_enabled` | off |
| NNTPS reader (implicit TLS) | 563 (`nntp_tls_addr`) | `nntp_tls_enabled` | off |
| NNTP peer feed | 1120 (`nntp_feed_addr`) | `nntp_feed_enabled` | off |
| NNTP peer feed (implicit TLS) | 1563 (`nntp_feed_tls_addr`) | `nntp_feed_tls_enabled` | off |
| Hotline (+ HTXF on port+1) | 5500 / 5501 (`hotline_addr`) | `hotline_enabled` | off |
| FidoNet / binkp mailer | 24554 (`ftn_addr`) | `ftn_enabled` | off |
| Radio delivery (Icecast/ICY) | 8000 (`radio_addr`) | `radio_enabled` | off |
| Radio DJ source + `updinfo` | 8001 (`radio_source_addr`) | `radio_source_enabled` | off |
| RSS/Atom syndication | — (outbound fetcher) | `syndication_enabled` | off |
| QWK/QWKE offline mail | — (telnet `[M]` + `ctl`) | `qwk_enabled` | off |
| Looking Glass tracker | 5498 TCP / 5499 UDP + 4656 UDP gossip | (`looking-glass` daemon) | — |

## Try it

```console
$ cargo run -p burrow -- run
  INFO burrow: burrow is up quic=0.0.0.0:4653 ws=0.0.0.0:4654 fingerprint=<hex>

# Admin from another terminal (unix ctl socket, no network):
$ burrow ctl account-create alice wonderland user
$ burrow ctl config-set motd "Down the rabbit hole we go"
$ burrow ctl status

# Serve the web SPA + HTTP file links without touching burrow.toml
# (build the SPA once with `trunk build` in crates/ui-web):
$ burrow --http --web-root crates/ui-web/dist run
  INFO burrow: http listening http=0.0.0.0:8080

# Sign in and chat:
$ rabbit login ws://127.0.0.1:4654 --user alice --password wonderland
$ rabbit say "oh my ears and whiskers"
$ rabbit who
$ rabbit tail            # stream the lobby

# QUIC with certificate pinning (fingerprint from `burrow ctl status`):
$ rabbit login 127.0.0.1:4653 --fingerprint <hex> --server-name localhost --guest
```

Back up and restore (the server must be **stopped** before a restore):

```console
$ burrow ctl backup ./snapshots/2026-07-03      # live, consistent snapshot
$ burrow ctl backup-verify ./snapshots/2026-07-03
$ burrow restore ./snapshots/2026-07-03 --data-dir ./burrow-data   # offline
```

A fully-commented flagship configuration — legacy surfaces, federation, radio,
rate limits, and the HTTP/PWA surface all shown — lives in
[`examples/flagship-burrow.toml`](examples/flagship-burrow.toml).

## Development

```console
$ cargo test --workspace          # full suite
$ cargo clippy --workspace --all-targets
$ cargo fmt --all
$ cargo check -p rabbithole-proto -p rabbithole-core --target wasm32-unknown-unknown
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md). License: **TBD** (being decided —
see PLAN.md §16; until then, all rights reserved by the project owner).
