# RabbitHole — Project Plan

> A modern Rust revival of the golden age of online communities — Hotline, Haxial KDX,
> BBSes, and AOL — one server, many doors: native apps, terminals, browsers, telnet,
> newsreaders, offline mail packets, and FidoNet.
>
> This plan is the synthesis of the original prompt (`prompt.md`), eight deep research
> briefs (`docs/research/01…08`), and the project owner's locked decisions. It is
> organized into **waves with an explicit dependency structure**. Implementation does
> not begin until this plan is reviewed and approved. The distilled tracker lives in
> `TODO.md`.

---

## Table of Contents

1. [Vision & North Star](#1-vision--north-star)
2. [Locked Decisions](#2-locked-decisions)
3. [Research Summary](#3-research-summary)
4. [Architecture Overview](#4-architecture-overview)
5. [The RabbitHole Protocol (RHP)](#5-the-rabbithole-protocol-rhp)
6. [Data Model](#6-data-model)
7. [Security, Identity & Permissions](#7-security-identity--permissions)
8. [Federation & Discovery](#8-federation--discovery)
9. [Feature Specifications](#9-feature-specifications)
10. [Legacy Interoperability](#10-legacy-interoperability)
11. [Client & Server Frontends](#11-client--server-frontends)
12. [Storage & Persistence](#12-storage--persistence)
13. [Packaging & Deployment](#13-packaging--deployment)
14. [Testing Strategy](#14-testing-strategy)
15. [Waves & Dependency Graph](#15-waves--dependency-graph)
16. [Open Questions & Defaults](#16-open-questions--defaults)

---

## 1. Vision & North Star

RabbitHole recreates the **sense of place** of the classic online services — a server
is a *destination* with its own name, art, culture, rules, radio station, and regulars —
on a foundation that is modern, open, secure, and federated.

Guiding principles (distilled from the research):

- **Warmth without the cage** (AOL lesson): chosen screen names, buddy lists, presence,
  humanized events ("you've got mail" moments), keyword teleport navigation — but on an
  open, documented protocol with portable identity and federation, never a walled garden.
- **The transaction heart of Hotline, modernized**: one uniform request/reply/push
  message model for everything; add a feature = add a message type, never new framing.
- **KDX's lessons**: encrypt everything, class-based permissions, multiple chat rooms,
  server-side file indexes, remote administration — but with *published*, audited
  crypto and an open spec (KDX died of proprietary obscurity).
- **BBS craft**: message bases that syndicate (FidoNet, QWK, NNTP), ANSI/CP437 art as a
  first-class medium, telnet access for real retro terminals, sysop culture and tooling.
- **One shared Rust core, many thin frontends**: every client and server surface (CLI,
  TUI, web, native GUI, telnet) is an adapter over the same `Command → Core → Event` API.
- **The server is trusted and central — and that's a feature**: it plays tracker,
  coordinator, relay, and permission authority for the swarm layer, roles BitTorrent
  has to solve the hard way.
- **Federation designed in from day one**: content-addressed, signed events; portable
  Ed25519 identity; append-only flood-fill sync (Usenet's model, not Matrix's).

---

## 2. Locked Decisions

Decisions made by the project owner (2026-07-01):

| # | Decision | Choice |
|---|----------|--------|
| 1 | Scope | **Full-vision phased roadmap** — comprehensive, wave-sequenced; every feature in the prompt is planned, nothing dropped for an MVP shortcut. |
| 2 | GUI stack | **Tauri v2** with **maximum Rust/wasm**: the UI is a Rust wasm SPA (Leptos) reused verbatim as (a) the Tauri webview content on desktop + mobile, (b) the embedded web client, (c) the server web admin. |
| 3 | Legacy interop | **All must-haves for v1**: Telnet/ANSI BBS access, Hotline protocol compatibility (real Hotline clients can connect), and message syndication (FidoNet + QWK + NNTP). The **RabbitHole-native protocol remains the flagship** — comprehensive, modern, best-in-class. |
| 4 | Federation | **First-class from day one** — the data model, identity, and protocol are federated by design; server-to-server sync ships as a core wave, not an afterthought. |

Derived defaults (revisable, flagged in [§16](#16-open-questions--defaults)):

- Primary transport QUIC (quinn + rustls), mandatory WebSocket fallback for browsers.
- Wire format: postcard (serde) with explicit versioning; CBOR for canonical/hashed docs.
- Server DB: SQLite via sqlx (Postgres-ready behind a trait). Client DB: rusqlite.
- Hashing/content addressing: BLAKE3 everywhere; Bao verified streaming for transfers.
- Identity: Ed25519 per-user + per-server keys; Argon2id passwords; opaque session tokens.

---

## 3. Research Summary

Eight briefs live in `docs/research/`. One-line takeaways:

| Brief | Key takeaways adopted |
|---|---|
| `01-hotline.md` | Transaction+TLV field model; 64-bit access bitmask; threaded news; tracker; two-channel transfer pain → multiplex on QUIC instead; full byte-level tables for the compat layer (types 101–500, field IDs, HTXF, FFO, tracker format). |
| `02-haxial-kdx.md` | Account **classes** over per-user flags; multiple chat rooms; hide-vs-deny folder ACLs; server file index; remote admin surface; open spec or die; never proprietary crypto. |
| `03-bbs-syndication.md` | Canonical internal message model with **syndication adapters at the edges**; FTN PKT (58-byte LE header) / SEEN-BY / PATH / MSGID dupe control; QWK 128-byte blocks + 0xE3 EOL + MBF NDX floats; SAUCE record; door dropfiles; security-level projection for legacy. |
| `04-aol-walled-gardens.md` | Screen names + multiple personas; buddy list with pub/sub presence (server-stored); keyword/command bar; member directory; away messages; welcome screen; per-identity capability tiers; OSCAR's FLAP/SNAC/TLV shape validates our protocol design. |
| `05-swarm-p2p.md` | BLAKE3/Bao content addressing (verification decoupled from chunk size); server as private tracker/coordinator/relay; **advertise-without-upload** via metadata + signed capability tokens; QUIC streams per chunk; iroh as substrate; persist bitfields for resume. |
| `06-rust-stack.md` | Shared `core` with Command/Event API; Tauri v2 + Leptos wasm; ratatui TUI; quinn + tungstenite behind a `Transport` trait; postcard wire; sqlx/rusqlite; cpal/symphonia/rodio + Opus; workspace layout; `dist` + Tauri bundler + Docker. |
| `07-legacy-line-protocols.md` | Per-protocol tokio codecs + connection state machines; telnet IAC/NAWS/TTYPE/BINARY; NNTP command surface + overview cache; ICY metadata math; CP437↔Unicode; shared presence registry feeding finger/who/BBS/Icecast. |
| `08-security-federation.md` | QUIC/TLS 1.3 primary; Argon2id (64 MiB/t=3); Ed25519 identity cross-signed by home server; role → u64 bitmask → nearest-ancestor ACL (deny wins); **signed flood-fill federation** with blake3 content IDs (no Matrix state resolution); `.well-known` + modern trackers + directory; opt-in E2EE for DMs only. |

---

## 4. Architecture Overview

### 4.1 Shared-core, thin-frontend

Everything reusable lives in `rabbithole-core`. Every frontend — CLI, TUI, Leptos
wasm SPA (in a browser or a Tauri webview), and even the telnet BBS renderer — drives
the same loop:

```
Frontend ──Command──▶ Core (state machines, session, cache) ──Event──▶ Frontend
                        │
                        └─ Transport trait ──▶ QUIC (quinn) │ WebSocket (tungstenite)
```

Rules that keep the matrix affordable:

- `core` depends on **nothing UI** and is wasm-compatible (net/time/fs feature-gated).
- Frontends depend on `core`, never on each other.
- `Transport` and `Repository` are traits in core; platform impls plug in.
- The wasm SPA compiles the **same** `proto` types the server uses — zero schema drift.

### 4.2 Cargo workspace layout

```
rabbithole/
├─ Cargo.toml                    # [workspace] + [workspace.dependencies]
├─ crates/
│  ├─ proto/                     # RHP wire types, postcard framing, version negotiation
│  ├─ core/                      # client-side domain logic, Command/Event API, session,
│  │                             #   offline store orchestration, transfer queue
│  ├─ server-core/               # server domain logic: rooms, bases, files, presence,
│  │                             #   permissions evaluator, federation engine
│  ├─ net/                       # Transport trait; quinn + tokio-tungstenite impls; rustls
│  ├─ identity/                  # Ed25519 keys, signatures, tokens, argon2, TOTP
│  ├─ store-server/              # sqlx/SQLite repositories + migrations
│  ├─ store-client/              # rusqlite local store + migrations (offline mode)
│  ├─ swarm/                     # manifests, chunk scheduler, Bao verify, peer wire, NAT
│  ├─ art/                       # CP437↔Unicode, ANSI/SGR parser+renderer, SAUCE r/w
│  ├─ screen/                    # shared "terminal screen" layer: ratatui widgets +
│  │                             #   custom backend serializable to telnet sockets
│  ├─ audio/                     # cpal/symphonia/rodio, Opus encode, radio client
│  ├─ legacy-hotline/            # Hotline transactions, HTXF, FFO, tracker (compat)
│  ├─ legacy-telnet/             # telnet codec (IAC/NAWS/TTYPE/BINARY state machine)
│  ├─ legacy-nntp/               # NNTP server codec + article/overview projections
│  ├─ legacy-ftn/                # FTN: PKT codec, tosser/scanner, binkp mailer, areafix
│  ├─ legacy-qwk/                # QWK/QWKE packer/unpacker, REP ingester, MBF floats
│  └─ ui-web/                    # Leptos wasm SPA (client UI + admin UI, feature-gated)
├─ apps/
│  ├─ cli/                       # rabbit — clap CLI client
│  ├─ tui/                       # rabbit-tui — ratatui client
│  ├─ gui/                       # RabbitHole — Tauri v2 app (desktop + mobile)
│  ├─ server/                    # rabbithole-server — headless daemon (quinn + axum)
│  ├─ server-tui/                # sysop console (local or remote-admin over RHP)
│  └─ tracker/                   # rabbithole-tracker — directory/tracker service
└─ docs/ Dockerfile dist-workspace.toml tauri.conf.json
```

### 4.3 Server process anatomy

One tokio runtime; one listener task per enabled surface; all funnel into `server-core`:

| Listener | Default port | Protocol |
|---|---|---|
| RHP native | **4653/udp** (QUIC) + **4653/tcp** (TLS fallback) | flagship — 4653 spells **H-O-L-E** on a phone keypad |
| HTTP/WS | **4654** | embedded web client, web admin, WS transport, `.well-known`, rabbit links |
| Tracker (native) | **4655** | directory service (`apps/tracker`) |
| Telnet | 23 (or 2323 unprivileged) | BBS surface |
| finger | 79 | presence |
| NNTP | 119 / 563 (TLS) | reader + peering |
| Hotline compat | 5500 + 5501 (HTXF) | legacy clients |
| Hotline tracker compat | 5498 | optional, tracker app |
| binkp | 24554 | FidoNet mailer |
| Radio (ICY/HTTP) | 8000 or via 4580 | streaming |

Shared internal services: **presence registry** (actor), **permission evaluator**
(cached effective masks), **message-base store** (+ per-protocol projections),
**file index**, **swarm coordinator**, **federation engine**, **event bus**
(tokio broadcast) feeding every surface.

All numeric defaults are configurable; every listener can be disabled per server.

---

## 5. The RabbitHole Protocol (RHP)

The flagship native protocol. Design DNA: Hotline's transaction model + OSCAR's
family/TLV extensibility + modern transport. Fully documented in-repo
(`docs/protocol/`) from day one — the spec is a deliverable, not an afterthought.

### 5.1 Transport

- **Primary: QUIC** (quinn, TLS 1.3, ALPN `rhp/1`). One connection per session:
  - Stream 0 (bidi): **control stream** — transactions.
  - Server-initiated uni streams: event push (ordered per-topic).
  - Ad-hoc bidi streams: bulk transfer (file chunks, art blobs, avatars, radio).
- **Fallback: WebSocket** over HTTPS (`/rhp` on the web port) with identical binary
  frames — mandatory, because browsers/wasm can't do raw QUIC, and some networks
  block UDP. Both behind the `Transport` trait.
- Certificates via ACME (`rustls-acme`) or self-signed + fingerprint pinning
  (fingerprint travels in rabbit links, tracker entries, and `.well-known`).

### 5.2 Framing & message model

Every message is a postcard-encoded frame on the control stream, length-delimited:

```
Frame {
  version: u16,            // protocol version, negotiated at Hello
  kind:    Kind,           // Request | Reply | Push
  family:  u8,             // namespace: 0=session 1=presence 2=chat 3=dm 4=board
                           //   5=file 6=swarm 7=admin 8=federation 9=radio ...
  type_:   u16,            // operation within family
  id:      u64,            // request id; echoed in Reply; 0 for Push
  error:   Option<ErrorCode>,
  payload: Payload,        // typed serde enum per (family, type_)
}
```

- **Requests** carry a client-chosen `id`; **Replies** echo it with `error` set.
- **Pushes** are server-initiated (chat lines, presence deltas, mail alerts) — clients
  route by `(kind, family, type_)`, never by outstanding-request id (Hotline lesson).
- Pipelining is legal and expected; the server never assumes strict request→reply order.
- Forward compatibility: payload enums are `#[non_exhaustive]`; unknown optional fields
  skip cleanly; a `Hello` capability exchange gates features (like NNTP CAPABILITIES).
- Large payloads never ride the control stream — they get a **transfer ticket**
  (BLAKE3 hash + token) and move on a dedicated QUIC stream (RefNum idea, made robust).

### 5.3 Session lifecycle

1. `Hello` (client) — proto versions, capabilities, client name/version.
2. `HelloAck` (server) — chosen version, capabilities, server identity key, sign-in modes.
3. Authenticate: password (Argon2id verify), Ed25519 challenge/response, guest (if enabled),
   or session-token resume. Optional TOTP step.
4. Server pushes: agreement (if unaccepted), welcome bundle (logo/banner/MOTD), the
   user's effective permission mask, initial presence roster, unread counts.
5. Steady state: transactions + pushes + keepalive; graceful resume on reconnect
   (session token + replay cursor for missed pushes).

---

## 6. Data Model

All IDs are content hashes or ULIDs; all federated objects are signed. Canonical
serialization = deterministic CBOR (for hashing); storage = SQL.

### 6.1 Identity

```
Account   { id, created_at, auth: {argon2_phc?, totp?, recovery_codes},
            identity_keys: [Ed25519 pub], class_id, flags, quota, … }
Persona   { id, account_id, screen_name (unique, spaces ok), pronouns?,
            avatar_ref, banner_ref, profile: {location?, interests?, quote?, …},
            buddy_list, prefs, capability_tier }   // AOL: up to N personas/account
ServerKey { ed25519 pub/priv, rotation history }   // signs events + attests user keys
```

- A user *is* `persona@server.domain` to the outside; the Ed25519 key is the portable
  root (re-attestable on migration).
- Guest = a restricted ephemeral persona; guests **optionally disabled** per server.

### 6.2 Community objects

```
Room      { id, name, topic, kind: Public|Private|AdHoc, members?, subject, art_theme? }
DM Thread { id, participants, e2ee: bool, messages[] (attachment refs, max size = server cfg) }
Board     { id, slug (dotted, e.g. rabbit.general), title, description, kind: Category|Bundle,
            parent?, acl, syndication: {nntp_group?, ftn_area?, qwk_conf?}, retention }
Post      { event_id = blake3(canonical), board_id, thread refs (parent, root),
            author (persona@server + key sig), origin server sig, subject, body (markdown |
            text/plain | text/x-ansi), created_at, edits/tombstones as follow-up events }
```

Posts are **append-only signed events** — the same object federates, syndicates to
NNTP/FTN/QWK, and syncs to offline clients. `Message-ID`/FTN MSGID/QWK numbers are
projections stored alongside.

### 6.3 Files

```
Area      { id, name, root folder, acl, dropbox?: bool }
Node      { id, parent, name, kind: Folder|File|Alias, size, blake3, mime,
            icon_ref, comment, uploader persona, upload date, download_count,
            rating?, acl_override?, sauce?: SauceRecord }
Manifest  { swarm manifest — see §9.6 }
Transfer  { persistent queue entry: manifest/file hash, direction, bitfield,
            state, priority — survives restarts }
```

### 6.4 Requests (wish system)

```
Request { id, kind: File|Board|Feature|Other, title, details, requester,
          status: Open|Claimed|Fulfilled|Declined, claims[], fulfillment_ref?, votes }
```

---

## 7. Security, Identity & Permissions

(Adopting `08-security-federation.md` wholesale; highlights here.)

- **Transport**: TLS 1.3 always (QUIC native or rustls/TCP). No plaintext native mode.
  Legacy surfaces (telnet, Hotline compat, finger) are explicitly labeled insecure and
  individually toggleable; they authenticate against the same accounts but can be
  restricted (e.g. "legacy surfaces read-only" or class-gated).
- **Passwords**: Argon2id `m=64MiB, t=3, p=1` (PHC string, rehash-on-login).
  **2FA**: TOTP (RFC 6238) + hashed recovery codes. **Key auth**: Ed25519 challenge.
- **Sessions**: opaque 32-byte tokens, server-stored, revocable, idle+absolute expiry,
  rotation on privilege change.
- **Authorization — three layers, deny wins, cached**:
  1. **Roles**: `guest < user < moderator < admin < superuser` (ordered enum).
  2. **Classes** (KDX): named permission sets; every account belongs to a class;
     editing a class updates all members. A class stores a **u64 capability bitmask**
     (Hotline heritage: file ops, chat, boards, DMs, swarm advertise, admin ops,
     broadcast, cannot-be-kicked, …) + per-user grant/revoke masks.
  3. **ACLs** on folders/files/boards/rooms: `(resource, principal, allow, deny)`,
     nearest-ancestor inheritance, **hide vs deny** distinguished (KDX), evaluated to
     a cached effective mask.
- **E2EE DMs (opt-in, later wave)**: X25519 + Double Ratchet (vodozemac),
  ChaCha20-Poly1305, sealed-sender envelopes. Public content stays operator-readable
  (searchable, moderatable) — encrypted at rest.
- **Abuse prevention**: `governor` token buckets per (IP, account, endpoint class);
  user/IP/CIDR bans with reason+expiry+issuer; registration gating (open / invite
  tokens / email / mCaptcha); guest tier minimal mask; report queues, tombstones,
  quarantine-for-review, shared hash deny-lists; full audit log of admin/mod actions.
- **Privacy**: minimal logging (configurable retention), secrets encrypted at rest,
  data export + account deletion with federated tombstone (best-effort, documented).

---

## 8. Federation & Discovery

First-class from day one: every post is born a signed, content-addressed event; the
S2S engine ships as a core wave.

### 8.1 Message-base federation — signed flood-fill

- Event ID = `blake3(canonical CBOR)`. Dual signature: author key + origin server key.
- Servers **subscribe per-board per-peer**. New event → `ihave` offer to subscribed
  peers → peer pulls unseen IDs. Seen-set (Bloom + store) = O(1) dedup; loops impossible.
- Ordering: per-thread parent DAG; display order `(created_at, event_id)`. No global
  consensus, no Matrix state resolution — append-only makes it unnecessary.
- Moderation: signed `tombstone`/`redact` events flood the same way; each server is
  sovereign in applying them.
- Ingest defense: verify both sigs, per-peer rate limits + reputation, auto-defederate
  thresholds, allow/deny lists.

### 8.2 Cross-server file search — pull fan-out

Peers publish signed **file catalogs** (path, size, blake3, tags, permission summary).
Home server fans out queries, aggregates, dedups by hash. Transfers go direct to the
origin (its authz applies) — or join the swarm if chunks are advertised (§9.6).

### 8.3 Cross-server identity

`persona@server` anchored by the user's Ed25519 key, cross-signed by the home server
(keys published at `.well-known`). Migration = re-attest the same key elsewhere;
peers can require key continuity.

### 8.4 Discovery — three layers

1. **`.well-known/rabbithole/server`** — signed JSON: endpoints, ports, versions,
   key fingerprints, capabilities, guest policy. Authoritative bootstrap.
2. **Trackers** (`apps/tracker`, Hotline heritage modernized): servers register signed
   descriptors with heartbeats; clients browse; categories + rich metadata (name,
   description, topics, user count, region, federation openness, uptime). Trackers
   gossip registrations to each other. Also speaks the **legacy Hotline tracker
   protocol** on 5498 so old clients can browse (§10.2).
3. **Global directory** — aggregating index over tracker gossip + `.well-known` crawl,
   with health metrics and optional catalog/topic search. Index, not authority:
   everything it serves is server-signed and verifiable.

---

## 9. Feature Specifications

### 9.1 Users, profiles & presence

- Registration (per server policy), login, password change, key enrollment, TOTP.
- **Personas**: multiple screen names per account (default cap 5, server-config),
  each with own profile, avatar (raster, size-capped, content-addressed), **banner
  image** shown in the connected-users list, buddy list, prefs, capability tier.
- **Profiles**: lightweight fun card — location, interests, quote, "now playing",
  free text; ANSI-art profile section supported. `Get Info` on any user.
- **Member directory**: search personas by name/field; "locate online" honoring privacy.
- **Presence**: pub/sub. States: Online, Away (custom message, auto-expiring), Idle
  (auto), Invisible, Offline. Server-stored buddy lists (groups, permit/deny) so they
  roam. Arrival/departure events with optional (mutable) sounds.
- **Who's online**: live list with persona, banner, icon, idle, current surface
  (native/web/telnet/hotline), feeding: native roster, BBS who screen, finger, admin.

### 9.2 Chat

- **Public rooms**: multiple named, categorized rooms (KDX/AOL); topic/subject; roster;
  capacity with overflow suggestion; per-room ACL; optional room art/theme.
- **Ad-hoc rooms**: create-by-name (AOL primitive) — private rooms are invite/link
  based, not guess-the-name.
- **Private rooms**: invite flow (invite/decline/join/leave pushes), subject changes.
- Formatting: plain text + light markdown; /me emotes; mention highlighting;
  scrollback fetch; per-room history retention policy (server-config).
- Moderation: kick from room, mute, room bans, slow-mode.

### 9.3 Direct messages & notifications

- 1:1 and small-group DM threads; persistent history (user-controllable retention);
  quoting; away auto-response (AOL); typing indicators (optional, capability-gated).
- **Attachments**: any file, server-configurable max size; stored content-addressed;
  inline previews for images/art in rich clients.
- **Notifications**: in-protocol push events; native OS notifications via Tauri plugin
  (desktop + mobile); TUI/CLI bell + status line; web notifications API. A tasteful
  signature "you've got mail" sound (optional, off by default in quiet mode).
- Offline delivery: DMs queue server-side; delivered + read receipts (privacy-gated).

### 9.4 Message bases (boards)

- Hierarchy: categories → bundles → boards (Hotline threaded-news shape) with
  dotted slugs (`rabbit.general`) that map 1:1 to NNTP groups.
- Threaded posts (parent/root refs), markdown or plain or ANSI bodies, signatures,
  edit-as-new-event with visible history, tombstones.
- Per-user **read pointers** (high-water marks per board — same concept feeds QWK
  lastread and NNTP article numbers).
- **Offline mode**: client batch-downloads selected boards (delta since cursor) into
  the local rusqlite store; user reads/replies offline; replies queue and sync on
  reconnect (conflict-free: append-only events). This is the same machinery as
  federation subscription — a client is "a tiny peer."
- Board-level ACLs, moderator assignment, retention/archive policy (KDX auto-archive
  to text bundles), pinned posts, per-board syndication toggles.

### 9.5 Files

- Areas → folder trees; per-node metadata: icon (from a built-in retro set or custom),
  comment, uploader attribution, dates, download counter, optional ratings,
  SAUCE-aware art metadata display.
- Full ACLs with hide-vs-deny; **drop boxes** (write-only upload folders, contents
  visible only with privilege); aliases/links.
- **Server file index**: background indexer (size/hash/name/comment/tags) → instant
  search (KDX lesson); powers cross-server catalog too.
- **Transfers**: over dedicated QUIC streams with Bao verified streaming; uploads and
  downloads resumable at byte/chunk granularity; folder transfers pipelined (no
  per-item lockstep — Hotline lesson).
- **Persistent transfer queue**: client-side queue (store-client) with priorities,
  bandwidth caps, schedule windows; survives restart/reconnect and resumes
  automatically. Server-side quota + per-class rate policy.

### 9.6 Swarm distribution ("the warren")

(Adopting `05-swarm-p2p.md` design.)

- **Content addressing**: file ID = BLAKE3 root; download set = **Manifest**
  (canonical CBOR: files[], sizes, roots, 1 MiB chunk advert size); manifest hash is
  the swarm identity; shareable **rabbit link**: `rabbit://host/<manifest-hash>`.
- **Advertise-without-upload**: `AdvertiseFiles` sends *metadata only*; the server
  catalogs who holds what. Permission scopes (public / class / users / link-only)
  gate discovery; **server-signed capability tokens** gate actual serving (defense in
  depth; works through brief server outages).
- **Multi-source pulls**: chunks fetched simultaneously from peers + the server's own
  optional chunk cache (policy knob: pure coordinator / LRU cache / full mirror) +
  federated servers. Rarest-first with coordinator-annotated rarity; endgame mode.
- **Verification**: Bao verified streaming against the BLAKE3 root — any source is
  interchangeable and untrusted.
- **NAT traversal**: QUIC hole punching with server/relay fallback (iroh model);
  optional UPnP/NAT-PMP; browser peers bridge via a server WebRTC gateway (WebTorrent
  lesson: plan it day one); "server-relay only" privacy mode (only the server sees
  your IP — classic Hotline privacy).
- **Persistence**: sparse files + `.rhstate` sidecar (bitfield + Bao outboard); resume
  across reconnects; partial downloaders auto-become partial seeds; advertisements are
  soft state with TTL + re-announce.
- Fairness: no tit-for-tat — authenticated community + server slot/rate policy;
  keep rotation only for load spreading.

### 9.7 Radio ("pirate radio")

- Optional per-server feature. Multiple **stations** (mount points).
- Server-side library (or designated file area) → playlist; **listeners vote** on the
  queue (upvote tracks; requests submit tracks from the file areas); simple rotation
  when no votes. DJ mode: a privileged user streams live (source connect).
- Encoding: Opus/Ogg primary; MP3 mount for legacy player compat.
- Delivery: native = dedicated QUIC uni stream; web = HTTP streaming; legacy players =
  **Icecast/ICY** compatible mounts with `icy-metaint` metadata (§10.6).
- Now-playing surfaces everywhere: presence line, chat topic ticker, TUI status bar.
- Client setting: radio on/off, per-station, volume, ducking on notification.

### 9.8 ASCII & ANSI art

- First-class medium: `art` crate renders CP437 + ANSI (SGR, cursor, iCE colors,
  blink, ANSImation timing) to (a) real terminals (telnet/TUI passthrough or
  Unicode-mapped), (b) HTML/canvas for web/GUI, (c) PNG thumbnails for previews.
- SAUCE read/write; art gallery file areas with correct aspect/font hints
  (`IBM VGA` 9px, etc.); ANSI editor is out of scope (use Moebius etc.) but
  round-tripping is lossless (store raw bytes + decoded view — BBS lesson).
- Server identity art: logo (ANSI and/or raster), login banner, welcome screen,
  room/board headers; theme packs.

### 9.9 Welcome experience & navigation

- On connect: **agreement gate** (if configured) → **welcome screen**: server logo
  (art), MOTD, unread mail/board counts, who's online sample, featured boards/areas,
  radio now-playing, news ticker. Server admins compose this (widgets, art, text).
- **Keyword bar** (AOL): `Ctrl+K` / `/go <word>` fuzzy teleport to any room, board,
  area, station, user, or admin panel. Server-definable keywords (branded shortcuts).
- Consistent, optional **sound identity**: connect, IM, mention, mail, buddy-arrive.

### 9.10 Requests ("the wishing well")

- Request board for desired files/boards/features; statuses (open/claimed/fulfilled/
  declined); voting; claim workflow; fulfillment links the delivered file/board;
  notifications to requester; admin curation.

### 9.11 Administration

- **Everything remotable over RHP** (family 7) with per-op capability bits + audit log:
  live config editing (typed schema, validated, hot-reload where safe), account/class
  CRUD, bans, kicks (with cannot-be-kicked bit), broadcast, room/board/area management,
  transfer monitor, federation peering management, tracker registration, radio control,
  legacy-surface toggles, backup trigger, metrics view.
- Surfaces: server CLI (`rabbithole-server ctl …`), sysop TUI (connection monitor,
  server history — KDX), **web admin** (same Leptos app, admin routes), and the native
  client's admin panels (authorized users administer from any client — per prompt).
- Server history/audit: connections, transfers, admin actions, federation events.

---

## 10. Legacy Interoperability

All adapters project the same core objects; none get their own data model.

### 10.1 Telnet / ANSI BBS surface (`legacy-telnet` + `screen`)

- Telnet codec: IAC state machine, option negotiation (ECHO, SGA, BINARY both ways,
  NAWS with mid-session resize, TTYPE cycling), 0xFF doubling, loop-safe (only respond
  on state change).
- Full-screen BBS UI = ratatui widget tree rendered through a custom backend that
  serializes to the socket as CP437+ANSI (or UTF-8 for modern terminals, chosen by
  TTYPE/negotiation). Same widgets power the local TUI app — one BBS look, two doors.
- Surface: login, welcome/art, who, boards (read/post), files (browse + **Zmodem**
  transfer + HTTP link handoff), chat rooms, DMs, keyword nav.
- **Zmodem**: ZRLE/CRC32 framing, streaming with error recovery, ZRPOS resume —
  native file transfer for retro terminals (SyncTERM, NetRunner, qodem).
- **Door games**: dropfile generation (DOOR32.SYS primary; DOOR.SYS/DORINFO1.DEF
  for older doors) + a telnet/PTY bridge to spawn doors safely (no raw fd
  inheritance — the ENiGMA½ model); door menu with per-door ACLs, node numbers,
  time limits; RBAC projected to a legacy security level (0–255) + flags.
- Guest + account login; legacy surface class restrictions apply.

### 10.2 Hotline compatibility (`legacy-hotline`)

- **Server compat**: TRTP/HOTL handshake; 20-byte transaction header; field TLV with
  16/32-bit size-dependent ints; the full type table from `01-hotline.md` (login 107,
  agreement 109/121, chat 105/106, private chat 112–120, IM 108, user list 300–304,
  news 370–421 threaded set, files 200–213, accounts 348–355, keepalive 500).
- Login obfuscation (`255-b`) accepted; **Hotline passwords verified against a
  separate legacy credential** (opt-in per account) so Argon2 hashes are never
  weakened; or guest.
- HTXF transfer channel on port+1 with FFO (flattened file object: INFO/DATA forks,
  MWIN platform, no resource fork), resume via fork offsets; folder transfers with
  the per-item action lockstep.
- Threaded news mapped to boards; flat-board transactions (101/102) served as a
  projection of recent posts. User icons: numeric icon IDs mapped to a bundled classic
  set; RabbitHole avatars downscale to nearest icon.
- Pipelined/early requests tolerated (clients fire userlist/news before login reply).
- Access mask: our capability bits projected onto Hotline's 64-bit layout
  (big-endian bit order — documented once, tested).
- **Tracker compat**: `apps/tracker` also answers HTRK listing requests (5498) and
  accepts classic heartbeat registrations.

### 10.3 NNTP (`legacy-nntp`)

- RFC 3977 reader server: CAPABILITIES, GROUP/LISTGROUP, ARTICLE/HEAD/BODY/STAT,
  NEXT/LAST, POST, OVER/XOVER + LIST OVERVIEW.FMT, NEWNEWS, LIST ACTIVE/NEWSGROUPS,
  AUTHINFO USER/PASS (TLS-only) per RFC 4643; dot-stuffing both directions.
- Boards ↔ groups: per-group monotonic article numbers (separate from event IDs,
  never reused), permanent Message-IDs minted at post time, References from parent
  refs, **precomputed overview cache** (computed on post, not per request).
- Peering/syndication: IHAVE + NEWNEWS for pull/push feeds with external NNTP peers;
  dedup strictly by Message-ID through the shared dupe subsystem (§10.7).

### 10.4 FidoNet FTN (`legacy-ftn`)

- 5D addressing (`zone:net/node.point@domain`); nodelist + NODEDIFF parsing (CRC-16).
- **PKT codec**: type-2+ with fallback to type-2 (capability-word detection); packed
  message layout; kludges (INTL, FMPT/TOPT, MSGID/REPLY, PID/TID); AREA: line;
  Origin line; SEEN-BY + PATH maintenance (add self + downlinks, honor suppression).
- **Tosser/scanner/mailer pipeline** as async services: scanner (base→PKT→ARCmail
  bundle→BSO outbound), tosser (bundle→dedupe→base→forward), **binkp** mailer
  (FTS-1026 frames, port 24554), day-coded bundle naming with collision handling.
- **AreaFix**: netmail-driven subscription management (+AREA/−AREA/%LIST/%QUERY).
- Echomail ↔ boards; netmail ↔ DMs (gateway persona), configurable per-board FTN
  area tags; charset: CP437 in, UTF-8 internal, CP437 out (lossless raw retained).

### 10.5 QWK / QWKE (`legacy-qwk`)

- Packer: MESSAGES.DAT 128-byte blocks (0xE3 line ends, status flags, conference
  number at 124–125), CONTROL.DAT, per-conf NDX (**MBF float** encoder — implement,
  don't trust on read), DOOR.ID (advertise QWKE), optional bulletins; ZIP bundling.
- QWKE kludges (To:/From:/Subject: long fields) both directions.
- REP ingester: validate, dedupe, post as events with the user's identity.
- Delivery: download via any client/web (`.qwk` export), the telnet surface, and
  an automatic per-user schedule; read pointers shared with the native offline mode.

### 10.6 finger / who / Icecast / (ident)

- **finger (79)**: RFC 1288 `{Q1}` — empty query = who's-online list; `user` = profile
  card + presence + plan (user-editable `.plan` field!); `/W` verbose; **refuse
  forwarding** (`@host`); output capped; privacy-respecting (per-persona opt-out).
- **who**: covered by finger empty-query + a `who` alias on the CLI/telnet surface.
- **Icecast/ICY**: listener GET with `Icy-MetaData: 1` → `icy-metaint: 8192` inline
  metadata (length byte × 16, zero when unchanged; exact interval math); MP3 mounts
  get ICY splicing, Ogg/Opus mounts use in-stream comments (never spliced); no
  chunked encoding on ICY paths; source connect (PUT/SOURCE + Basic auth) for DJs
  using standard tools (butt, ices).
- **ident (113)**: not served (privacy). Optional outbound ident *client* annotation
  for inbound legacy sessions, cosmetic only.

### 10.7 Shared syndication infrastructure

- **Canonical message model** with per-network projections (Message-ID, FTN MSGID,
  QWK numbers, NNTP article numbers) stored beside each event.
- **Unified dupe/loop subsystem**: time-windowed seen-store keyed by every network's
  ID form + content hash; SEEN-BY/PATH logic; this is core infrastructure with real
  tests — echo storms get new nodes excommunicated (BBS lesson).
- **Lossless raw retention**: original bytes stored beside decoded text so re-emission
  round-trips exactly (kludges, CP437, padding).
- Golden-file test corpus from real Synchronet/Mystic-generated PKT + QWK packets.

---

## 11. Client & Server Frontends

| Surface | App | Notes |
|---|---|---|
| Client CLI | `apps/cli` (`rabbit`) | scripting-friendly: login, send, post, fetch, transfer, queue mgmt, JSON output |
| Client TUI | `apps/tui` | full experience in ratatui: rooms, boards, files, transfers, radio, art viewer |
| Client GUI | `apps/gui` | Tauri v2; Leptos UI; desktop macOS/Win/Linux + iOS/iPadOS/Android; native notifications, tray/menubar presence, background audio (mobile plugins) |
| Client web | served by server | same Leptos SPA over WS transport; installable PWA |
| Server daemon | `apps/server` | headless; quinn + axum + legacy listeners |
| Server CLI | `rabbithole-server ctl` | local socket or remote RHP admin |
| Server TUI | `apps/server-tui` | sysop console: monitor, history, config, moderation |
| Server web admin | served by server | same Leptos SPA, admin routes, capability-gated |
| Server native GUI | Tauri wrapper over admin routes | menu-bar/systray app bundling the daemon for "run a server from your desktop" (Hotline spirit) |
| Tracker | `apps/tracker` | native directory + legacy HTRK compat |

UI design language: **clean, minimal, robust** (per prompt) with an optional retro
theme layer (CP437 fonts, scanline accents) — modern by default, nostalgic on demand.

---

## 12. Storage & Persistence

- **Server**: SQLite via sqlx (WAL mode) behind repository traits; Postgres impl
  later if needed. In-code migrations (`sqlx migrate`). Content-addressed blob store
  on disk (`blobs/ab/cd/<blake3>`) for files/avatars/art with refcounting GC.
- **Client**: rusqlite store — session cache, offline boards, outbox, transfer queue
  + `.rhstate` sidecars, buddy list cache, prefs. Same migration discipline.
- **Backups**: online snapshot command (SQLite backup API + blob manifest);
  scheduled; restore documented and tested.
- **Metrics/observability**: `tracing` throughout; optional Prometheus exporter;
  structured audit log separate from ops log.

## 13. Packaging & Deployment

- **Server**: multi-stage Docker (cargo-chef → debian-slim/distroless), docker-compose
  example (server + tracker), raw binaries via `dist` (cargo-dist), systemd unit,
  one-line install script. Config: TOML file + env overrides + `ctl` editing.
- **CLI/TUI**: `dist` archives, Homebrew tap, MSI/shell installers.
- **GUI**: Tauri bundler — dmg/app, msi/NSIS, deb/rpm/AppImage; iOS via App Store
  Connect/TestFlight; Android via Play (.aab). Budget: signing, notarization,
  background-audio entitlements, privacy manifests.
- **CI**: GitHub Actions — fmt/clippy/test matrix (Linux/mac/Win), wasm build, mobile
  cross-compile smoke (early — the NDK/C-deps pain is front-loaded by design: pure-Rust
  deps preferred: rustls, symphonia, redb-or-bundled-sqlite).

## 14. Testing Strategy

- **Protocol**: golden-transcript tests for RHP; fuzz the frame decoder (cargo-fuzz).
- **Legacy codecs**: byte-exact round-trip suites (PKT, QWK+MBF, SAUCE, telnet IAC,
  Hotline transactions, FFO); corpus from real generators; fuzz all decoders.
- **Compat**: integration tests driving real third-party clients where scriptable —
  Hotline (mobius client lib), NNTP (`tin`/python nntplib), QWK readers, telnet
  (expect scripts), Icecast (mpv/ffprobe headers).
- **Swarm**: multi-peer simulation harness (in-process peers, lossy links, NAT sim);
  resume/corruption injection (bitfield lies, bad chunks).
- **Federation**: three-server testnet in CI; partition/rejoin; dupe-storm tests.
- **Core**: property tests for the permission evaluator (deny-wins invariants);
  offline sync conflict tests.
- **E2E**: Playwright against the web client; Tauri driver smoke on desktop.

---

## 15. Waves & Dependency Graph

Legend: each wave lists **[depends on]**. Waves overlap where dependencies allow;
tracks (client surfaces, docs, CI) run continuously.

```
W0 ─▶ W1 ─▶ W2 ─▶ W3 ─▶ W4 ─▶ W5(swarm)
            │      │      ├──▶ W6(telnet)──▶ W7(hotline)
            │      │      │
            │      └──────┴──▶ W8(web/GUI) ─▶ W12(mobile)
            │             │
            │             ├──▶ W9(federation+discovery)
            │             │        └──▶ W10(syndication: NNTP→FTN→QWK)
            │             └──▶ W11(radio)
            └────────────────────────────▶ W13(E2EE, moderation+, polish, 1.0)
```

### Wave 0 — Foundations
*[depends on: —]*
- Workspace scaffold (all crates/apps stubbed), CI (fmt/clippy/test/wasm), licensing (dual MIT/Apache-2.0), CONTRIBUTING, rustfmt/clippy config.
- `proto`: RHP frame, families, versioning, postcard framing, error model; protocol doc skeleton (`docs/protocol/`).
- `identity`: Ed25519 keys, signing, Argon2id, tokens, TOTP.
- `net`: Transport trait; quinn impl; tungstenite impl; rustls/ACME plumbing.
- Storage traits + sqlx/rusqlite skeletons + migration harness; blob store.
- `server-core`/`core` skeletons with Command/Event API + event bus.

### Wave 1 — Vertical slice: a server you can talk to
*[W0]*
- Server daemon: QUIC+WS listeners, Hello/auth (password, guest, token resume), sessions, keepalive, reconnect resume.
- Roles/classes/bitmask + ACL evaluator (cached, property-tested).
- Presence registry; who's online.
- Public chat (rooms CRUD minimal: one lobby), server agreement + MOTD.
- `rabbit` CLI: login, who, chat, admin ping. `ctl` local admin (config get/set, account create).
- Config system (TOML + env + hot-reload where safe); tracing/audit skeleton.

### Wave 2 — Community layer
*[W1]*
- Accounts UX: registration gating, TOTP, key enrollment; classes admin.
- Personas (multi per account), profiles, avatars/banners (blob store), member directory.
- Buddy lists (server-stored, groups, permit/deny), full presence states + pushes.
- Multiple/ad-hoc/private chat rooms, invites, moderation ops.
- DMs: threads, offline queueing, attachments (size-capped), receipts, notifications.
- Welcome screen composition + keyword registry; sounds (client-side).
- TUI client v1 (login, chat, who, DMs); shared `screen` crate begun.
- Server TUI v1 (monitor, config, accounts); remote admin family live.

### Wave 3 — Message bases & offline
*[W2]*
- Board hierarchy, threaded posts as **signed blake3 events** (federation-ready), ACLs, moderators, retention/archive.
- Read pointers; unread counts in welcome/keyword surfaces.
- Client offline store: batch subscribe/download, offline read/reply, outbox sync.
- Request system ("wishing well") with voting + claims.
- Dupe/seen subsystem (shared, tested) — foundation for W9/W10.
- CLI/TUI board reading + posting.

### Wave 4 — File libraries & transfers
*[W3 (ACL evaluator, events for audit)]*
- Areas/folders/files with metadata, icons, comments, attribution, counters, ratings; aliases; drop boxes; hide-vs-deny ACLs.
- Blob-backed storage + background **file indexer** + instant search.
- Transfer engine: QUIC streams, Bao verified streaming, byte-resume, folder pipelining, quotas, rate classes.
- **Persistent client transfer queue** (priorities, schedules, auto-resume).
- CLI/TUI file browsing + transfers.

### Wave 5 — Swarm ("the warren")
*[W4]*
- Manifests + rabbit links; AdvertiseFiles (list-without-upload) + TTL soft state.
- Coordinator: FindSources, announce, rarity annotation; capability tokens.
- Peer wire over QUIC; multi-source scheduler (rarest-first, endgame); server chunk-cache policies; relay fallback + hole punching (iroh or quinn+custom — spike first); `.rhstate` resume; partial-seed behavior.
- Federated sources hook (consumed fully in W9).
- WebRTC gateway for browser peers (may slip to W8 alongside web client).

### Wave 6 — Telnet BBS + finger + art pipeline
*[W2 (chat/DM), W3 (boards); W4 optional for file menus]*
- `art` crate: CP437 tables, ANSI/SGR parser+renderer, SAUCE, ANSImation; PNG thumbnailer.
- `screen` crate: ratatui→socket backend (CP437/ANSI + UTF-8 modes).
- Telnet codec + negotiation state machine; full-screen BBS: login, welcome art, who, boards, chat, DMs, keyword nav; file browse + HTTP-link handoff.
- **Zmodem file transfer** on the telnet surface (download first, then upload): ZRLE/CRC32 framing, streaming with error recovery, resume (ZRPOS) — so real retro terminals (SyncTERM, NetRunner, qodem) transfer files natively.
- **Door games**: external program support via dropfiles (DOOR32.SYS primary; DOOR.SYS/DORINFO1.DEF for older doors) with a **telnet/PTY bridge** (no raw fd inheritance — portability + safety, the ENiGMA½ model); per-door config, node numbers, time limits; legacy **security-level projection** (RBAC → 0–255 SL + flags) so doors get sane values.
- finger server (who + profile + .plan); presence projections.

### Wave 7 — Hotline compatibility
*[W2, W3, W4 (all the mapped features); W6's lessons on legacy auth]*
- Transaction codec + field TLV; login (obfuscation, legacy credential opt-in), agreement/banner, user list + icons, public/private chat, IM, threaded news mapping, flat-news projection.
- File browsing + HTXF transfer channel (FFO, forks, resume, folder lockstep).
- Account admin transactions; access-mask projection; kick/ban.
- Tracker app: native registry **+ HTRK legacy listing/registration**.
- Compat test rig against archived Hotline clients + mobius.

### Wave 8 — Web & desktop GUI
*[W2–W4 for feature surface; W5 for transfer UI; parallel with W6/W7]*
- `ui-web` Leptos SPA: auth, welcome, rooms, DMs, boards, files (upload/download via WS+fetch), transfers, member directory, profiles, keyword bar, art rendering (canvas), themes (clean default + retro).
- Served embedded web client + **web admin** routes (config, accounts/classes, moderation, monitors, federation & radio panels as those land).
- Tauri v2 desktop shell: core linked in-process, QUIC transport, native notifications, tray presence, deep links (`rabbit://`), auto-update.
- Server native GUI wrapper (menubar/systray + bundled daemon).

### Wave 9 — Federation & discovery
*[W3 (events, dupe store), W4/W5 (catalogs/swarm), W8 (admin UI helpful)]*
- S2S QUIC channel + server key exchange; peering handshake + admin approval flow.
- Board flood-fill: subscriptions, ihave/pull, seen-set, tombstones, per-peer reputation/rate limits, defederation.
- Cross-server identity attestation; `persona@server` display + verification.
- Cross-server file search (signed catalogs, pull fan-out, dedupe by hash); swarm federated sources live.
- `.well-known/rabbithole/server`; tracker service full (signed descriptors, heartbeats, categories, gossip); directory index + health; client server-browser UI.

### Wave 10 — Syndication (NNTP → FTN → QWK)
*[W3 (boards, dupe subsystem); W9 helpful but not required]*
- **NNTP server** (reader + AUTHINFO/TLS + overview cache) and peering (IHAVE/NEWNEWS) against external Usenet peers.
- **FTN**: PKT codec + golden tests; tosser/scanner; binkp mailer; BSO outbound; ARCmail; AreaFix; nodelist/NODEDIFF; echomail↔boards, netmail↔DMs gateway.
- **QWK/QWKE**: packer/NDX(MBF)/CONTROL/DOOR.ID; REP ingest; scheduled per-user packets; telnet + web + CLI delivery.
- Syndication admin UI: per-board network mappings, feeds monitor, dupe stats.

### Wave 11 — Radio
*[W1 (sessions); W4 (library); W8 for UI polish]*
- Station/mount model; playlist engine + **vote queue** + requests from file areas; DJ live-source (Icecast source protocol).
- Opus/Ogg + MP3 encode pipelines; native QUIC delivery; HTTP/ICY mounts with exact metaint math; listener counts in presence.
- Client players: GUI/web (rodio/web audio), TUI (now-playing + external player handoff), telnet (now-playing line).

### Wave 12 — Mobile & distribution
*[W8; audio plugins need W11]*
- Tauri iOS/iPadOS + Android: mobile plugin glue (notifications, background audio, share sheet), transport resilience (connection migration), store packaging, TestFlight/Play tracks.
- `dist` release automation for CLI/TUI/server; Docker images + compose; install docs; versioned protocol docs published.

### Wave 13 — Hardening & 1.0
*[everything]*
- E2EE DMs (X25519 + Double Ratchet via vodozemac, sealed sender), key backup UX.
- Moderation suite completion: report queues, quarantine, shared blocklists/hash-deny lists, audit UI.
- Rate limiting/gating everywhere (governor), mCaptcha option, invite trees.
- Security review pass (RUSTSEC audit, fuzz coverage goals, pen-test checklist), performance pass (load harness: 10k sessions/server target), accessibility pass, i18n scaffolding.
- 1.0: docs site, sample "flagship" community server config, migration/backup guides.

---

## 16. Open Questions & Defaults

Flagged for review — each has a working default so nothing blocks:

| # | Question | Status |
|---|----------|--------|
| 1 | Project license | **OPEN — owner deciding.** Candidates: MIT/Apache-2.0 dual (max adoption), AGPL-3.0 server + MIT clients (prevents closed server forks), MPL-2.0 (file-level middle ground) |
| 2 | Default native port | **DECIDED: 4653** — spells H-O-L-E on a phone keypad; web/WS 4654, tracker 4655 (IANA-unassigned range, all configurable) |
| 3 | iroh vs hand-rolled quinn for swarm NAT layer | **Spike both in W5** (rationale in PLAN review thread); default iroh if its endpoint model coexists with our quinn listener |
| 4 | Zmodem on the telnet surface | **DECIDED: yes** — in Wave 6 |
| 5 | Door-game support (DOOR32.SYS + telnet/PTY bridge) | **DECIDED: yes** — in Wave 6 |
| 6 | Persona cap per account | 5 (server-configurable) |
| 7 | E2EE DM timing | Default W13 (rationale in PLAN review thread) — owner may pull earlier |
| 8 | Postgres support | Behind repository trait; implement when a deployment needs it |
| 9 | Server-hosted IRC bridge (KDX had one) | Out of scope for 1.0; protocol families leave room |
| 10 | Matrix/ActivityPub bridges | Out of scope for 1.0; federation model doesn't preclude adapters |

---

*Plan version 1.0 — 2026-07-01. Companion tracker: `TODO.md`. Research: `docs/research/`.*
