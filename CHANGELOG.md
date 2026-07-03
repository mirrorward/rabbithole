# Changelog

All notable changes to the RabbitHole workspace are recorded here. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Pre-1.0 wire note.** While the version is below `1.0`, the RabbitHole
> Protocol (RHP) wire format may still change between minor releases — the
> families are still settling. Such changes are always *deliberate*: the
> `proto` crate carries a checked-in wire registry
> (`crates/proto/src/registry.rs`) and a golden guard test
> (`crates/proto/tests/registry.rs`) that fails on any *accidental*
> renumber, rename, collision, or add/remove of a message type. See
> [`docs/protocol/versioning.md`](docs/protocol/versioning.md) for the
> compatibility policy.

One bullet per release, summarizing the wave/feature that shipped it. Versions
below `0.3.0` (Waves 0–5) landed before per-release version tagging and are
grouped as _Foundations_ at the end; those waves are where the `proto` crate's
message families were first defined.

## [Unreleased]

### Added
- `proto`: curated wire registry (`REGISTRY`) enumerating every
  `(family, message_type, name)` triple (174 message types across 9 active
  families), plus guard tests asserting no `(family, message_type)`
  collisions, a completeness count (`EXPECTED`), and a golden snapshot
  (`tests/wire-registry.golden`) that must be re-blessed for any intentional
  wire change.
- Protocol compatibility policy at [`docs/protocol/versioning.md`](docs/protocol/versioning.md)
  (additive-only rules, version negotiation vs. capability flags,
  `#[non_exhaustive]` discipline, and the registry-golden tripwire).
- This changelog.

## [0.63.0] - 2026-07-03
- Docs refresh: flagship config, Reticulum (RNS) decision record, and marking
  the reticulum-rs spike decision landed.

## [0.62.0] - 2026-07-03
- Wave 10: live gateway/feed stats over the admin wire (`ADMIN`
  `GatewayStatsRequest`/`GatewayStatsReply`).

## [0.61.0] - 2026-07-03
- Waves 9 + 11: TUI radio handoff and the Looking Glass server browser.

## [0.60.0] - 2026-07-03
- Wave 14: `rabbit://` links carry RNS destination hashes.

## [0.59.0] - 2026-07-03
- Wave 13: chat moderation hardening — room mute and slow-mode (`CHAT`
  `RoomMute`/`RoomUnmute`/`RoomSlowMode` and matching pushes).

## [0.58.0] - 2026-07-03
- Wave 14: RNS transport-layer model — links, hashes, announce cache.

## [0.57.0] - 2026-07-03
- Wave 6: ZMODEM transfers on the telnet BBS — `zget`/`zput` with resume.

## [0.56.0] - 2026-07-03
- Wave 13: accessibility pass over the web SPA.

## [0.55.0] - 2026-07-03
- Wave 8: server theme-bundle application with safety rails.

## [0.54.0] - 2026-07-03
- Wave 10: syndication admin panel — gateway matrix, feeds, poll editor.

## [0.53.0] - 2026-07-03
- Wave 10: TLS for the NNTP surfaces — NNTPS, STARTTLS, AUTHINFO gate.

## [0.52.0] - 2026-07-03
- Wave 8: installable PWA — service worker, manifest, icons.

## [0.51.0] - 2026-07-03
- Wave 6: telnet is a real BBS — welcome, boards, chat, DMs, `/go`.

## [0.50.0] - 2026-07-03
- Wave 13: warren-stampede load harness.

## [0.49.1] - 2026-07-03
### Fixed
- CI-red finger refusal: read the query before refusing.

## [0.49.0] - 2026-07-03
- Wave 7: HTXF upload with fork-offset resume.

## [0.48.1] - 2026-07-03
- `burrow`: `--http`/`--http-addr`/`--web-root` CLI flags for the web SPA.

## [0.48.0] - 2026-07-03
- Wave 11: web radio player — now-playing, Icecast audio, prefs.

## [0.47.0] - 2026-07-03
- Wave 13: backups — consistent snapshot and offline restore.

## [0.46.0] - 2026-07-03
- Wave 8: web theme editor — admin panel, live preview, token IO.

## [0.45.0] - 2026-07-02
- Wave 10: QWK offline mail — packet build and REP ingest in `burrow`.

## [0.44.1] - 2026-07-02
- CI: RUSTSEC security-audit gate (`cargo-audit`).

## [0.44.0] - 2026-07-02
- Wave 8: embedded HTTP server — SPA shell and `/files` handoff.

## [0.43.1] - 2026-07-02
- Wave 9: 3-server federation testnet end-to-end.

## [0.43.0] - 2026-07-02
- Wave 13: moderation suite — reports, quarantine, hash-deny (`ADMIN`
  report/quarantine/deny-hash message types).

## [0.42.1] - 2026-07-02
- Docs: protocol spec lockstep audit, federation, legacy-surfaces.

## [0.42.0] - 2026-07-02
- Wave 6 polish: RBAC→SL projection, min-role gates, telnet files.

## [0.41.0] - 2026-07-02
- Wave 9: Looking Glass directory index (health/uptime).

## [0.40.0] - 2026-07-02
- Wave 13: token-bucket rate limiting across every surface.

## [0.39.0] - 2026-07-02
- Wave 9: `persona@server` attestation and key continuity.

## [0.38.0] - 2026-07-02
- Waves 10 + 11: NNTP peer-feed service and radio updinfo wiring.

## [0.37.0] - 2026-07-02
- Wave 9: Looking Glass signed descriptors, categories, gossip.

## [0.36.0] - 2026-07-02
- Wave 7: Hotline private chat rooms and IM quoting/auto-response.

## [0.35.0] - 2026-07-02
- Wave 11: Icecast mid-stream metadata codec (updinfo + metaint reader).

## [0.34.1] - 2026-07-02
### Fixed
- HTXF flake root cause: re-roll the port pair on ephemeral binds.

## [0.34.0] - 2026-07-02
- Wave 6: door-games runner wired into the telnet shell.

## [0.33.0] - 2026-07-02
- Wave 8: Retro and High Contrast theme packs with shareable tokens.

## [0.32.1] - 2026-07-02
### Fixed
- Persistent Windows HTXF flake: drain to peer FIN before close.

## [0.32.0] - 2026-07-02
- Waves 10 + 11: RSS/Atom feed ingest service and radio notice bridge.

## [0.31.0] - 2026-07-02
- Wave 6: doors session-runner core (registry, nodes, FSM, bridge).

## [0.30.0] - 2026-07-02
- Wave 7: Hotline account-admin plus kick/ban wired into `burrow`.

## [0.29.0] - 2026-07-02
- Wave 11: TUI radio now-playing status bar and station panel.

## [0.28.0] - 2026-07-02
- Wave 9: signed-catalog sync and federated search over S2S.

## [0.27.0] - 2026-07-02
- Wave 7: Hotline account-admin codec (access mask, user flags).

## [0.26.0] - 2026-07-02
- Wave 11: DJ live source ingest, library playlists, now-playing.

## [0.25.0] - 2026-07-02
- Wave 10: NNTP peering codec (IHAVE/streaming/NEWNEWS/wildmat).

## [0.24.0] - 2026-07-02
- Wave 10: syndication feed→board ingestion and poll scheduling.

## [0.23.0] - 2026-07-02
- Wave 9: S2S federation peering (QUIC + admin approval) into `burrow`.

## [0.22.0] - 2026-07-02
- Wave 8: web admin routes, WS reconnect/backoff, FILE dispatch.

## [0.21.0] - 2026-07-02
- Wave 14: LXMF message layer on the reticulum crate.

## [0.20.1] - 2026-07-02
### Fixed
- Flaky Windows HTXF download: graceful socket shutdown.

## [0.20.0] - 2026-07-02
- Wave 10: FidoNet gateway and binkp mailer wired into `burrow`.

## [0.19.0] - 2026-07-02
- Wave 8: web SPA files UI, ANSI art canvas, light/dark theme.

## [0.18.0] - 2026-07-02
- Wave 9: federation signed catalogs and cross-server search.

## [0.17.0] - 2026-07-02
- Wave 7.4: Hotline threaded news → boards and file transactions/HTXF.

## [0.16.0] - 2026-07-02
- Wave 10: FTN tosser/scanner/ARCmail/AreaFix/nodelist services.

## [0.15.0] - 2026-07-02
- Wave 10: QWK REP ingest, dedupe, and packet export builder.

## [0.14.0] - 2026-07-02
- Wave 7.3: Hotline-compatible server surface wired into `burrow`.

## [0.13.0] - 2026-07-02
- Wave 8.3: browser WebSocket transport for the web SPA.

## [0.12.1] - 2026-07-02
### Fixed
- CI: Icecast clippy 1.96 lint and flaky macOS tracker TTL tests.

## [0.12.0] - 2026-07-02
- Icecast radio delivery wired into `burrow`; e2ee group messaging.

## [0.11.0] - 2026-07-02
- Wave 8.2: web SPA — boards, DMs, directory views, and nav.

## [0.10.0] - 2026-07-02
- NNTP gateway wired into `burrow`; radio service and art renderers landed.

## [0.9.0] - 2026-07-02
- Landed binkp and Icecast/ICY codec crates.

## [0.8.0] - 2026-07-02
- Landed the Leptos web SPA foundation.

## [0.7.0] - 2026-07-02
- Landed federation and Reticulum data-model crates.

## [0.6.0] - 2026-07-02
- Landed e2ee and doors crates plus packaging tooling.

## [0.5.0] - 2026-07-02
- Landed six agent-built codec/app crates.

## [0.4.0] - 2026-07-02
- Landed the screen and Hotline-codec crates.

## [0.3.0] - 2026-07-02
- Landed the Wave 6 and Wave 10 legacy-surface crates (telnet, finger, art,
  screen, syndication).

## Foundations (0.1.0 – 0.2.x, Waves 0–5, pre-tagged)

These waves predate per-release version tagging. They built the core
workspace and every `proto` message family enumerated in the wire registry.

- Wave 5: The Warren swarm — manifests and `rabbit://` links, coordinator
  (advertise / find-sources / TTL soft state), server-signed capability
  tokens, Bao-verified peer wire, multi-source work-stealing scheduler, and
  resumable swarm fetches. (`SWARM` family)
- Wave 4: file libraries — areas, folder tree, metadata, drop boxes, aliases,
  search, small blobs, and a resumable transfer engine with per-account
  quotas and a persistent client queue. (`FILE` family)
- Wave 3: message bases (signed post events, threading, read pointers), the
  Wishing Well request board, and an offline board cache. (`BOARD` and
  `WISHING_WELL` families)
- Wave 2: personas, profiles, directory, registration, TOTP 2FA, the admin
  family, presence states / buddy lists / blocks, DMs, rooms
  (public/private/invite/kick), and the welcome composer + signed theme
  bundle + keyword teleport. (`SESSION`, `PRESENCE`, `CHAT`, `DM`, `ADMIN`
  families)
- Wave 1: the first vertical slice — auth, permissions, presence, lobby chat,
  control channel, and CLI.
- Wave 0: workspace foundations — `proto`, `identity`, `net`, `blobs`,
  stores, and the event bus.

[Unreleased]: https://github.com/mirrorward/rabbithole/compare/v0.63.0...HEAD
