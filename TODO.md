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
- [x] File browse + HTTP-link handoff — telnet `files` sub-shell (`ls`/`cd`/`get`/paging) with RBAC identical to Hotline's browse (FILE_LIST + per-folder SEE, drop-box contents hidden without DROPBOX_VIEW); `get` mirrors FILE_DOWNLOAD + drop-box rules and prints `<files_http_base>/files/<area>/<percent-encoded-path>` (link minting only — the HTTP file server is the web slice; empty base → polite refusal). Download counting happens on actual fetch (web slice)
- [~] Zmodem transfers over telnet — codec landed: `rabbithole-legacy-zmodem` (CRC16/32, ZDLE, hex/bin16/bin32 headers, data subpackets, ZFILE, sans-IO Sender/Receiver state machine, fuzz-tolerant; 61 tests). Telnet-stream wiring + resume + client-interop testing (SyncTERM/NetRunner/qodem) is the next slice
- [~] Door games: DOOR32.SYS (+DOOR.SYS/DORINFO1.DEF) dropfiles — landed: `rabbithole-legacy-doors` (`DoorContext` + faithful writers/readers for all three formats, never-panic parsers) **plus the session-runner core**: `door` (`DoorDef` argv/dropfile/IoMode/NodeRange/daily-limit + `DoorRegistry` with TOML `[[doors]]` serde), `node` (`NodePool` + RAII `NodeLease`, lowest-free + range-scoped allocation, contention-tested), `session` (pure FSM Preparing→Running→Ended/TimedOut/Aborted, injected clocks, `prepare_dropfile` pinning the node), `bridge` (sans-IO CP437-clean pump w/ chunk-boundary-safe telnet IAC doubling + `BridgeStats`); 47 tests. **Runner wired into burrow**: `apps/server/src/doors.rs` (`DoorService` — boot-validated `[[doors]]` registry, shared NodePool, `%D/%F/%N/%H` token expansion + env, tokio process spawn, IAC-safe stdio pump, min(session-max, daily-limit) timeout→kill, RAII node release, `door-run`/`door-denied` audit) + a real telnet shell (`telnet.rs`: banner → AuthService login → menu with `doors`/`door <id>`), gated by `doors_enabled` (default off) + new `Caps::DOOR_RUN` (member+); 7 e2e tests. Deferred: DOOR32 comm-type-2 socket-handle inheritance, per-user daily accumulation, TOTP on telnet, TTYPE CP437 detection
- [x] Legacy security-level projection (RBAC → 0–255 SL + flags) for dropfiles — `server-core::security_level(&Subject)`: role bases Guest=10/User=30/Moderator=80/Admin=100/Superuser=255, twelve participation caps nudge ±2 within disjoint role bands (1–25/26–70/71–95/96–250) so cross-role monotonicity holds under any grant/revoke mask (tested adversarially); door dropfiles now carry the projection (hardcoded table deleted)
- [x] finger (79): empty = who list; user = profile+presence+.plan; /W; **forwarding refused**; output caps — `rabbithole-legacy-finger` (RFC 1288, pluggable `FingerDirectory`, control-char sanitized so a hostile .plan can't inject escapes, 32 KiB cap); per-persona opt-out + burrow wiring TBD
- [x] Legacy-surface class restrictions + per-listener toggles — `telnet_min_role`/`nntp_min_role`/`hotline_min_role`/`finger_min_role` (guest|user|moderator|admin, `member` alias, live-appliable, validated by `ctl config set`; default guest = prior behavior): telnet refuses pre-menu, Hotline names the requirement in the login error, NNTP answers 480 anonymous/481 authed-below-min, finger (anonymous protocol) refuses every query when min>guest; finger's accept loop moved into burrow with the per-IP conn budget for parity. 4 e2e + 8 unit tests
- [~] Wire the telnet/finger listeners into `burrow` — done: opt-in `telnet_enabled`/`finger_enabled` config, `burrow::legacy` adapters (`TelnetAuth`→`AuthService`, `FingerDirectory`→presence+`PersonasRepo`, invisible users hidden), listeners spawned at startup, e2e-tested. Art rendering in the telnet menus + full BBS navigation still to wire

## Wave 7 — Hotline compatibility
*Depends on: W2, W3, W4*

- [x] TRTP/HOTL handshake; 20-byte transaction codec; TLV fields w/ 16/32-bit size-dependent ints — `rabbithole-legacy-hotline` (handshake/transaction/field/reassembly/constants, big-endian, minimal-width int helpers, fragment reassembler with 16 MiB ceiling, fuzz-tolerant; 29 tests). Networking + login flow are later slices
- [x] Login (255−b obfuscation) + opt-in legacy credential; agreement/banner; Agreed/SetClientUserInfo flows; pipelined-early-request tolerance — `apps/server/src/hotline.rs` bridges the wire codec to the shared services: TRTP/HOTL handshake + OK, login (107) de-obfuscates creds and authenticates via `AuthService` (guest fallback when enabled), pushes agreement (109), dedicated non-cancellable reader + reassembler tolerate pipelined early requests; `hotline_enabled`/`hotline_addr` (5500) config, off by default; 3 e2e tests
- [x] User list + icon-ID mapping; NotifyChange/DeleteUser pushes; UserFlags — GetUserNameList (300) packs id/icon/flags/name from the presence snapshot (invisible users hidden); NotifyChange/DeleteUser (301/302) driven off the `EventBus`; SetClientUserInfo (304) re-publishes identity via `PresenceRegistry::rename`
- [x] Public chat, private chat rooms (112–120), IM (108) w/ quoting + auto-response — public chat (105→106) bridged to the shared `ChatService` lobby (Hotline/native/telnet share one room); private IM (108) routed via the per-server `Hub` (SERVER_MSG 104). **Private rooms landed**: a Hotline private chat IS a shared `ChatService` private room (Hub `chat_id↔room` projection): 112 create+invite (CHAT_CREATE_ROOM gate), 113 invite, 115 join (invite-only + bans enforced, returns subject + member records), 114 decline notice, 116/118 leave pushes, 119/120 subject; 105 w/ CHAT_ID routes room-scoped chat (members-only fan-out, lobby isolated); invites bridge natively both directions (`RoomInvited`), native `RoomKicked` → 118. **IM quoting (field 214 relayed) + away auto-response** (OPTIONS=4, shared once-per-away re-arm set; field 215 sets/clears away). 3 e2e tests. Notes: 117/119 pushes are Hotline-members-only; auto-response not durably stored (Hotline IMs ephemeral)
- [x] Threaded news transactions (370–411) mapped to boards; flat-news (101/102) projection — `apps/server/src/hotline.rs`: GetNewsCatNameList/GetNewsArtNameList/GetNewsArtData/PostNewsArt/DelNewsArt onto the shared `BoardService` (postable board→category, bundle→bundle; articles signed via the same author-seed as native; article ids a stable 32-bit projection of the event id); flat news GetMsgs/NewMessage/OldPostNews project the first postable board. Constants added to `legacy-hotline::constants` (verified vs Mobius)
- [~] File transactions (200–213); HTXF channel (port+1); FFO encode/decode (INFO/DATA forks, MWIN); fork-offset resume; folder lockstep — `apps/server/src/hotline.rs`: GetFileNameList (areas as root folders, drop-box hiding + SEE filter), GetFileInfo, DownloadFile (RBAC FILE_DOWNLOAD + drop-box checks, records the download); `serve_htxf` binds control-port+1 and streams the flattened file object (FILP + INFO + DATA forks) for whole-file download; 2 e2e tests. HTXF **upload**, fork-offset **resume**, and folder (recursive) downloads are the documented deferred slices
- [x] Account admin transactions (348–355); access-mask projection (big-endian bit order, tested); kick/ban (110/111) — codec (`rabbithole-legacy-hotline::access`: AccessMask big-endian bit order, 38-variant Privilege, UserFlags verified vs Mobius/HL1.9, obfuscate/deobfuscate; 42 tests) **wired into burrow**: NewUser/DeleteUser/GetUser/SetUser onto the shared account service + RBAC (documented role↔mask projection: caps→bits outbound, nearest-role inbound with lossy cases noted; superuser never assignable/demotable), all admin ops capability-gated + audit-logged; DisconnectUser kicks via the native `ServerEvent::Kick` path (DisconnectMsg 111 delivered, cannot-be-disconnected respected) with 30-min in-memory login+IP temp-ban; UserBroadcast bridged both ways (native admin Broadcast reaches Hotline clients as ServerMsg 104); login reply carries the projected USER_ACCESS bitmap. 6 unit + 3 e2e tests. Deferred: ban persistence/unban op, hard delete, per-bit grant/revoke masks, login rename
- [x] `apps/tracker`: native registry + HTRK (5498) listing + UDP (5499) heartbeat registration — `looking-glass` daemon (TTL registry, HTRK UDP registration + TCP listing codecs with packet diagrams, native `LIST` status port; 18 tests). Signed descriptors / tracker gossip land with W9
- [ ] Compat rig: archived Hotline clients + mobius-driven integration tests

## Wave 8 — Web & desktop GUI
*Depends on: W2–W4 (W5 for transfer UI); parallel with W6/W7*

- [~] `ui-web` Leptos SPA — `rabbithole-ui-web` (Leptos 0.6 CSR: router + Login, Lobby chat + WhoList, **Boards** tree/thread views, **DMs** conversation+compose, **member Directory** with live search, nav bar, ThemeToggle, StatusBar; DOM-free `UiState`/`apply` reducer + `UiClient`/`MockClient` seam; **browser WebSocket transport** (`ws.rs`, web-sys, RHP-over-ws hello + command↔frame mapping + keepalive) behind a host-testable `wire`/`EventClient` seam; wasm-check in CI; 37 host tests). Reconnect/backoff + profiles/keyword-bar views are the next slices
- [~] Files UI: browse, upload/download (WS + fetch), transfer queue — `ui-web`: `files.rs` (`FilesState` reducer + `Transfer` queue model), FILE-family `FileCommand`/`FileEvent` + `file_command_to_frame`/`frame_to_file_events` in `wire.rs` (host-tested), FolderBrowser/FileDetail/TransferQueue components + `MockClient::dispatch_file`. Live async transfer progress + real download bytes land when `WsClient` grows FILE-family dispatch
- [x] Art rendering (canvas) — `ui-web::art`: pure `parse_art`/`to_draw_ops` (reusing `rabbithole-art`'s Canvas/Cell/PALETTE, no parser duplication) + wasm-gated `paint` to an HTML canvas; ArtCanvas/ArtGallery components; 7 host tests
- [~] Design tokens; **light/dark mode** (OS-follow + manual override) across all rich clients — `ui-web::theme_css`: design tokens as CSS custom properties, `ThemeChoice` + pure `effective_mode(os_pref, override)` resolution (host-tested), wasm-gated localStorage persistence + reworked ThemeToggle. Retro/HighContrast pack plumbing (`root_style(pack, mode)`) is in place; TUI/desktop parity is a later slice
- [x] Theme packs: Clean (default), Retro (CP437/scanlines/ANSI palette), High Contrast; shareable token files — `ui-web::packs`: `PackTokens` (complete `--rh-*` token set per pack×mode; Retro = core Retro palette + scanline gradients + monospace/boxy; High Contrast = AAA palettes w/ WCAG contrast-floor tests), `to/from_tokens_json` (tamper-tolerant: unknown keys ignored, missing vars defaulted) as the server-theme-bundle seam; `ThemeChoice` now pack×mode (legacy persistence parsed), 3×3 picker in ThemeToggle; 120 host tests + wasm check green. Runtime pack-JSON loading from a server bundle is the follow-on
- [ ] Server theme bundle application (accents, icons, art, sounds) w/ safety rails (structured tokens only, contrast minimums, user cap/disable)
- [ ] Theme editor panel in web admin (upload assets, accents, live light/dark/retro preview)
- [ ] Embedded web client served by server; installable PWA
- [~] Web admin routes: config, accounts/classes, moderation, monitors (federation/radio panels as they land) — `ui-web`: `/admin` route with config (ConfigGet/Set), accounts & classes (AccountList/Set, ClassList/Set), moderation (Kick/Broadcast); `AdminCommand`/`AdminEvent` + `admin_command_to_frame`/`frame_to_admin_events` in `wire.rs` (host-tested, mirrors FILE family), `AdminState` reducer, nav gated on `is_admin`, `MockClient::dispatch_admin`. Plus `WsClient` **reconnect/backoff** (pure host-tested `backoff_delay` capped-exponential+jitter, `ConnState` Connecting/Online/Reconnecting/Offline surfaced in StatusBar) and live **FILE-family dispatch** over the socket. Federation/radio monitor panels land with those subsystems
- [ ] Tauri v2 desktop: core in-process, QUIC transport, native notifications, tray/menubar presence, `rabbit://` deep links, auto-update
- [ ] Server native GUI wrapper (systray + bundled daemon)
- [ ] Playwright E2E suite

## Wave 9 — Federation & discovery
*Depends on: W3 (+W4/W5 for catalogs/swarm; W8 admin UI helpful)*

- [x] S2S QUIC channel; server key exchange; peering handshake + admin approval — `apps/server/src/federation.rs`: dedicated QUIC endpoint on `federation_addr` (reuses `rabbithole-net` QuicListener/Transport + the burrow TLS identity/fingerprint pinning, ALPN `rhp/1`, handshake carried in the `Family::FEDERATION(8)` frames), nonce-bound Ed25519 challenge-response (both sides sign `rhp-fed-s2s-auth-v1‖keys‖nonces` with the server identity key, replay-proof). Unknown peers stay **Pending** until an admin approves via audited ctl `peer-approve`/`peer-revoke`; approvals persist across restart. `PeerRegistry`/`PeerRecord`/`PeerState` in server-core; `federation_*` config off by default (port 4655); graceful `close()` on every path. 5 unit + 4 e2e tests. Catalog sync/search over the session is the next slice
- [~] Board flood-fill: per-peer/per-board subscriptions, ihave/pull, Bloom+store seen-set — model landed: `rabbithole-federation` (`Subscription`/`IHave`/`PullRequest`/`PushEvents`/`FedEvent`, classic `BloomFilter` with optimal m,k; 25 tests). S2S transport + store wiring is the service slice
- [~] Tombstone/redact propagation (server-sovereign application) — `rabbithole-federation::redaction` (signed `Redaction`, verify authenticates; apply left to receiver). Propagation wiring pending
- [~] Ingest defense: dual-sig verify, per-peer rate limits, reputation, auto-defederation, allow/deny lists — `rabbithole-federation::policy` (per-peer token-bucket `RateLimiter`, allow/deny `PeerPolicy`) + descriptor verify done; reputation/auto-defederation pending
- [~] Cross-server identity attestation (`persona@server`, key continuity) — model landed: `rabbithole-federation::attestation` (`PersonaAttestation` home-server-signed under `rhp-fed-attestation-v1` w/ injected-clock freshness + name caps; strict `FedAddress` parser `[a-z0-9-_.]` alnum-edged, DNS-capped; `ContinuityChain` — rotations generation-bound + signed by the *previous* persona key so a server-side key swap fails verification, only the latest link must be fresh; `verify_visitor` challenge-response w/ 16-byte-min challenges under `rhp-fed-visitor-challenge-v1`); 30 tests (84 crate total). Wiring issuance + the challenge flow over the S2S transport is the server slice
- [x] Cross-server file search: signed catalogs, pull fan-out, blake3 dedupe — `rabbithole-federation` primitives (SignedCatalog w/ generation chain, search, dedupe_by_hash, plan_fetch; 54 tests) **wired over the S2S session**: `apps/server/src/fed_catalog.rs` builds the local catalog from publicly-listable files (anonymous-guest SEE|FILE_LIST check, drop-boxes excluded, blake3 blob ids), content-compared rebuilds (stable id when unchanged, generation+prev_id bump when changed), signed catalog persisted so the chain survives restarts; dialer-pull sync on peer connect (announce→compare→fetch→verify against the handshake-pinned key, staleness+tamper+non-approved refused, mid-session revocation cuts fetches); ctl `fed-catalogs`/`fed-search` (federated search w/ provenance + hash-dedupe). 3 e2e tests. Peer-catalog persistence, client-facing RHP search, and live swarm federated sources are follow-ups
- [~] `.well-known/rabbithole/server` (signed descriptor) — `rabbithole-federation::PeerDescriptor` (ed25519-signed under `rhp-fed-descriptor-v1`, verify + tamper tests) done; HTTP surface pending
- [x] Looking Glass tracker: signed descriptors, heartbeats, categories, tracker-to-tracker gossip — `looking-glass`: `descriptor` (ed25519 self-certifying `Descriptor` signed as `rhp-trk-descriptor-v1`‖postcard, field caps at verify; registry slots hold the first verified key until TTL expiry — anti-hijack over fast rotation; signed upgrades unsigned, unsigned refreshes but can't rewrite a verified slot, replay-stale timestamps rejected), categories (`LIST cat=<name>` filter + `CATEGORIES` summary verb + categories column), `gossip` (sans-IO `GossipDigest`/`diff→Want`/`GossipBatch`, `RHGS` single-datagram UDP codec on :4656 w/ signed `Announce`, push-pull anti-entropy over static `--gossip-peer`s, via-marker loop safety, gossiped entries under the same lazy TTL); 40 tests. Classic unsigned HTRK flow unchanged. Deferred: digest sampling beyond the 16-entry prefix, signed key-handover
- [~] Directory index (health/uptime, verifiable-not-authoritative); client server-browser UI — tracker side landed: `looking-glass::health` (`HealthLog` — 96×15-min ring anchored at each server's first observation, absolute bucket indices w/ lazy lap-invalidation, expected-heartbeats-so-far judging so fresh servers read 100%, integer per-mille uptime, flap counts fed by the lazy expiry sweep, logs forgotten after a silent 24h) + `INDEX`/`INDEX cat=`/`HEALTH <ip:port>` status verbs (signed-first → uptime → name sort; ASCII bucket sparkline; rows carry signed yes/no + key prefix + descriptor generation so clients verify via gossip `Want` instead of trusting). Uptime is local unsigned bookkeeping, never gossiped (gossiped entries start fresh logs). 59 tests. Client server-browser UI is the client slice
- [ ] Deploy flagship public **Looking Glass** (project-run, domain TBD); pre-configure in clients (user-removable)
- [x] 3-server CI testnet: partition/rejoin, dupe-storm tests — `e2e_w9_testnet.rs` (4 tests, ~3s): full 6-edge mesh federated search w/ provenance; partition (outer nodes keep serving, dead-node dials bounded) + rejoin on the same data dir (identity + approved peers + catalog chain persist, gen-2 supersedes pre-partition gen-1, no re-approval); 8× announce-storm redials + 5× signed-byte replays are no-op/stale-refused with byte-stable state; mesh-wide stale-generation refusal. Pins today's contract explicitly: one-hop dialer-pull (no transitive relay — flood-fill will break the assertion deliberately). Gaps documented for follow-up: peer catalogs in-memory w/o revoke eviction, no disconnect propagation (no QUIC idle timeout), listener never pulls

## Wave 10 — Syndication (NNTP → FTN → QWK)
*Depends on: W3 (dupe subsystem); W9 helpful*

**NNTP**
- [x] Reader server: CAPABILITIES, GROUP/LISTGROUP, ARTICLE/HEAD/BODY/STAT, NEXT/LAST, POST, OVER/XOVER + OVERVIEW.FMT, LIST; dot-stuffing both ways — codec (`rabbithole-legacy-nntp`, 51 tests) + a live gateway wired into burrow (`apps/server/src/nntp.rs`, config-gated `nntp_enabled`/`nntp_addr`, AUTHINFO→AuthService, POST→BoardService; e2e-tested). NEWNEWS/IHAVE peering deferred to the federation slice
- [ ] AUTHINFO USER/PASS on TLS only (563/STARTTLS)
- [x] Group↔board mapping (identity slug↔group); per-group monotonic article numbers; permanent Message-IDs (`<hex(blake3 event id)@origin>`); References threading; overview rendered from post metadata — in `apps/server/src/nntp.rs`
- [x] Peering: IHAVE/NEWNEWS with external peers; Message-ID dedupe via shared subsystem — codec (`legacy-nntp::transit` Exchange machine, `wildmat`, `datetime`, `listing`; 96 tests) **wired as a peer-feed service**: `apps/server/src/nntp_feed.rs` behind `nntp_feed_enabled` (default off; `nntp_feed_addr` :1120, `nntp_feed_peers` TOML allowlist — empty refuses all): AUTHINFO required before any transit verb (480 otherwise, TAKETHIS body still consumed), MODE STREAM/CHECK/TAKETHIS/IHAVE drive the codec Exchange, dedupe via the shared `SeenKey::MessageId` window (recorded on settle, both offered + header ids; native `<hex(event)@origin>` ids resolving to stored posts refused independently), articles validated to a known group and posted via BoardService with a gateway author seed (437/439 on malformed/unknown), NEWNEWS w/ wildmat+datetime for authed peers; reuses the reader's Message-ID/article parsing (`pub(crate)`). 3 e2e + 2 unit tests. Deferred: outbound feeding (receive-only + NEWNEWS pull), TLS on the feed port

**FidoNet**
- [x] PKT codec: type-2+ w/ type-2 fallback (capability word), packed messages, golden-file round-trip tests — `rabbithole-legacy-ftn` (bounds-checked LE reader, 5D addresses, CP437; 31 tests incl. 2000-iter fuzz)
- [x] Kludges: INTL/FMPT/TOPT/MSGID/REPLY/PID/TID; AREA:; Origin; SEEN-BY + PATH maintenance — `rabbithole-legacy-ftn::kludge` (canonical re-serialization, raw-CP437 text). Tosser/scanner/binkp/AreaFix/nodelist below are the service layer
- [x] Tosser + scanner services; ARCmail bundles (day-coded names + collision handling); BSO outbound — `rabbithole-legacy-ftn`: `tosser` (`Tosser::toss` classifies echomail vs netmail, cross-packet MSGID dedupe → `TossedBatch`, resolves 5D addrs from INTL/FMPT/TOPT, expands 2D SEEN-BY/PATH); `scanner` (group outbound into packets + pure BSO naming: lowercase-hex `NNNNnnnn`, `.?ut`/`.?lo` flavor matrix, cross-zone `outbound.NNN`, point paths); `arcmail` (`<hexdiff>.<weekday><seq>` with 0-9/a-z collision handling). Pure/sans-IO; the TCP mailer (binkp) service is the wiring slice
- [x] binkp mailer (FTS-1026, port 24554) — codec + FSM + service landed: `rabbithole-legacy-binkp` (2-byte block framing, M_NUL..M_SKIP, CRAM-MD5 auth verified against RFC 2202 vectors, sans-IO originating+answering session state machine; 42 tests); wired in `apps/server/src/ftn.rs` — tokio TCP listener (24554) drives the answering FSM to receive PKT/bundles into an inbound spool, `poll_uplink`/`run_originating` flush BSO outbound to a configured uplink; `ftn_*` config toggles, off by default. Crash-recovery resume + ARCmail zip decompression deferred
- [x] AreaFix (netmail commands: +/−/％LIST/％QUERY) — `rabbithole-legacy-ftn::areafix`: `parse` (password / `+`/`-` / bare-toggle / `%LIST`/`%QUERY`/`%HELP`, skips kludge/SEEN-BY/tearline) + `process` against `AreaFixConfig` producing the reply body
- [x] Nodelist + NODEDIFF parsing (CRC-16); echomail↔boards; netmail↔DM gateway; CP437 lossless round-trip — `rabbithole-legacy-ftn::nodelist` (parse, `apply_nodediff`, `crc16`/`verify_nodelist`) + CP437 via `::cp437`; gateways wired in `apps/server/src/ftn.rs`: inbound packets run the `Tosser`, echomail posts into the mapped board (AREA→slug) and netmail delivers as a DM (synthetic sender id 0), MSGID dedupe via `TossedBatch`, RBAC BOARD_POST/DM_SEND checked; outbound scanner stages local echomail posts (loop-guarded by `@{origin}` authorship) into BSO. 5 e2e tests

**RSS/Atom (inbound web syndication)**
- [x] Feed parsing: lenient hand-rolled XML pull tokenizer, RSS 2.0 + Atom 1.0 → `Feed`/`FeedItem`, manual RFC 2822 + RFC 3339 date parsing, HTML-to-text (tag strip + entity decode + char-boundary cap), blake3 dedup ids — `rabbithole-legacy-syndication` (blake3-only dep, 71 tests)
- [x] Feed → board ingestion layer (pure): `mapping` (`to_post_drafts(&Feed, &BoardMapping) -> Vec<PostDraft>` — subject/body/author fallback chains, canonical source link, reuses the blake3 dedup id), `seen` (`partition_fresh` + `SeenSet`, order-preserving, intra-batch repeat handling), `poll` (clockless conditional-GET state machine: `on_response(status, etag, last_modified, ttl, now)` covering 304/2xx/error with capped exponential backoff, TTL/`sy:updatePeriod` as minimum interval, saturating arithmetic). **Fetcher wired into burrow**: `apps/server/src/syndication.rs` — background service behind `syndication_enabled` (default off; `syndication_feeds` url→board map TOML-only, `syndication_poll_secs`), minimal HTTP/1.1 GET over tokio with sans-IO rustls + webpki-roots for https, conditional GET (If-None-Match/If-Modified-Since from `PollState`), 3-hop redirects, 1 MiB cap, timeouts; fresh items dedupe via `SeenKey::Syndication` and post via BoardService with a gateway author seed; 3 e2e tests (post-once → 304 → no dupes across restart). Feed-declared TTL wiring + IPv6 hosts + gzip are follow-ups.

**QWK/QWKE**
- [~] Packer: MESSAGES.DAT 128-byte blocks (0xE3 EOL, conf# @124–125), CONTROL.DAT, per-conf NDX with **MBF float** encode, DOOR.ID (QWKE advertised), bulletins — codec landed: `rabbithole-legacy-qwk` (MBF float verified against known vectors, Latin-1 fields; 40 tests incl. fuzz). ZIP bundling is the documented seam
- [x] QWKE long To/From/Subject kludges (both directions) — `rabbithole-legacy-qwk::qwke`
- [~] REP ingest: validate, dedupe, post as signed events — `rabbithole-legacy-qwk::reply`: `ReplyPacket::parse/encode` over `<BBSID>.MSG` → `Vec<ReplyMessage>` (conference from the to-reader slot, reusing the MESSAGES.DAT record codec); `validate()` surfaces per-record `ReplyProblem` (conf out-of-range / empty body / malformed header); `content_hash` (blake3 over semantic fields) + `dedupe()` catches re-uploads and within-batch repeats. Posting the accepted set as signed board events is the server-wiring slice
- [~] Delivery: CLI/web export, telnet surface, scheduled per-user packets; read pointers shared with offline mode — `rabbithole-legacy-qwk::packet::build_packet` assembles MESSAGES.DAT/CONTROL.DAT/per-conf NDX/DOOR.ID (reusing existing encoders) and exposes `QwkPacket::members()` named buffers for the ZIP/export seam. CLI/web/telnet surfaces + scheduling are the server slice
- [ ] Syndication admin UI: per-board network mappings, feed monitor, dupe stats

## Wave 11 — Radio
*Depends on: W1, W4 (W8 for UI polish)*

- [x] Station/mount model (multiple stations, per-server toggle) — `rabbithole-audio` (PCM frames, mixer, `Station` fan-out, jitter buffer, VU meter; 21 tests) + `rabbithole-radio::StationRegistry` (create/remove/list, per-station enable toggle, listener accounting; 23 tests)
- [x] Playlist engine: rotation (Sequential/seeded-Shuffle/RepeatOne), **vote queue** + requests, `StationController` tying playlist→audio Station with now-playing/metadata — `rabbithole-radio`. Library-from-file-areas source wiring is the server slice
- [x] DJ live source (Icecast SOURCE/PUT + Basic auth) — works with butt/ices — dedicated source-ingest listener (`radio_source_enabled`/`radio_source_addr` :8001, `radio_source_user`/`radio_source_password` config creds, off by default): `legacy-icecast::parse_source_request` auth → OK2/401/403, mount claim into the existing per-mount byte fan-out, DJ pre-empts the playlist program (`go_live`) and rotation resumes on disconnect (graceful shutdown); now-playing from `ice-*` headers. Library-from-file-areas playlist source (`radio_library_areas` area→mount map via FileService → `tracks_from_nodes` → `install_program` + 1s playlist driver). Bytes→PCM decode remains a follow-up; **mid-stream metadata codec landed**: `legacy-icecast::admin` (`parse_metadata_update` — `/admin/metadata` + `admin.cgi` updinfo, total percent-decoding, query-`pass`/Basic-auth with header-wins, Icecast `<iceresponse>` XML replies incl. 401 challenge) + `::metaread` (`MetaintReader` incremental client-side de-interleaver: chunk sizes 1..=17, empty/NUL blocks, malformed-block counter without desync) + `StreamTitle` encode/parse helpers; 86 unit + 3 doctests. **updinfo wired**: the source-ingest listener sniffs `parse_metadata_update` before source ingest — creds vs `radio_source_user/password` (empty-password fail-safe), mount from `mount=` or the sole live mount (admin.cgi), "Artist - Title" split, now-playing + presence republished (pure-DJ mounts too), XML `<return>` / 401 replies. 6 e2e + 3 unit tests
- [ ] Encode pipelines: Opus/Ogg primary + MP3 legacy mount
- [x] Delivery: ICY mounts w/ exact icy-metaint math — codec (`rabbithole-legacy-icecast`, 43 tests) + a live listener wired into burrow (`apps/server/src/radio.rs`, config-gated `radio_enabled`/`radio_addr`): SOURCE/PUT DJ auth via AuthService+`Caps::BROADCAST`, per-mount broadcast fan-out, listeners get metaint-spliced `StreamTitle`, lagging listeners dropped; e2e-tested. Native QUIC uni-stream + HTTP/Ogg transports are later refinements
- [~] Listener counts in presence; now-playing surfaced (presence line, TUI status, telnet, web) — server side landed: `PresenceRegistry` gains `RadioStatus` + `set/clear_radio_now_playing`, publishing `ServerEvent::RadioNowPlaying` on the bus. **TUI landed**: `apps/tui/src/radio.rs` — status-bar `♪` segment + Ctrl-N station panel (live-DJ-wins reducer, render-to-lines, truncation; 9 tests), fed by the `[radio]` ServerNotice bridge — **server emission wired**: `push_for_event` projects `ServerEvent::RadioNowPlaying` to the bridge-format ServerNotice (live flag from `presence.radio_status`, never recorded to offline replay) and station teardown publishes the `off` notice; format round-trip tested against the TUI parser spec. Remaining: RADIO proto family (replacing the bridge), telnet + web surfacing
- [ ] Client players: GUI/web, TUI handoff, per-user enable + volume/ducking settings

## Wave 12 — Mobile & distribution
*Depends on: W8 (W11 for background audio)*

- [ ] Tauri iOS/iPadOS + Android builds; mobile plugin glue: notifications, background audio + audio session, share sheet
- [ ] Transport resilience on mobile (QUIC connection migration, WS fallback)
- [ ] App Store (TestFlight) + Play (.aab) packaging, signing, privacy manifests, entitlements
- [~] `dist` release automation (CLI/TUI/server): archives, installers, Homebrew — `.github/workflows/release.yml` (tag-triggered cross-platform binary archives + checksums + GitHub Release) and `scripts/release.sh` landed; installers/Homebrew tap pending
- [x] Docker images (multi-stage → slim) + docker-compose; systemd unit; install docs — `Dockerfile` + `.dockerignore` + `docker-compose.yml` + hardened `contrib/burrow.service` + `docs/deployment.md` (accurate to real bins/ports/env vars)
- [~] Versioned protocol docs published (docs site) — the spec itself is current in-repo (see W-Continuous lockstep line); publishing to a docs site is the remaining step

## Wave 13 — Hardening & 1.0
*Depends on: all*

- [~] E2EE DMs + private rooms: X25519 + Double Ratchet (1:1) + **Sender Keys (groups)**, ChaCha20-Poly1305, sealed sender — crypto core complete: `rabbithole-e2ee` (Double Ratchet w/ bounded skipped-key store + transactional decrypt, X3DH-lite, sealed sender, Ed25519-signed sender-key group messaging w/ bounded skip + rekey; RNG-generic, wasm-friendly; 38 tests). Per-thread opt-in wiring into the DM/room proto + key-backup UX pending
- [~] Moderation suite: report queues, quarantine-for-review, shared blocklists + blake3 hash-deny lists, moderation audit UI — server side landed: `store-server` migration 0008 + `repo7` (reports w/ state-machine guard, quarantine, deny_hashes; STRICT+indexed); `server-core::moderation` (`ModerationService` — file_report deduped by (reporter,subject) while open, claim/resolve/dismiss, quarantine + deny mirrors warmed at boot, every mutation audited); new `Caps::MODERATE` (1<<54, Moderator default — dedicated bit since the queue spans posts/DMs/files/users); ADMIN family types 30–40 + `handlers11`; moderators get a ModNotice push on new reports. Quarantine enforced on native RHP paths (thread list/fetch, file list/search, node get, inline+bulk download, folder manifest — files by blob hash so aliases covered); deny-hash at upload finalize (staging deleted), inline upload, DM attachments. 4 e2e tests. Deferred (documented): legacy surfaces/syndication/federation/swarm don't consult quarantine yet, BlobPut (avatars) skips deny, shared cross-server blocklists + the moderation audit UI
- [~] Rate limiting everywhere (buckets per IP/account/endpoint-class); mCaptcha option; invite trees — **limiter landed**: `server-core::ratelimit` (hand-rolled token buckets, injectable clock, lazy buckets + expiry sweep, saturating math, `peek_with` so successes never spend auth budget; 8 tests) with six classes wired across every surface — `conn` 30/min/IP at all accept loops, `auth` 5/min/IP on failures (native, AUTHINFO, Hotline, telnet, radio SOURCE/updinfo), `msg` 10/s/account (ChatSend/DmSend), `post` 6/min/account (native, NNTP POST, Hotline news), `transfer` 10/min/account (native TransferOpen after the concurrency cap, Hotline DownloadFile), `legacy` 20/s/IP per command; 13 `ratelimit_*` config knobs (`ctl config set`, 0-rate disables), sparse once-per-key-per-minute audit; 4 e2e tests (flood→RateLimited while session survives, AUTHINFO hammer→481+close, conn drop, disabled→no limits). Deferred: finger conn gate (accept loop lives in the crate), binkp/S2S auth classes, Retry-After propagation; mCaptcha + invite trees are separate slices. (Invite codes themselves landed in W2.)
- [ ] Fuzzing coverage goals (all codecs); RUSTSEC audit gate in CI; security review checklist/pen-test pass
- [ ] Load harness (target: 10k concurrent sessions/server) + performance pass
- [ ] Accessibility pass (web/GUI); i18n scaffolding
- [ ] Backups: snapshot + restore tested; migration guides
- [ ] 1.0: docs site complete, flagship sample server config, launch

## Wave 14 — Reticulum & off-grid mesh (post-1.0 / 1.1)
*Depends on: W3, W9, W13*

- [ ] Spike: `reticulum-rs` maturity vs Python RNS gateway sidecar → decision
- [~] RNS interop foundation: identity (X25519+Ed25519), destination/name hashes, packet + announce codecs, ECDH+AEAD token — `rabbithole-reticulum` (spec-faithful hashes, never-panic decoders, documented cipher/field divergences; 40 tests). The live RNS transport adapter (Burrow as a Reticulum destination, constrained RHP profile) builds on this
- [~] LXMF bridge: DMs ↔ LXMF (delay-tolerant, NomadNet-compatible); boards ↔ LXMF propagation nodes (shared dupe subsystem) — message layer landed: `rabbithole-reticulum::lxmf` (`LxmfMessage`/`SignedLxmf`, `hash()` = SHA-256(dest‖src‖packed payload), Ed25519 sign/verify, deterministic postcard packing via a sorted fields map, total `unpack`; 22 tests). Wiring DMs↔LXMF over the RNS transport + propagation-node board bridge (with the shared dupe subsystem) and reconciling the packing with upstream MessagePack are the transport slices; the LXMF stamp/PoW is deferred
- [ ] Delay-tolerant Tunnels (S2S flood-fill) over RNS with bandwidth-aware batching
- [ ] rabbit links w/ RNS destination hashes; Looking Glass entries may advertise RNS destinations
- [ ] LoRa/packet-radio deployment docs (power/bandwidth budgets)

---

## Continuous tracks (every wave)

- [~] Protocol spec kept in lockstep with implementation (`docs/protocol/`) — full drift audit vs `crates/proto` (7 mismatches fixed incl. the file.md BulkPreamble mis-typed as frame 30, missing session personas/TOTP/Register groups, presence directory 10–13, blob 100–103); new `federation.md` (S2S wire + catalog sync + attestation, wire-today vs model-only marked) and `docs/legacy-surfaces.md` (12-surface operator matrix: ports, toggles, min-roles, rate classes, deferrals); README family table (RADIO 9 reserved, FEDERATION 8 S2S-only). Ongoing discipline: re-audit with each proto change
- [ ] Golden-file + fuzz tests accompany every codec
- [ ] CHANGELOG + semver discipline on `proto`
- [ ] Mobile cross-compile smoke in CI from W0 (front-load NDK pain)
