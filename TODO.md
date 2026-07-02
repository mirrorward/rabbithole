# RabbitHole — Implementation Tracker

> Distilled from `PLAN.md` (read that first — it has the specs, rationale, and
> dependency graph). Check items off as they land. Waves must respect the
> dependency edges shown in PLAN.md §15. ⛔ = do not start until PLAN.md is
> reviewed and approved by the project owner.

**Status: Waves 0–5 complete (swarm coordinator, Bao-verified peer wire, multi-source scheduler, resumable fetches, blob cache policy; NAT traversal + WebRTC deferred to their proper environments). Wave 6 (legacy surfaces) underway — the `art` (CP437/ANSI/SAUCE), `legacy-telnet` (negotiation + login shell), and `legacy-finger` (RFC 1288) crates have landed as standalone libraries; Wave 10 gained an RSS/Atom parsing crate. Next: wire these surfaces into `burrow`.**

> W4.2: transfers are resumable + integrity-checked, folder-pipelined, and
> move bytes over dedicated QUIC bulk streams (off the control channel) with
> a WebSocket ranged-chunk fallback — one protocol, one verification path.

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

- [x] Server daemon: QUIC + WS listeners; graceful shutdown
- [x] Hello/HelloAck; auth: password, guest (toggleable), token resume; keepalive; reconnect w/ replay cursor
- [x] Roles (guest→superuser) + classes + u64 capability bitmask
- [x] ACL evaluator: nearest-ancestor, deny-wins, hide-vs-deny, cached effective masks (property-tested)
- [x] Presence registry actor; who's-online query + pushes
- [x] Public chat (lobby), server agreement gate, MOTD
- [x] `rabbit` CLI: login, who, chat, JSON output mode
- [x] `burrow ctl`: config get/set, account create, local admin socket
- [x] Config system: TOML + env overrides + hot-reload-where-safe
- [x] `tracing` + audit-log skeleton

## Wave 2 — Community layer
*Depends on: W1*

- [x] Registration gating (open/invite/email), TOTP enrollment, key enrollment
- [x] Class admin (create/edit/assign; live inheritance)
- [x] Personas: multiple per account (cap configurable), switcher
- [x] Profiles (location/interests/quote/free text/.plan), avatars + **banner images** (blob-backed, size-capped)
- [x] Member directory + search; "locate online" (privacy-gated)
- [x] Buddy lists: server-stored, groups, permit/deny; presence states (online/away+msg/idle/invisible) + pub/sub pushes
- [x] Chat: multiple public rooms w/ categories + topics; ad-hoc rooms; private rooms w/ invite/join/leave; subjects; room kick/ban (mute + slow-mode deferred to W13 moderation hardening)
- [x] DMs: threads, offline queueing, quoting, away auto-response, receipts (privacy-gated)
- [x] DM attachments (server-config max size, content-addressed)
- [x] Notifications: protocol pushes + client-side sounds (optional, tasteful)
- [x] Welcome screen composer (widgets: MOTD, unread, who, featured, ticker)
- [x] Server theme bundle v1 (signed, content-addressed: logo, banner art, accent tokens, icon set) in welcome bundle
- [x] Keyword registry + `/go` fuzzy teleport
- [x] TUI client v1: login, chat, who, DMs (`screen` crate begun); light/dark palettes
- [x] Server TUI v1: connection monitor, config, accounts
- [x] Remote admin protocol family live (capability-gated + audited)

## Wave 3 — Message bases & offline
*Depends on: W2*

- [x] Board hierarchy (categories/bundles/boards, dotted slugs) + per-board ACLs + moderators
- [x] Posts as signed blake3 events (author + server sigs) — federation-ready
- [x] Threading (parent/root), markdown/plain/ANSI bodies, edit-as-event, tombstones
- [x] Retention/auto-archive policy (KDX-style)
- [x] Per-user read pointers; unread counts surfaced (welcome, keyword bar)
- [x] Client offline store: board cache, batch delta download (content-id merge), offline read/reply, outbox sync on reconnect
- [x] Request system ("wishing well"): CRUD, voting, claim → fulfilled linkage, requester notifications
- [x] Shared dupe/seen subsystem (time-windowed, multi-ID-form) + tests
- [x] CLI/TUI board reading + posting; `rabbit sync`/`read`/`reply`/`wish`

## Wave 4 — File libraries & transfers
*Depends on: W3*

- [x] Areas + folder trees; metadata: icons (retro set + custom), comments, uploader, dates, download counters, ratings
- [x] Aliases; **drop boxes** (write-only, privilege-gated viewing); hide-vs-deny folder ACLs
- [x] Background file indexer → instant search (projection-backed substring search; FTS5 later)
- [x] Transfer engine: ticketed resumable transfer with dedicated QUIC bulk streams (WS ranged-chunk fallback), whole-file blake3 verify; per-chunk Bao verify shares the W5 swarm crate
- [x] Folder transfers (pipelined, no per-item lockstep) — one FolderManifest round trip, then independent per-file transfers (`rabbit file getdir`)
- [x] Quotas + rate policy — per-account upload storage quota (`upload_quota_bytes`) at ticket + inline upload; per-account transfer **concurrency cap** (`max_concurrent_transfers`, refused with `RateLimited`) with session-scoped ticket cleanup; per-transfer **bandwidth cap** (`transfer_rate_bytes_per_sec`) on both download paths (per-class overrides deferred)
- [x] Persistent client transfer queue: priorities + auto-resume across restarts (store-client `transfer_queue`); queue driver (`rabbithole_core::queue::drain`, bandwidth cap via `Client::set_rate_limit`) + CLI `rabbit queue get/put/list/run/pause/resume/prio/rm/clear`
- [x] CLI file browse + transfer UX (`rabbit file areas/ls/put/get/search/rate/…`); TUI + big-file transfers with W4.2

## Wave 5 — Swarm ("the warren")
*Depends on: W4*

- [x] Spike: iroh vs quinn+custom for hole punching/relay → **decision: quinn + custom** (stack is already quinn end-to-end with fingerprint pinning and the coordinator handles discovery; iroh is the documented fallback if NAT traversal underdelivers) — see docs/protocol/swarm.md
- [x] Manifest format (per-file blake3 roots, 1 MiB chunks) + `rabbit://` links — `rabbithole-swarm` `Manifest`/`ManifestFile` (content-addressed id = blake3 over canonical postcard bytes; path-sorted for determinism) and `RabbitLink` (`rabbit://host[:port]/{files,manifest,blob}/…`, percent-encoded, root-pinned). CBOR interop deferred to a later slice.
- [x] `AdvertiseFiles` (list-without-upload): metadata catalog, permission scopes, TTL soft state + re-announce — SWARM family (6) types 1-5, `SwarmCatalog` (TTL'd soft state, per-account cap `swarm_adverts_max`, session-scoped cleanup), gated by `SWARM_ADVERTISE` on the `swarm` resource; `rabbit swarm share/find/unshare`
- [~] Coordinator: FindSources (scope-gated, reports origin-server fallback + source count as list-level rarity) done; per-chunk rarity annotation arrives with the peer wire/scheduler
- [x] Server-signed capability tokens; peer-side verification — `rabbithole-swarm::CapToken` (ed25519 over `rhp-swarm-cap-v1`-separated claim {root, fetcher, expiry}), issued via `SourceTicketRequest` (FILE_DOWNLOAD-gated, 10 min TTL), verified against the hello-time server key; `PeerContact` cards (observed-IP + declared port + pinned cert fp) join `SourceList` entries
- [x] Peer wire over QUIC: request/response with Bao-verified streams — `rabbithole-swarm::peer` (PeerServer on the rabbithole-net QUIC stack, one bi-stream per `PeerRequest{token, root, offset, len≤4MiB}`, responses are Bao streams at 16 KiB chunk groups verified block-by-block against the root; SeedStore precomputes outboards and refuses root-mismatched files; `rabbit swarm share` seeds / `rabbit swarm fetch` does find→ticket→fetch). Have-bitfields/Cancel arrive with partial seeding (`.rhstate`)
- [x] Multi-source scheduler: per-source speed assignment via work-stealing (1 MiB units, faster peers naturally pull more), endgame duplication of in-flight stragglers, failed sources retire and their units migrate — `rabbithole-swarm::scheduler::fetch_swarm`; `rabbit swarm fetch` uses all reachable sources. Rarest-first is a cross-file ordering (coordinator source counts) and lands with manifest-set fetching
- [x] Server chunk cache policies (none/LRU/mirror) — blob store `evict_unreferenced_over(max_bytes)` (oldest-first, referenced library content never evicted) driven by a periodic `maintenance` task on `swarm_cache_max_bytes` (0 = unlimited/mirror; positive = LRU cap; policy notes in config)
- [ ] NAT: hole punching + server relay fallback; optional UPnP/NAT-PMP; "relay-only" privacy mode — deferred (needs a real multi-NAT network to build/test meaningfully); `fetch_range` now has a bounded `PEER_CONNECT_TIMEOUT` so unreachable peers fail fast and the scheduler routes around them
- [x] `.rhstate` persistence — resumable swarm fetches (`fetch_swarm_resumable`: unit bitfield persisted atomically per unit, foreign/stale state ignored, whole-file hash check on completion catches lying partials, state removed on success; `rabbit swarm fetch` resumes). Bao-outboard persistence + partial *seeding* (serving what you have) is a later refinement
- [ ] WebRTC gateway for browser peers — deferred to W8 (browser transport lands with the wasm SPA)
- [x] Multi-peer simulation test harness (lossy links, corruption injection) — `crates/swarm/tests/sim.rs`: 10-peer swarm, flaky/dead/wrong-fingerprint majority, and inline corrupting/truncating malicious peers proving an adversary can waste time but never land a wrong byte; surfaced+fixed a real `QuicListener::accept` bug (one bad handshake no longer kills the listener for all peers)

## Wave 6 — Telnet BBS + finger + art pipeline
*Depends on: W2, W3 (W4 optional for file menus)*

- [x] `art` crate: CP437↔Unicode tables, ANSI/SGR + cursor parser, iCE colors, renderers to terminal/plain/**PNG**/**HTML** — `rabbithole-art` (`cp437`/`ansi`/`sauce`/`render`/`raster`/`font`, `AnsiParser`→`Canvas` of `Cell`; PNG thumbnails via embedded 8x16 CP437 VGA font + 16-color palette; `<pre>`+`<span>` HTML output; fuzz-tolerant; 73 tests). Live ANSImation streaming deferred
- [x] SAUCE reader/writer (128-byte record + COMNT) — `rabbithole-art::sauce`, tolerant reads / strict writes, iCE-color tflag, roundtrip tests
- [x] `screen` crate: CP437/ANSI + UTF-8 text surface — `rabbithole-screen` (direct `Cell` buffer over `rabbithole-art`, `ScreenMode::{Utf8,Cp437Ansi}`, box/menu drawing with reverse-video selection, SGR-coalesced `flush`; no ratatui dep). Socket wiring into the telnet shell is the next slice
- [x] Telnet codec: IAC state machine, ECHO/SGA/NAWS(resize)/TTYPE, 0xFF doubling, loop-safe negotiation — `rabbithole-legacy-telnet` (`proto`/`negotiate`/`stream`, sans-IO parser + RFC 1143-style state machine, line IO w/ password mode, CP437/UTF-8 seam); BINARY option TBD with the art integration
- [~] BBS surface: telnet login shell + MAIN MENU stub (`legacy-telnet::shell`, pluggable `TelnetAuth`); full welcome art / boards / chat / DMs / keyword nav still to wire into burrow
- [ ] File browse + HTTP-link handoff
- [~] Zmodem transfers over telnet — codec landed: `rabbithole-legacy-zmodem` (CRC16/32, ZDLE, hex/bin16/bin32 headers, data subpackets, ZFILE, sans-IO Sender/Receiver state machine, fuzz-tolerant; 61 tests). Telnet-stream wiring + resume + client-interop testing (SyncTERM/NetRunner/qodem) is the next slice
- [~] Door games: DOOR32.SYS (+DOOR.SYS/DORINFO1.DEF) dropfiles — landed: `rabbithole-legacy-doors` (`DoorContext` + faithful writers/readers for all three formats, never-panic parsers; 13 tests). telnet/PTY bridge (no fd inheritance) + door menu + per-door ACLs + time limits are the runner slice
- [ ] Legacy security-level projection (RBAC → 0–255 SL + flags) for dropfiles
- [x] finger (79): empty = who list; user = profile+presence+.plan; /W; **forwarding refused**; output caps — `rabbithole-legacy-finger` (RFC 1288, pluggable `FingerDirectory`, control-char sanitized so a hostile .plan can't inject escapes, 32 KiB cap); per-persona opt-out + burrow wiring TBD
- [ ] Legacy-surface class restrictions + per-listener toggles
- [~] Wire the telnet/finger listeners into `burrow` — done: opt-in `telnet_enabled`/`finger_enabled` config, `burrow::legacy` adapters (`TelnetAuth`→`AuthService`, `FingerDirectory`→presence+`PersonasRepo`, invisible users hidden), listeners spawned at startup, e2e-tested. Art rendering in the telnet menus + full BBS navigation still to wire

## Wave 7 — Hotline compatibility
*Depends on: W2, W3, W4*

- [x] TRTP/HOTL handshake; 20-byte transaction codec; TLV fields w/ 16/32-bit size-dependent ints — `rabbithole-legacy-hotline` (handshake/transaction/field/reassembly/constants, big-endian, minimal-width int helpers, fragment reassembler with 16 MiB ceiling, fuzz-tolerant; 29 tests). Networking + login flow are later slices
- [ ] Login (255−b obfuscation) + opt-in legacy credential; agreement/banner; Agreed/SetClientUserInfo flows; pipelined-early-request tolerance
- [ ] User list + icon-ID mapping; NotifyChange/DeleteUser pushes; UserFlags
- [ ] Public chat, private chat rooms (112–120), IM (108) w/ quoting + auto-response
- [ ] Threaded news transactions (370–411) mapped to boards; flat-news (101/102) projection
- [ ] File transactions (200–213); HTXF channel (port+1); FFO encode/decode (INFO/DATA forks, MWIN); fork-offset resume; folder lockstep
- [ ] Account admin transactions (348–355); access-mask projection (big-endian bit order, tested); kick/ban (110/111)
- [x] `apps/tracker`: native registry + HTRK (5498) listing + UDP (5499) heartbeat registration — `looking-glass` daemon (TTL registry, HTRK UDP registration + TCP listing codecs with packet diagrams, native `LIST` status port; 18 tests). Signed descriptors / tracker gossip land with W9
- [ ] Compat rig: archived Hotline clients + mobius-driven integration tests

## Wave 8 — Web & desktop GUI
*Depends on: W2–W4 (W5 for transfer UI); parallel with W6/W7*

- [~] `ui-web` Leptos SPA — `rabbithole-ui-web` (Leptos 0.6 CSR: router + Login, Lobby chat + WhoList, **Boards** tree/thread views, **DMs** conversation+compose, **member Directory** with live search, nav bar, ThemeToggle, StatusBar; DOM-free `UiState`/`apply` reducer + `UiClient`/`MockClient` seam; wasm-check in CI; 25 host tests). Real wasm WebSocket transport + profiles/keyword bar are the next slices
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
- [~] Board flood-fill: per-peer/per-board subscriptions, ihave/pull, Bloom+store seen-set — model landed: `rabbithole-federation` (`Subscription`/`IHave`/`PullRequest`/`PushEvents`/`FedEvent`, classic `BloomFilter` with optimal m,k; 25 tests). S2S transport + store wiring is the service slice
- [~] Tombstone/redact propagation (server-sovereign application) — `rabbithole-federation::redaction` (signed `Redaction`, verify authenticates; apply left to receiver). Propagation wiring pending
- [~] Ingest defense: dual-sig verify, per-peer rate limits, reputation, auto-defederation, allow/deny lists — `rabbithole-federation::policy` (per-peer token-bucket `RateLimiter`, allow/deny `PeerPolicy`) + descriptor verify done; reputation/auto-defederation pending
- [ ] Cross-server identity attestation (`persona@server`, key continuity)
- [ ] Cross-server file search: signed catalogs, pull fan-out, blake3 dedupe; swarm federated sources live
- [~] `.well-known/rabbithole/server` (signed descriptor) — `rabbithole-federation::PeerDescriptor` (ed25519-signed under `rhp-fed-descriptor-v1`, verify + tamper tests) done; HTTP surface pending
- [ ] Looking Glass tracker: signed descriptors, heartbeats, categories, tracker-to-tracker gossip
- [ ] Directory index (health/uptime, verifiable-not-authoritative); client server-browser UI
- [ ] Deploy flagship public **Looking Glass** (project-run, domain TBD); pre-configure in clients (user-removable)
- [ ] 3-server CI testnet: partition/rejoin, dupe-storm tests

## Wave 10 — Syndication (NNTP → FTN → QWK)
*Depends on: W3 (dupe subsystem); W9 helpful*

**NNTP**
- [x] Reader server: CAPABILITIES, GROUP/LISTGROUP, ARTICLE/HEAD/BODY/STAT, NEXT/LAST, POST, OVER/XOVER + OVERVIEW.FMT, LIST; dot-stuffing both ways — codec (`rabbithole-legacy-nntp`, 51 tests) + a live gateway wired into burrow (`apps/server/src/nntp.rs`, config-gated `nntp_enabled`/`nntp_addr`, AUTHINFO→AuthService, POST→BoardService; e2e-tested). NEWNEWS/IHAVE peering deferred to the federation slice
- [ ] AUTHINFO USER/PASS on TLS only (563/STARTTLS)
- [x] Group↔board mapping (identity slug↔group); per-group monotonic article numbers; permanent Message-IDs (`<hex(blake3 event id)@origin>`); References threading; overview rendered from post metadata — in `apps/server/src/nntp.rs`
- [ ] Peering: IHAVE/NEWNEWS with external peers; Message-ID dedupe via shared subsystem

**FidoNet**
- [x] PKT codec: type-2+ w/ type-2 fallback (capability word), packed messages, golden-file round-trip tests — `rabbithole-legacy-ftn` (bounds-checked LE reader, 5D addresses, CP437; 31 tests incl. 2000-iter fuzz)
- [x] Kludges: INTL/FMPT/TOPT/MSGID/REPLY/PID/TID; AREA:; Origin; SEEN-BY + PATH maintenance — `rabbithole-legacy-ftn::kludge` (canonical re-serialization, raw-CP437 text). Tosser/scanner/binkp/AreaFix/nodelist below are the service layer
- [ ] Tosser + scanner services; ARCmail bundles (day-coded names + collision handling); BSO outbound
- [~] binkp mailer (FTS-1026, port 24554) — codec + FSM landed: `rabbithole-legacy-binkp` (2-byte block framing, M_NUL..M_SKIP, CRAM-MD5 auth verified against RFC 2202 vectors, sans-IO originating+answering session state machine; 42 tests). TCP mailer service is the wiring slice
- [ ] AreaFix (netmail commands: +/−/％LIST/％QUERY)
- [ ] Nodelist + NODEDIFF parsing (CRC-16); echomail↔boards; netmail↔DM gateway; CP437 lossless round-trip

**RSS/Atom (inbound web syndication)**
- [x] Feed parsing: lenient hand-rolled XML pull tokenizer, RSS 2.0 + Atom 1.0 → `Feed`/`FeedItem`, manual RFC 2822 + RFC 3339 date parsing, HTML-to-text (tag strip + entity decode + char-boundary cap), blake3 dedup ids — `rabbithole-legacy-syndication` (blake3-only dep, 39 tests). Network fetch + board ingestion wiring is the next slice.

**QWK/QWKE**
- [~] Packer: MESSAGES.DAT 128-byte blocks (0xE3 EOL, conf# @124–125), CONTROL.DAT, per-conf NDX with **MBF float** encode, DOOR.ID (QWKE advertised), bulletins — codec landed: `rabbithole-legacy-qwk` (MBF float verified against known vectors, Latin-1 fields; 40 tests incl. fuzz). ZIP bundling is the documented seam
- [x] QWKE long To/From/Subject kludges (both directions) — `rabbithole-legacy-qwk::qwke`
- [ ] REP ingest: validate, dedupe, post as signed events
- [ ] Delivery: CLI/web export, telnet surface, scheduled per-user packets; read pointers shared with offline mode
- [ ] Syndication admin UI: per-board network mappings, feed monitor, dupe stats

## Wave 11 — Radio
*Depends on: W1, W4 (W8 for UI polish)*

- [x] Station/mount model (multiple stations, per-server toggle) — `rabbithole-audio` (PCM frames, mixer, `Station` fan-out, jitter buffer, VU meter; 21 tests) + `rabbithole-radio::StationRegistry` (create/remove/list, per-station enable toggle, listener accounting; 23 tests)
- [x] Playlist engine: rotation (Sequential/seeded-Shuffle/RepeatOne), **vote queue** + requests, `StationController` tying playlist→audio Station with now-playing/metadata — `rabbithole-radio`. Library-from-file-areas source wiring is the server slice
- [ ] DJ live source (Icecast SOURCE/PUT + Basic auth) — works with butt/ices
- [ ] Encode pipelines: Opus/Ogg primary + MP3 legacy mount
- [~] Delivery: native QUIC uni-stream; HTTP streaming; ICY mounts w/ exact icy-metaint math (8192, len×16, 0x00 when unchanged) — ICY codec landed: `rabbithole-legacy-icecast` (SOURCE/PUT source auth, listener headers, exact `IcyMetaInterleaver` metaint math with fuzz-verified boundary correctness; 43 tests). Server delivery wiring + QUIC/HTTP transports pending
- [ ] Listener counts in presence; now-playing surfaced (presence line, TUI status, telnet, web)
- [ ] Client players: GUI/web, TUI handoff, per-user enable + volume/ducking settings

## Wave 12 — Mobile & distribution
*Depends on: W8 (W11 for background audio)*

- [ ] Tauri iOS/iPadOS + Android builds; mobile plugin glue: notifications, background audio + audio session, share sheet
- [ ] Transport resilience on mobile (QUIC connection migration, WS fallback)
- [ ] App Store (TestFlight) + Play (.aab) packaging, signing, privacy manifests, entitlements
- [~] `dist` release automation (CLI/TUI/server): archives, installers, Homebrew — `.github/workflows/release.yml` (tag-triggered cross-platform binary archives + checksums + GitHub Release) and `scripts/release.sh` landed; installers/Homebrew tap pending
- [x] Docker images (multi-stage → slim) + docker-compose; systemd unit; install docs — `Dockerfile` + `.dockerignore` + `docker-compose.yml` + hardened `contrib/burrow.service` + `docs/deployment.md` (accurate to real bins/ports/env vars)
- [ ] Versioned protocol docs published (docs site)

## Wave 13 — Hardening & 1.0
*Depends on: all*

- [~] E2EE DMs: X25519 + Double Ratchet, ChaCha20-Poly1305, sealed sender — crypto core landed: `rabbithole-e2ee` (X25519, Signal Double Ratchet w/ bounded skipped-key store + transactional decrypt, X3DH-lite, sealed sender; RNG-generic, wasm-friendly; 23 tests). Per-thread opt-in wiring into the DM proto + key-backup UX pending
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
- [~] RNS interop foundation: identity (X25519+Ed25519), destination/name hashes, packet + announce codecs, ECDH+AEAD token — `rabbithole-reticulum` (spec-faithful hashes, never-panic decoders, documented cipher/field divergences; 40 tests). The live RNS transport adapter (Burrow as a Reticulum destination, constrained RHP profile) builds on this
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
