# RabbitHole — Implementation Tracker

> Distilled from `PLAN.md` (read that first — it has the specs, rationale, and
> dependency graph). Check items off as they land. Waves must respect the
> dependency edges shown in PLAN.md §15. ⛔ = do not start until PLAN.md is
> reviewed and approved by the project owner.

**Status: APPROVED 2026-07-01 — Wave 0 complete; Wave 1 is next.**

---

## Wave 0 — Foundations
*Depends on: — (first wave)*

- [x] Cargo workspace scaffold: all `crates/*` + `apps/*` stubs compile
- [x] CI: fmt + clippy + test matrix (Linux/macOS/Windows) + wasm build check
- [ ] Licensing (⛔ owner still deciding — see PLAN §16); README ✅, CONTRIBUTING ✅, rustfmt/clippy config ✅
- [x] `proto`: RHP frame (version/kind/family/type/id/error/payload), postcard framing, error model
- [x] `proto`: version negotiation + capability flags; `#[non_exhaustive]` discipline
- [x] `docs/protocol/` skeleton — spec is a deliverable
- [x] `identity`: Ed25519 keygen/sign/verify; Argon2id (m=64MiB,t=3,p=1, PHC, rehash-on-login)
- [x] `identity`: opaque session tokens; TOTP (RFC 6238) + hashed recovery codes
- [x] `net`: `Transport` trait; quinn (QUIC/TLS1.3) impl; tokio-tungstenite (WS) impl
- [x] `net`: rustls setup; ACME (`rustls-acme`) + self-signed w/ fingerprint pinning
- [x] Storage: `Repository` traits; sqlx/SQLite (WAL) + rusqlite skeletons; migration harness
- [x] Content-addressed blob store (blake3 pathing, refcount GC)
- [x] `core` / `server-core` skeletons: Command/Event API, event bus (tokio broadcast)

## Wave 1 — Vertical slice: a server you can talk to
*Depends on: W0*

- [ ] Server daemon: QUIC + WS listeners; graceful shutdown
- [ ] Hello/HelloAck; auth: password, guest (toggleable), token resume; keepalive; reconnect w/ replay cursor
- [ ] Roles (guest→superuser) + classes + u64 capability bitmask
- [ ] ACL evaluator: nearest-ancestor, deny-wins, hide-vs-deny, cached effective masks (property-tested)
- [ ] Presence registry actor; who's-online query + pushes
- [ ] Public chat (lobby), server agreement gate, MOTD
- [ ] `rabbit` CLI: login, who, chat, JSON output mode
- [ ] `burrow ctl`: config get/set, account create, local admin socket
- [ ] Config system: TOML + env overrides + hot-reload-where-safe
- [ ] `tracing` + audit-log skeleton

## Wave 2 — Community layer
*Depends on: W1*

- [ ] Registration gating (open/invite/email), TOTP enrollment, key enrollment
- [ ] Class admin (create/edit/assign; live inheritance)
- [ ] Personas: multiple per account (cap configurable), switcher
- [ ] Profiles (location/interests/quote/free text/.plan), avatars + **banner images** (blob-backed, size-capped)
- [ ] Member directory + search; "locate online" (privacy-gated)
- [ ] Buddy lists: server-stored, groups, permit/deny; presence states (online/away+msg/idle/invisible) + pub/sub pushes
- [ ] Chat: multiple public rooms w/ categories + topics; ad-hoc rooms; private rooms w/ invite/decline/join/leave; subjects; room moderation (kick/mute/ban/slow-mode)
- [ ] DMs: threads, offline queueing, quoting, away auto-response, receipts (privacy-gated)
- [ ] DM attachments (server-config max size, content-addressed)
- [ ] Notifications: protocol pushes + client-side sounds (optional, tasteful)
- [ ] Welcome screen composer (widgets: MOTD, unread, who, featured, ticker)
- [ ] Server theme bundle v1 (signed, content-addressed: logo, banner art, accent tokens, icon set) in welcome bundle
- [ ] Keyword registry + `/go` fuzzy teleport
- [ ] TUI client v1: login, chat, who, DMs (`screen` crate begun); light/dark palettes
- [ ] Server TUI v1: connection monitor, config, accounts
- [ ] Remote admin protocol family live (capability-gated + audited)

## Wave 3 — Message bases & offline
*Depends on: W2*

- [ ] Board hierarchy (categories/bundles/boards, dotted slugs) + per-board ACLs + moderators
- [ ] Posts as signed blake3 events (author + server sigs) — federation-ready
- [ ] Threading (parent/root), markdown/plain/ANSI bodies, edit-as-event, tombstones
- [ ] Retention/auto-archive policy (KDX-style)
- [ ] Per-user read pointers; unread counts surfaced (welcome, keyword bar)
- [ ] Client offline store: board subscribe, batch delta download, offline read/reply, outbox sync on reconnect
- [ ] Request system ("wishing well"): CRUD, voting, claim → fulfilled linkage, notifications
- [ ] Shared dupe/seen subsystem (time-windowed, multi-ID-form) + tests
- [ ] CLI/TUI board reading + posting

## Wave 4 — File libraries & transfers
*Depends on: W3*

- [ ] Areas + folder trees; metadata: icons (retro set + custom), comments, uploader, dates, download counters, ratings
- [ ] Aliases; **drop boxes** (write-only, privilege-gated viewing); hide-vs-deny folder ACLs
- [ ] Background file indexer → instant search
- [ ] Transfer engine: dedicated QUIC streams, Bao verified streaming, byte-level resume
- [ ] Folder transfers (pipelined, no per-item lockstep)
- [ ] Quotas + per-class rate policy
- [ ] Persistent client transfer queue: priorities, bandwidth caps, schedules, auto-resume across restarts
- [ ] CLI/TUI file browse + transfer UX

## Wave 5 — Swarm ("the warren")
*Depends on: W4*

- [ ] Spike: iroh vs quinn+custom for hole punching/relay → decision
- [ ] Manifest format (canonical CBOR, per-file blake3 roots, 1 MiB chunks) + `rabbit://` links
- [ ] `AdvertiseFiles` (list-without-upload): metadata catalog, permission scopes, TTL soft state + re-announce
- [ ] Coordinator: FindSources (scope-gated), announce, rarity annotation
- [ ] Server-signed capability tokens; peer-side verification
- [ ] Peer wire over QUIC: Hello/Have/RequestRange/Cancel; Bao-verified responses
- [ ] Multi-source scheduler: rarest-first, per-source speed assignment, endgame mode
- [ ] Server chunk cache policies (none/LRU/mirror)
- [ ] NAT: hole punching + server relay fallback; optional UPnP/NAT-PMP; "relay-only" privacy mode
- [ ] `.rhstate` persistence (bitfield + Bao outboard), lazy re-verify, partial seeding
- [ ] WebRTC gateway for browser peers (may land with W8)
- [ ] Multi-peer simulation test harness (lossy links, corruption injection)

## Wave 6 — Telnet BBS + finger + art pipeline
*Depends on: W2, W3 (W4 optional for file menus)*

- [ ] `art` crate: CP437↔Unicode tables, ANSI/SGR + cursor parser, iCE colors, ANSImation, renderer to terminal/HTML-canvas/PNG-thumbs
- [ ] SAUCE reader/writer (128-byte record + COMNT)
- [ ] `screen` crate: ratatui → socket backend (CP437/ANSI mode + UTF-8 mode)
- [ ] Telnet codec: IAC state machine, ECHO/SGA/BINARY/NAWS(resize)/TTYPE, 0xFF doubling, loop-safe negotiation
- [ ] BBS surface: login, welcome art, who, boards (read/post), chat, DMs, keyword nav
- [ ] File browse + HTTP-link handoff
- [ ] Zmodem transfers over telnet: download, then upload; ZRPOS resume; tested against SyncTERM/NetRunner/qodem
- [ ] Door games: DOOR32.SYS (+DOOR.SYS/DORINFO1.DEF) dropfiles; telnet/PTY bridge (no fd inheritance); door menu + per-door ACLs + time limits
- [ ] Legacy security-level projection (RBAC → 0–255 SL + flags) for dropfiles
- [ ] finger (79): empty = who list; user = profile+presence+.plan; /W; **forwarding refused**; output caps; per-persona opt-out
- [ ] Legacy-surface class restrictions + per-listener toggles

## Wave 7 — Hotline compatibility
*Depends on: W2, W3, W4*

- [ ] TRTP/HOTL handshake; 20-byte transaction codec; TLV fields w/ 16/32-bit size-dependent ints
- [ ] Login (255−b obfuscation) + opt-in legacy credential; agreement/banner; Agreed/SetClientUserInfo flows; pipelined-early-request tolerance
- [ ] User list + icon-ID mapping; NotifyChange/DeleteUser pushes; UserFlags
- [ ] Public chat, private chat rooms (112–120), IM (108) w/ quoting + auto-response
- [ ] Threaded news transactions (370–411) mapped to boards; flat-news (101/102) projection
- [ ] File transactions (200–213); HTXF channel (port+1); FFO encode/decode (INFO/DATA forks, MWIN); fork-offset resume; folder lockstep
- [ ] Account admin transactions (348–355); access-mask projection (big-endian bit order, tested); kick/ban (110/111)
- [ ] `apps/tracker`: native registry + HTRK (5498) listing + heartbeat registration
- [ ] Compat rig: archived Hotline clients + mobius-driven integration tests

## Wave 8 — Web & desktop GUI
*Depends on: W2–W4 (W5 for transfer UI); parallel with W6/W7*

- [ ] `ui-web` Leptos SPA: auth, welcome, rooms, DMs, boards, member directory, profiles, keyword bar
- [ ] Files UI: browse, upload/download (WS + fetch), transfer queue
- [ ] Art rendering (canvas)
- [ ] Design tokens; **light/dark mode** (OS-follow + manual override) across all rich clients
- [ ] Theme packs: Clean (default), Retro (CP437/scanlines/ANSI palette), High Contrast; shareable token files
- [ ] Server theme bundle application (accents, icons, art, sounds) w/ safety rails (structured tokens only, contrast minimums, user cap/disable)
- [ ] Theme editor panel in web admin (upload assets, accents, live light/dark/retro preview)
- [ ] Embedded web client served by server; installable PWA
- [ ] Web admin routes: config, accounts/classes, moderation, monitors (federation/radio panels as they land)
- [ ] Tauri v2 desktop: core in-process, QUIC transport, native notifications, tray/menubar presence, `rabbit://` deep links, auto-update
- [ ] Server native GUI wrapper (systray + bundled daemon)
- [ ] Playwright E2E suite

## Wave 9 — Federation & discovery
*Depends on: W3 (+W4/W5 for catalogs/swarm; W8 admin UI helpful)*

- [ ] S2S QUIC channel; server key exchange; peering handshake + admin approval
- [ ] Board flood-fill: per-peer/per-board subscriptions, ihave/pull, Bloom+store seen-set
- [ ] Tombstone/redact propagation (server-sovereign application)
- [ ] Ingest defense: dual-sig verify, per-peer rate limits, reputation, auto-defederation, allow/deny lists
- [ ] Cross-server identity attestation (`persona@server`, key continuity)
- [ ] Cross-server file search: signed catalogs, pull fan-out, blake3 dedupe; swarm federated sources live
- [ ] `.well-known/rabbithole/server` (signed descriptor)
- [ ] Looking Glass tracker: signed descriptors, heartbeats, categories, tracker-to-tracker gossip
- [ ] Directory index (health/uptime, verifiable-not-authoritative); client server-browser UI
- [ ] Deploy flagship public **Looking Glass** (project-run, domain TBD); pre-configure in clients (user-removable)
- [ ] 3-server CI testnet: partition/rejoin, dupe-storm tests

## Wave 10 — Syndication (NNTP → FTN → QWK)
*Depends on: W3 (dupe subsystem); W9 helpful*

**NNTP**
- [ ] Reader server: CAPABILITIES, GROUP/LISTGROUP, ARTICLE/HEAD/BODY/STAT, NEXT/LAST, POST, OVER/XOVER + OVERVIEW.FMT, LIST, NEWNEWS; dot-stuffing both ways
- [ ] AUTHINFO USER/PASS on TLS only (563/STARTTLS)
- [ ] Group↔board mapping; monotonic article numbers; permanent Message-IDs; References threading; **overview cache computed on post**
- [ ] Peering: IHAVE/NEWNEWS with external peers; Message-ID dedupe via shared subsystem

**FidoNet**
- [ ] PKT codec: type-2+ w/ type-2 fallback (capability word), packed messages, golden-file round-trip tests
- [ ] Kludges: INTL/FMPT/TOPT/MSGID/REPLY/PID/TID; AREA:; Origin; SEEN-BY + PATH maintenance
- [ ] Tosser + scanner services; ARCmail bundles (day-coded names + collision handling); BSO outbound
- [ ] binkp mailer (FTS-1026, port 24554)
- [ ] AreaFix (netmail commands: +/−/％LIST/％QUERY)
- [ ] Nodelist + NODEDIFF parsing (CRC-16); echomail↔boards; netmail↔DM gateway; CP437 lossless round-trip

**QWK/QWKE**
- [ ] Packer: MESSAGES.DAT 128-byte blocks (0xE3 EOL, conf# @124–125), CONTROL.DAT, per-conf NDX with **MBF float** encode, DOOR.ID (QWKE advertised), bulletins; ZIP bundling
- [ ] QWKE long To/From/Subject kludges (both directions)
- [ ] REP ingest: validate, dedupe, post as signed events
- [ ] Delivery: CLI/web export, telnet surface, scheduled per-user packets; read pointers shared with offline mode
- [ ] Syndication admin UI: per-board network mappings, feed monitor, dupe stats

## Wave 11 — Radio
*Depends on: W1, W4 (W8 for UI polish)*

- [ ] Station/mount model (multiple stations, per-server toggle)
- [ ] Playlist engine: library from file areas, rotation, **vote queue**, requests
- [ ] DJ live source (Icecast SOURCE/PUT + Basic auth) — works with butt/ices
- [ ] Encode pipelines: Opus/Ogg primary + MP3 legacy mount
- [ ] Delivery: native QUIC uni-stream; HTTP streaming; ICY mounts w/ exact icy-metaint math (8192, len×16, 0x00 when unchanged); no ICY splicing into Ogg
- [ ] Listener counts in presence; now-playing surfaced (presence line, TUI status, telnet, web)
- [ ] Client players: GUI/web, TUI handoff, per-user enable + volume/ducking settings

## Wave 12 — Mobile & distribution
*Depends on: W8 (W11 for background audio)*

- [ ] Tauri iOS/iPadOS + Android builds; mobile plugin glue: notifications, background audio + audio session, share sheet
- [ ] Transport resilience on mobile (QUIC connection migration, WS fallback)
- [ ] App Store (TestFlight) + Play (.aab) packaging, signing, privacy manifests, entitlements
- [ ] `dist` release automation (CLI/TUI/server): archives, installers, Homebrew
- [ ] Docker images (cargo-chef → slim) + docker-compose (server + tracker); systemd unit; install docs
- [ ] Versioned protocol docs published (docs site)

## Wave 13 — Hardening & 1.0
*Depends on: all*

- [ ] E2EE DMs: X25519 + Double Ratchet (vodozemac), ChaCha20-Poly1305, sealed sender; opt-in per thread; key backup UX
- [ ] Moderation suite: report queues, quarantine-for-review, shared blocklists + blake3 hash-deny lists, moderation audit UI
- [ ] Rate limiting everywhere (governor buckets per IP/account/endpoint-class); mCaptcha option; invite trees
- [ ] Fuzzing coverage goals (all codecs); RUSTSEC audit gate in CI; security review checklist/pen-test pass
- [ ] Load harness (target: 10k concurrent sessions/server) + performance pass
- [ ] Accessibility pass (web/GUI); i18n scaffolding
- [ ] Backups: snapshot + restore tested; migration guides
- [ ] 1.0: docs site complete, flagship sample server config, launch

## Wave 14 — Reticulum & off-grid mesh (post-1.0 / 1.1)
*Depends on: W3, W9, W13*

- [ ] Spike: `reticulum-rs` maturity vs Python RNS gateway sidecar → decision
- [ ] RNS transport adapter: Burrow as a Reticulum destination; constrained RHP profile (control + text; no bulk over LoRa-class links)
- [ ] LXMF bridge: DMs ↔ LXMF (delay-tolerant, NomadNet-compatible); boards ↔ LXMF propagation nodes (shared dupe subsystem)
- [ ] Delay-tolerant Tunnels (S2S flood-fill) over RNS with bandwidth-aware batching
- [ ] rabbit links w/ RNS destination hashes; Looking Glass entries may advertise RNS destinations
- [ ] LoRa/packet-radio deployment docs (power/bandwidth budgets)

---

## Continuous tracks (every wave)

- [ ] Protocol spec kept in lockstep with implementation (`docs/protocol/`)
- [ ] Golden-file + fuzz tests accompany every codec
- [ ] CHANGELOG + semver discipline on `proto`
- [ ] Mobile cross-compile smoke in CI from W0 (front-load NDK pain)
