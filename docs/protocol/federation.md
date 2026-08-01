# RHP Federation Family (8) — Tunnels (S2S)

Status: **Wave 9** — mutually authenticated peering, signed-catalog sync, and
signed board-event flood-fill are on the wire (`apps/server/src/federation.rs`).
Federation protocol v2 binds an immutable origin namespace to the handshake
key and requires independently established provenance before accepting a
relayed origin. The last section separates the remaining model-only pieces.

Family 8 is **server-to-server only**. It is never spoken on a client
connection; a client sending family-8 frames gets `Unsupported` like any
other unknown message.

## Transport

- A **dedicated QUIC endpoint** bound to `federation_addr` (default
  `0.0.0.0:4655`), separate from the client QUIC (4653) / WebSocket (4654)
  listeners. Opt-in via `federation_enabled` (default **off**).
- Same TLS identity and ALPN (`rhp/1`) as the client transport; the dialer
  **pins the peer's certificate blake3 fingerprint** (from the peer entry's
  `fingerprint`) and may additionally pin the expected Ed25519 server key
  (`key`). Every configured target also declares its immutable expected
  `origin`.
- Messages are ordinary RHP `Frame`s with `family = 8`; the request `id` is
  always 0 (the exchange is strictly sequenced, not pipelined). Isolation is
  by port **and** by family: a non-federation frame on the S2S channel kills
  the session.
- Bounds: handshake payloads are capped at **64 KiB** (`MAX_MSG`); the
  full-catalog reply at **4 MiB** (`MAX_CATALOG`). Oversized payloads end
  the session.
- Unknown federation message types received post-welcome are **ignored**
  (forward compatibility), not errors.

## Messages

Message-type constants from `apps/server/src/federation.rs`
(`FED_PROTOCOL = 2`):

| type | name | direction | payload |
|---|---|---|---|
| 1 | Hello | dialer → listener | `hello: PeerHello` {`server_key: [u8;32]`, `origin`, `server_name`, `protocol_version: u32`, `software`}, `nonce: [u8;32]` |
| 2 | HelloAck | listener → dialer | `ack: PeerHelloAck` {same fields + `accepted: bool` (advisory verdict for the claimed origin/key tuple)}, `nonce: [u8;32]`, `proof: Signature` |
| 3 | Proof | dialer → listener | `proof: Signature` |
| 4 | Welcome | listener → dialer | `connected: bool` — sent *after* the listener's registry is updated, so the dialer has a deterministic readiness signal |
| 5 | CatalogAnnounce | both, post-welcome | `catalog_id: [u8;32]`, `generation: u64` — "my current catalog", cheap staleness check |
| 6 | CatalogGet | dialer → listener | — (empty) request the full signed catalog |
| 7 | Catalog | listener → dialer | `bytes: Vec<u8>` — a `SignedCatalog` in its postcard wire form; verified before a byte of it is trusted |
| 8 | Subscribe | both, post-welcome | bounded board slugs (or `*`) this peer wants |
| 9 | IHave | both, post-welcome | bounded signed-event ids available for a board |
| 10 | Pull | both, post-welcome | bounded signed-event ids requested for a board |
| 11 | Events | both, post-welcome | signed events plus parallel origin-key selectors; selectors never establish trust |

`PeerHello`/`PeerHelloAck` are the `crates/federation::handshake` types.

## Handshake: nonce-bound challenge-response

Both sides sign the same transcript with their Ed25519 server identity key
(domain separator `rhp-fed-s2s-auth-v2`; strings are length-prefixed):

```text
transcript = "rhp-fed-s2s-auth-v2" ‖ dialer_key ‖ listener_key
             ‖ len(dialer_origin) ‖ dialer_origin
             ‖ len(listener_origin) ‖ listener_origin
             ‖ dialer_nonce ‖ listener_nonce
```

1. Dialer connects (fingerprint-pinned TLS), sends `Hello` with a fresh
   32-byte random nonce.
2. Listener replies `HelloAck`: its announcement, its own fresh nonce, and
   its signature over the transcript. The dialer verifies it against the
   announced key, the configured `expected_origin`, and `expected_key` when
   configured; any mismatch aborts.
3. Dialer sends `Proof` — its signature over the same transcript. The
   listener verifies it against the dialer's announced key.
4. Listener sends `Welcome { connected }`.

Each proof demonstrates **live possession** of the announced identity key and
binds the immutable origin to it. The nonces bind the proof to *this*
connection, so a captured proof cannot be replayed on another session.

`federation_origin` is TOML-only, restart-only, and required whenever
federation is enabled. The hot-reloadable display `name` does not change the
origin used in newly signed events.

## Admin approval: pending / approved peers

A new peer origin/key tuple is **never trusted automatically**:

- An inbound handshake from an unknown tuple authenticates, is recorded
  `PeerState::Pending` in the `PeerRegistry`, receives
  `Welcome { connected: false }`, and the connection closes.
- An admin approves the exact tuple with `ctl peer-approve KEY [ORIGIN]`.
  Approved tuples persist in versioned
  `<data_dir>/federation/approved_peers.json` and reload on boot. A subsequent
  matching handshake transitions to `PeerState::Connected`.
- **Dialing implies tuple approval on the dialer's side**: the operator
  configured `origin` and the TLS/key pins in `federation_peers`. The listener
  still approves the dialer independently.
- Approval is re-checked before every post-welcome frame and on a one-second
  lifecycle tick. `peer-revoke` therefore stops ingestion and closes an active
  session rather than waiting for reconnect.

A background dialer re-checks configured `federation_peers` every 30 s and
redials any without a live session.

## Catalog sync

Sync is **dialer-pull**. After `Welcome { connected: true }`:

1. The dialer sends `CatalogAnnounce` for its local catalog; the listener
   answers with its own id/generation (it does not fetch back on this
   connection — it pulls the dialer's catalog when it dials back itself).
2. If the announced generation is fresher than what the dialer holds for
   this peer, it sends `CatalogGet`; the listener replies `Catalog` with the
   `SignedCatalog` bytes.
3. The dialer verifies the catalog against the peer's **pinned key** — the
   Ed25519 key the handshake just proved, not any key named inside the
   bytes — plus generation staleness, before storing it.

Sync failure is non-fatal (the peering session stands; the next dial
retries). Cross-server search runs locally over the verified stored
catalogs (`ctl fed-search`); a client-facing RHP search over federated
catalogs is a follow-up.

### SignedCatalog semantics

From `crates/federation::catalog`, signature domain `rhp-fed-catalog-v1`:

- `Catalog` (the signed body): `server_key: [u8;32]` (stamped from the
  signing key — self-certifying), `generation: u64` (monotonic; higher =
  strictly newer), `prev_id: option<[u8;32]>` (the previous generation's
  `catalog_id`; `None` = genesis), `issued_at` (unix ms), `entries`.
- `CatalogEntry`: `name`, `size`, `hash: [u8;32]` (blake3 — the cross-server
  dedupe key), `area`, `path`, `mime`, `timestamp`.
- `catalog_id = blake3(postcard(catalog))` — content-addressed; entry order
  is part of the canonical bytes.
- `verify(pubkey)` requires the supplied key to equal `catalog.server_key`
  **and** the Ed25519 signature over `context ‖ postcard(catalog)` to check.
- **Staleness / generation chain**: `a.supersedes(b)` iff same `server_key`,
  `a.generation > b.generation`, and `a.prev_id == b.catalog_id()` — a
  higher generation with a broken back-link is not a valid successor.

## Discovery: `.well-known/rabbithole/server`

A burrow with the HTTP surface enabled (`http_enabled`) serves its
self-certifying **`PeerDescriptor`** as JSON at
`/.well-known/rabbithole/server` (`apps/server/src/well_known.rs`). The body
is built from config — immutable federation origin, display name, advertised `scheme://host:port`
endpoints (`quic`, `ws`, and `http`/`fed+quic` when enabled), feature tags per
enabled surface, and a unix-ms `issued_at` — signed with the burrow's identity
key over `rhp-fed-descriptor-v1 ‖ postcard(body)`. Anyone can fetch it and
verify that the document is self-consistent: the signature is checked against
the key the document names. Self-signature alone is **not** proof that the key
owns the claimed origin; authoritative HTTPS retrieval or explicit operator
approval is still required before installing an origin binding.

The host of each advertised endpoint comes from the TOML-only `advertise_host`
config; with it unset, a concrete bind IP is used and wildcard (`0.0.0.0`/`::`)
binds contribute no host-based address (the fetcher already knows the host it
dialed). JSON is the transport convention here; because the signature covers
the **postcard** encoding of the body, the same descriptor verifies whether it
arrives as `.well-known` JSON or postcard over a tracker/S2S relay. Automated
peer fetch + authoritative consumption of the descriptor is the remaining
half.

## Origin provenance and relayed events

`MT_EVENTS` carries an origin key next to each signed event, but that key is
only a selector for an existing trusted binding. It cannot create or change
authority. A direct, approved peer installs its own proven origin/key tuple;
an operator can pre-anchor an indirect origin with `ctl origin-pin ORIGIN KEY`.
This permits A to accept C's events relayed through B without an A–C peer
session, while preventing B from minting an unseen `C` origin.

Bindings persist in the versioned
`<data_dir>/federation/origin_keys.json`. Installation is serialized and
persisted before visibility; conflicts, aliasing one key across origins, cap
exhaustion, corrupt/legacy state, and write failure all fail closed. The old
unversioned first-seen files and key-only peer approvals are intentionally not
promoted: after upgrading, operators must confirm/re-pin origins and re-approve
peer tuples before their events are accepted.

Server-key rotation is not implicit. The current store retains one authorized
key per origin and rejects a replacement. A future protocol must carry an
old-key-authorized, monotonic successor chain and retain historical keys; lost
key recovery requires an explicit audited operator procedure, never
first-seen network input.

Protocol-v1 peers do not get a permissive compatibility path. They can be
upgraded and re-approved, but v1 traffic cannot ingest board events under the
v2 provenance rules.

## Model-only today (implemented in `crates/federation`, not on this wire)

These are pure, tested data models awaiting a transport slice. Nothing below
is exchanged between servers yet.
- **Redactions** (`redaction`) — the *cross-community* server-sovereign
  redaction signal ("I no longer serve this hash"), still model-only. (Board
  **Edit/Tombstone follow-ups now flood live** over `MT_IHAVE`/`MT_PULL`/
  `MT_EVENTS` as signed events, served from `board_followups`, gated by the
  author-or-home-server authorization check in `BoardService::ingest_event`
  and reconciled when they arrive before their target post — see
  `docs/design/board-followup-flood.md`.)
- **Additional ingest defense** (`policy`) — per-peer token-bucket
  `RateLimiter` and allow/deny `PeerPolicy`; origin provenance and signatures
  are enforced live, while reputation and automatic defederation remain
  model-only.
- **Search / dedupe / fan-out** (`search`, `dedupe`, `fanout`) — these *run*
  today, but locally over stored catalogs; no query travels between servers.

### The attestation model (`attestation`)

Cross-server identity, model-only:

- **Addressing**: `persona@server` (`FedAddress`). Both parts are lowercase
  ASCII alphanumerics plus `-`/`_`/`.`, starting and ending alphanumeric;
  persona ≤ 64 bytes, server ≤ 253. The parser is total (errors, never
  panics).
- **`PersonaAttestation`**: the home server's signed statement binding a
  persona name to a persona-held Ed25519 key — `persona_name`,
  `persona_key`, `home_server_key` (stamped, self-certifying), validity
  window `[issued_at, expires_at)` in unix ms, `generation` (starts at 0,
  +1 per rotation), optional `rotation`. Signed over
  `rhp-fed-attestation-v1`. Freshness is checked against a caller-supplied
  clock — no ambient time.
- **Continuity chains** (`ContinuityChain`): one attestation per generation,
  oldest first. Every non-genesis link must carry a `KeyRotation` — the
  *previous* persona key's signature (domain `rhp-fed-rotation-v1`) over a
  statement binding persona name, home server key, the new key, and the
  target generation. `verify` checks: every link's server signature, one
  persona throughout, generations increase by exactly 1, every rotation's
  `new_key` matches the link's attested key and its `prev_sig` verifies
  under the previous link's key, and the **latest** link is fresh
  (historical links may have lapsed). This means a home server can never
  silently swap a persona's key: rotations require the outgoing key's
  consent.
- **Visitor challenges**: when `alice@a.example` knocks on server B, B mints
  ≥ 16 fresh random bytes (32 recommended); the visitor answers with their
  chain plus `sign_challenge` — the current persona key's signature over
  `rhp-fed-visitor-challenge-v1 ‖ challenge`. `verify_visitor` is the pure
  check B runs: chain valid, latest attestation fresh, challenge signature
  under the attested key. No RHP messages carry these bytes yet.
