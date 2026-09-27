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

Inbound connections consume the shared per-IP **CONN** budget once when the
QUIC listener hands over a connection, before spawning its application
handshake. The key is the actual remote IP, independent of source port or any
claimed peer identity. An exhausted budget drops the connection without a
HelloAck; it does not wait for peer acknowledgement in the accept loop.
`ratelimit_conn_per_min` and `ratelimit_conn_burst` are read live. The master
`ratelimit_enabled = false` switch or a connection rate of `0` disables this
gate; a zero burst with a nonzero rate refuses admission. Tokens refill, and
unrelated IPs keep separate budgets. Peers sharing an IP also share CONN with
the other server listeners.

This admission charge includes valid, pending and incomplete application
handshakes. It is separate from the failures-only AUTH budget below. Outbound
dialing is unchanged. QUIC's existing TLS/control-stream handshake limits
run before this application admission gate.

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
| 12 | CatalogSyncSupport | dialer probe / listener reply | `catalog-sync-v1` capability string; correlated nonzero request ID |
| 13 | LiveCatalogAnnounce | both, after negotiation | `catalog_id`, `generation`; push with ID zero |
| 14 | LiveCatalogGet | both, after negotiation | empty request; nonzero request ID |
| 15 | LiveCatalogReply | both, after negotiation | signed catalog bytes or an error; echoes the request ID |

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

Inbound peering uses the shared **AUTH** failure budget for the remote IP
(`ratelimit_auth_per_min` and `ratelimit_auth_burst`). A non-consuming probe
runs before the exchange and again after each received handshake message,
before challenge signing or proof verification. An exhausted bucket closes
that connection without a Welcome; the initial gate sends no HelloAck.
Malformed authentication frames, unsupported federation versions, invalid
origin claims and invalid signatures each charge one failed attempt.
Transport disconnects and local I/O failures do not. Successful proofs spend
nothing, including valid peers waiting for approval; later authorization or
session failures also spend nothing. Approval never bypasses the gate because
the announced key is unproved until the signature is checked.

The policy is read live. `ratelimit_enabled = false` or
`ratelimit_auth_per_min = 0` disables this gate; a zero burst with a nonzero
rate refuses all attempts. Tokens refill normally, and other remote IPs retain
their own budgets. Peers sharing an IP share its AUTH budget with the other
login surfaces. This gate does not replace QUIC transport bounds or limit
post-authentication traffic, and does not change the separate native
grant-pull handshake.

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
- A configured outbound peer is implicitly approved by `federation_peers`.
  Remove it from configuration and restart before running `peer-revoke`; the
  command refuses a contradictory revoke that the dialer would immediately undo.

A background dialer re-checks configured `federation_peers` every 30 s and
redials any without a live session.

## Catalog sync

The initial exchange remains **dialer-pull** for compatibility. After
`Welcome { connected: true }`:

1. The dialer sends `CatalogAnnounce` for its local catalog; the listener
   answers with its own id/generation.
2. If the announced generation is fresher than what the dialer holds for
   this peer, it sends `CatalogGet`; the listener replies `Catalog` with the
   `SignedCatalog` bytes.
3. The dialer verifies the catalog against the peer's **pinned key** — the
   Ed25519 key the handshake just proved, not any key named inside the
   bytes — plus generation staleness, before storing it.

After this exchange, a supporting dialer sends a `catalog-sync-v1` probe.
Only a matching capability reply enables live synchronization. Older listeners
ignore the additive probe; newer listeners send no extension traffic to an
older dialer. Protocol-2 authentication and the initial sync remain unchanged.

When negotiated, both endpoints check local catalog content every 30 seconds
and announce changed IDs on the existing connection. This also catches moves,
deletions and moderation changes that do not emit file-added events. Both
sides can request a newer catalog without a reciprocal dial. Only local file
catalogs are advertised; this remains one-hop exchange, with no transitive
relay of cached peer catalogs.

Each direction permits one outstanding fetch. Replies must have the expected
message type, reply kind and exact nonzero request ID; wrong, unsolicited or
expired IDs leave the active fetch untouched. Probe/fetch deadlines are 15
seconds, and fetch attempts are spaced at least 30 seconds apart. Control
payloads are capped at 128 bytes and catalog replies at 4 MiB including their
encoded payload envelope. A signed reply at the announced generation must
match its announced ID; a newer signed generation is also accepted. Signature,
approval-revision and replay checks still run before persistence.

Local catalog/cache failures and explicit fetch refusals leave board traffic
usable and retry at the next interval. Invalid peer envelopes, signatures or
authorization can close the connection. Cross-server search runs locally over
the verified stored catalogs (`ctl fed-search`); a client-facing RHP search
over federated catalogs is a follow-up.

### Peer cache and restart behavior

Verified peer catalogs are saved in
`<data_dir>/federation/peer_catalogs.bin`. The versioned snapshot is signed by
this burrow's identity and replaced atomically after a newer catalog verifies.
A failed cache write leaves the previous accepted generation in place.

On startup, the burrow loads approved peers first, authenticates the cache,
reverifies each retained catalog, and exposes only payloads whose origin/key
pair still matches a current approval. Revocation hides a peer immediately;
reapproval must receive a strictly newer generation, including after restart.
Eviction removes payloads while retaining their generation watermarks.

The cache retains at most 64 payloads totaling 32 MiB, with a 4 MiB limit per
catalog. Older accepted payloads are evicted first. At most 4,096 peer keys
can hold watermarks or pending fetch slots; catalog fetches for additional keys
are refused instead of discarding replay protection. Peering connections can
remain active when catalog sync is refused. A corrupt, oversized, unsupported,
or differently signed cache is ignored and logged, so its payloads are never
served. Its watermarks cannot be recovered from that invalid snapshot;
subsequent approved-peer pulls rebuild the cache.

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
key over `rhp-fed-descriptor-v2 ‖ postcard(body)`. Descriptor v2 adds the
immutable signed `origin`; v1 bodies are not decoded or accepted as v2. Anyone
can fetch it and
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

## Periodic board history

Approved live sessions reuse `Subscribe` / `IHave` / `Pull` / `Events` for
history recovery; there are no new wire messages or protocol negotiation.
`federation_board_subscribe` is a TOML array of board slugs (`"*"` or `"all"`
means all). A history offer requires both this burrow's local opt-in and the
connected peer's subscription, a current postable board, and no quarantine on
the event's target or thread root. Approval is checked again after database
reads, before sending. Empty local subscriptions disable history offers.
This setting expresses subscription interest, not an authorization boundary
for incoming event ingestion.

`federation_history_reoffer_secs` is live-editable with `ctl config set`:

| Value | Behavior |
|---|---|
| `60` (default) | One bounded history opportunity per approved peer each minute |
| `5`–`3600` | Custom interval in seconds |
| `0` | Disable subsequent periodic passes; subscription-triggered catch-up remains bounded by a 60-second peer cooldown |

The live setter rejects other values; nonzero values read from TOML are
clamped to 5–3600 seconds. A delayed task performs one pass, without accruing
a burst of missed intervals. Subscription repeats and reconnects cannot reset
the shared peer cooldown. Initial catch-up uses the same budget as periodic
work, so large histories converge over multiple passes.

Each pass returns/processes at most 256 metadata candidates from a keyset
union of stored posts and edit/tombstone follow-ups, then offers at most 128
IDs in at most eight board-scoped frames. Each table contributes at most 256
rows to that merged query; this is not a total database scan-cost guarantee.
Positions use `(event_id, event-kind)` rather than peer-supplied timestamps.
The cursor advances over filtered candidates and wraps after reaching the
end. Live Bloom seen filters never suppress these recovery offers. Receivers
check both durable event tables as well as their duplicate window, so repeated
history does not produce repeated post or follow-up pushes. A failed or
cancelled ingest releases its temporary duplicate reservation for later retry.
Successful duplicate records still protect retention-pruned events from
immediate resurrection.

A peer has one shared cooldown and round-robin turns across at most eight
registered live links; each link keeps its own cursor for its own subscription.
At most 4,096 peer scheduling records are retained. Active records are never
evicted; disconnected records expire after an hour. Excess peers/links keep
their existing live traffic but receive no history scheduling slot. Local
history-query failures keep the session alive and retry after the normal
cooldown. Cursors are in memory and reset on a new link or server restart.
Recovery covers events still retained by an eligible connected peer; it does
not restore content every peer has already deleted.

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
