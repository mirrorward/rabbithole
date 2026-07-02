# RHP Federation Family (8) — Tunnels (S2S)

Status: **Wave 9** — the mutually-authenticated peering handshake with admin
approval and signed-catalog sync are on the wire (`apps/server/src/federation.rs`).
The rest of the federation data model (descriptors, board flood-fill,
redactions, attestations) is implemented and tested in `crates/federation`
but **not yet carried by this transport** — the last section marks what
rides the wire today vs what is model-only.

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
  (`key`; empty = accept whatever key the handshake proves).
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
(`FED_PROTOCOL = 1`):

| type | name | direction | payload |
|---|---|---|---|
| 1 | Hello | dialer → listener | `hello: PeerHello` {`server_key: [u8;32]`, `server_name`, `protocol_version: u32`, `software`}, `nonce: [u8;32]` |
| 2 | HelloAck | listener → dialer | `ack: PeerHelloAck` {same fields + `accepted: bool` (advisory approval verdict for the claimed key)}, `nonce: [u8;32]`, `proof: Signature` |
| 3 | Proof | dialer → listener | `proof: Signature` |
| 4 | Welcome | listener → dialer | `connected: bool` — sent *after* the listener's registry is updated, so the dialer has a deterministic readiness signal |
| 5 | CatalogAnnounce | both, post-welcome | `catalog_id: [u8;32]`, `generation: u64` — "my current catalog", cheap staleness check |
| 6 | CatalogGet | dialer → listener | — (empty) request the full signed catalog |
| 7 | Catalog | listener → dialer | `bytes: Vec<u8>` — a `SignedCatalog` in its postcard wire form; verified before a byte of it is trusted |

`PeerHello`/`PeerHelloAck` are the `crates/federation::handshake` types.

## Handshake: nonce-bound challenge-response

Both sides sign the same transcript with their Ed25519 server identity key
(domain separator `rhp-fed-s2s-auth-v1`):

```text
transcript = "rhp-fed-s2s-auth-v1" ‖ dialer_key ‖ listener_key
             ‖ dialer_nonce ‖ listener_nonce
```

1. Dialer connects (fingerprint-pinned TLS), sends `Hello` with a fresh
   32-byte random nonce.
2. Listener replies `HelloAck`: its announcement, its own fresh nonce, and
   its signature over the transcript. The dialer verifies it against the
   announced key (and against `expected_key` when configured — a mismatch
   aborts).
3. Dialer sends `Proof` — its signature over the same transcript. The
   listener verifies it against the dialer's announced key.
4. Listener sends `Welcome { connected }`.

Each proof demonstrates **live possession** of the announced identity key,
and the nonces bind the proof to *this* connection — a captured proof cannot
be replayed on another session.

## Admin approval: pending / approved peers

A new peer key is **never trusted automatically**:

- An inbound handshake from an unknown key authenticates, is recorded
  `PeerState::Pending` in the `PeerRegistry`, receives
  `Welcome { connected: false }`, and the connection closes.
- An admin approves the key (audited, via the `ctl` peer commands). Approved
  keys persist to `<data_dir>/federation/approved_peers.json` and reload on
  boot. A subsequent handshake then transitions to `PeerState::Connected`.
- **Dialing implies approval on the dialer's side** (the operator configured
  the peer in `federation_peers`); the listener still approves the dialer
  independently.
- Approval is re-checked at every catalog fetch, so revoking a peer
  mid-session stops serving it.

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

## Model-only today (implemented in `crates/federation`, not on this wire)

These are pure, tested data models awaiting a transport slice. Nothing
below is exchanged between servers yet.

- **`PeerDescriptor`** — the self-certifying
  `.well-known/rabbithole/server` document (identity key, name, addresses,
  feature tags, `issued_at`), Ed25519-signed over `rhp-fed-descriptor-v1`.
  Not yet served or fetched.
- **Board flood-fill** (`floodfill`) — `Subscription`, `IHave` / `PullRequest`
  / `PushEvents` moving signed board events between peers unchanged, gated
  by a Bloom-filter seen-set (`bloom`) against re-ingest loops.
- **Redactions** (`redaction`) — server-sovereign tombstone/redact
  propagation.
- **Ingest defense** (`policy`) — per-peer token-bucket `RateLimiter` and
  allow/deny `PeerPolicy`.
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
