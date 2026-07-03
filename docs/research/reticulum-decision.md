# Decision record — Reticulum integration strategy (Wave 14 spike)

> **Spike:** community `reticulum-rs` maturity vs a Python RNS gateway sidecar
> vs an in-repo pure-Rust sans-IO model crate → **decision**.
>
> **Status:** Accepted (spike closed). **Scope:** Wave 14 ("Reticulum &
> off-grid mesh", post-1.0 / 1.1). **Supersedes:** the open `[ ] Spike` line in
> `TODO.md` §Wave 14.

## Context

Wave 14 wants a Burrow to be reachable over the [Reticulum](https://reticulum.network)
(RNS) mesh — delay-tolerant Tunnels, DMs bridged to LXMF, and `rabbit://` links
that can carry RNS destination hashes — so RabbitHole survives on LoRa /
packet-radio / off-grid links with no IP transit. Reticulum's reference
implementation is Mark Qvist's Python stack (`RNS`), with `LXMF` and `NomadNet`
layered on top. The protocol is defined largely by that implementation rather
than by a frozen RFC, so *byte-level* fidelity is a moving target tied to
upstream releases (the 0.7 → 0.8 line changed link/MTU handling, added ratchets,
etc.).

We already have a de-facto answer on the table: `crates/reticulum`
(`rabbithole-reticulum`), landed as the Wave-14 "RNS interop foundation" slice.
It is a **pure, sans-IO protocol data model + cryptographic identity** — no
transport, no interfaces, no networking — covering:

- `identity` — X25519 (encryption) + Ed25519 (signing); public identity
  `x25519_pub(32) ‖ ed25519_pub(32)`; identity hash `SHA-256(public)[..16]`.
- `destination` — `app_name`+aspects naming, the 10-byte name hash and 16-byte
  destination hash, and the hex `DestinationHash` value type.
- `packet` — the RNS wire packet header/body with a **never-panic** codec that
  enforces the 500-byte MTU and exposes the forwarding-stable packet hash.
- `announce` — announce construction + Ed25519 verification, plus a TTL +
  rate-limit `AnnounceCache` with an injected clock.
- `link` — sans-IO `LinkInitiator`/`LinkResponder` FSMs (request → proof → RTT,
  `Pending → Handshake → Active → Closed`), per-direction AEAD in `LinkCipher`.
- `crypto` — signing/verification + an X25519 + AEAD token.
- `lxmf` — addressed, Ed25519-signed LXMF messages (build/hash/sign/pack).

It carries **99 tests**, is `#![forbid(unsafe_code)]`, injects all randomness
and clocks, and — critically — **documents its own divergences from upstream**:
five module-level divergence notes and **seven inline `// SPEC-CHECK:` anchors**,
each pinned by a test so the eventual interop pass can adjust one byte-layout in
exactly one place. The open question this record closes is not "do we have a
model?" but "**how do we get to a wire-compatible, deployable RNS gateway** — and
what do we depend on to validate it?"

## Options considered

### (a) Depend on the community `reticulum-rs` crate

Adopt a third-party Rust RNS implementation and build the Burrow adapter on top.

> **Uncertainty flag.** This sandbox cannot reach crates.io or GitHub, so the
> current version, maintenance cadence, feature coverage, and audit status of
> any community `reticulum-rs` **could not be verified for this record**. The
> assessment below is from general knowledge as of early 2026 and must be
> re-checked before this option is ever reconsidered.

To the best of our knowledge the Rust RNS ports are **early-stage and partial**:
they track a subset of the Python stack, tend to lag upstream protocol changes,
and do not offer the same interface/transport breadth (TCP/UDP/LoRa/serial/I2P)
or the LXMF + propagation-node ecosystem. Pros: potentially a shorter path to a
*working* transport if a mature crate exists. Cons: (1) unverified maturity and
bus-factor; (2) a large new dependency + transitive supply-chain surface pulled
into a workspace that is otherwise deliberately lean (`blake3`, `ed25519-dalek`,
`x25519-dalek`, `sha2`, `hkdf`, `chacha20poly1305`, `postcard`); (3) we would
still owe our own reconciliation work because *upstream Python* is the
compatibility oracle, not another Rust port; (4) it inverts control — a sans-IO
core we drive from our own socket loop becomes someone else's runtime we adapt
to. **Not chosen now**, but re-evaluate at the transport slice if a crate has
demonstrably reached parity + an audit.

### (b) Python RNS sidecar via a local socket bridge

Run the reference `RNS`/`LXMF` Python daemon as a sidecar process and bridge it
to the Burrow over a local socket (Unix domain socket / loopback), letting
Python own the mesh interfaces and RabbitHole speak a thin local protocol.

Pros: **maximum wire fidelity by construction** — the reference implementation
*is* the spec, so ratchets, IFAC, interface types, and LXMF propagation nodes
all "just work" and stay correct as upstream evolves. Cons: (1) a Python runtime
+ its dependencies added to every deployment (containers, systemd, LoRa boards),
against RabbitHole's single-static-binary posture; (2) a second process to
supervise, version-pin, and secure; (3) a bridge protocol and trust boundary to
design and audit; (4) operational drift between two languages. Strong for
*correctness*, weak for *distribution and self-containment*.

### (c) Continue the in-repo pure-Rust sans-IO model crate

Keep investing in `rabbithole-reticulum`: finish reconciling the documented
divergences, then add the transport/interface layer and the Burrow-as-a-
Reticulum-destination adapter that drives the sans-IO core from a socket loop.
This is the **current de-facto path** (99 tests already in tree).

Pros: (1) one language, one build, one static binary — matches every other
RabbitHole surface; (2) lean, auditable supply chain (only vetted RustCrypto +
`postcard`); (3) sans-IO design makes the whole protocol exhaustively testable
without a network and lets us reuse the same core across native, wasm, and
embedded targets; (4) we own the reconciliation and can pin every byte decision
behind a single seam. Cons: (1) we carry the reconciliation burden ourselves,
tracking upstream Python across releases; (2) the crate is **model-only today** —
no bytes have crossed a real RNS link — so parity is asserted by our own tests,
not proven against a live peer; (3) more total work than option (a) *if* a
mature crate existed.

## Decision

**Adopt (c) — the in-repo pure-Rust sans-IO crate — as the primary
implementation path, and use (b) — the Python RNS sidecar — as an
interop-validation *reference peer* in a future test rig, not as a production
dependency.** Option (a) is declined for now and revisited only if a community
crate demonstrably reaches upstream parity with an audit trail.

Rationale, in priority order:

1. **Supply-chain posture.** RabbitHole ships as a single static Rust binary
   with a small, vetted crypto dependency set. (c) preserves that; (a) adds an
   unverified large dependency; (b) adds a whole Python runtime to every node,
   including the constrained LoRa/packet-radio targets Wave 14 exists to serve.
2. **The sans-IO design already pays off.** Injected clocks + injected
   randomness + never-panic decoders mean the entire RNS state machine
   (identity, destination hashing, packet codec, announce ingestion, link
   handshake, LXMF packing) is exercised deterministically by 99 tests with no
   sockets. That is exactly the property we want when the wire counterpart is a
   moving, implementation-defined target.
3. **We owe reconciliation regardless.** Even option (a) would not free us from
   validating against the Python oracle, because the *reference* is Python, not
   another Rust port. So the marginal cost of (c) over (a) is "write the
   transport ourselves" — which we want to own anyway for the single-binary and
   embedded story.
4. **(b) is the right tool for the one thing it's best at — proving bytes.** A
   throwaway/CI-only sidecar makes the reference implementation available as a
   conformance peer without inflicting it on operators. It validates (c); it
   does not ship inside it.

## Acceptance gate — reconcile the 7 SPEC-CHECK points first

The crate is honest that it is **not yet byte-compatible** with upstream RNS.
Before **any live RNS deployment or exchange with real RNS/LXMF/NomadNet peers**,
the transport slice MUST reconcile the seven `// SPEC-CHECK:` points already
flagged and test-pinned in the module docs (each is isolated so it can be fixed
in one place), validated against a reference Python peer per option (b):

1. **Link cipher / HKDF `info`** (`link.rs`). The link channel substitutes a
   per-direction ChaCha20-Poly1305 pair (64-byte HKDF expansion, counter
   nonces, replay rejection) for upstream's single AES-128-CBC + HMAC-SHA256
   Fernet-like token, and uses a crate-versioned HKDF `info` where upstream
   passes an empty context. Byte-compat is already broken here, so the whole
   `LinkCipher` seam is the reconciliation unit.
2. **Link RTT encoding** (`link.rs`). `LinkRtt` is a big-endian `u64` of
   milliseconds; upstream packs a MessagePack float of seconds.
3. **Establishment timeout scaling** (`link.rs`).
   `DEFAULT_ESTABLISHMENT_TIMEOUT_MS = 6000` mirrors upstream's 6 s per-hop
   budget at a *single* hop; upstream scales by path hop count and per-interface
   latency, which a transport slice must reintroduce.
4. **0.8-era link-request MTU signalling** (`link.rs`).
   `LinkRequest::from_bytes` requires exactly 64 bytes; newer upstream may append
   link-MTU signalling bytes to the request that must be tolerated.
5. **Packet MDU / IFAC modelling** (`packet.rs`). Upstream advertises a flat
   `Packet.MDU = 464` (budgeting the maximum 35-byte header plus one reserved
   IFAC byte for every packet); this codec caps encoded packets at the 500-byte
   MTU with exact per-header budgets (481 for HEADER_1, 465 for HEADER_2) and
   models **no IFAC field body**. Adding IFAC must revisit `max_data_len`.
6. **Packet context bytes** (`packet.rs`). `NONE`/`LRPROOF` match upstream; the
   link-lifecycle contexts (`KEEPALIVE`, `LINKIDENTIFY`, `LINKCLOSE`,
   `LINKPROOF`, `LRRTT`) follow the RNS `Packet.py` constants as understood at
   0.7/0.8 and are pinned in `context_bytes_are_pinned`.
7. **Announce de-duplication policy** (`announce.rs`). Upstream `Transport`
   de-duplicates by packet hash and applies per-interface
   `announce_rate_target`-style policy; `AnnounceCache` is a deliberately
   simplified per-destination TTL + min-interval policy for local ingestion, not
   a wire-behavior clone.

Two further module-level **divergences** feed the same gate and should be closed
alongside the seven anchors: the **announce wire field order** (this crate puts
the signature *last*; upstream places it before the trailing app-data — the
*signed content* is identical, so signatures interchange, but the serialized
byte order differs) and the **LXMF payload packing** (deterministic `postcard`
with a string-keyed `fields` map + hash-only signing here, vs upstream's
MessagePack `[timestamp, title, content, fields]` with integer-keyed fields and
its stamp/proof-of-work cost field — deferred). Ratchets and IFAC bodies are
explicitly out of scope for the model slice and are net-new work for the
transport slice.

**Gate summary:** no `rabbithole-reticulum` code path may open a socket to the
public RNS mesh until (i) all seven SPEC-CHECK points and the two divergences
above are reconciled and (ii) a conformance run against a reference Python `RNS`
peer (option (b), CI/dev only) passes for identity announce, link establishment,
and an LXMF round-trip.

## Consequences

- **Immediate:** the `[ ] Spike` line in `TODO.md` §Wave 14 is resolved by this
  record; the existing `[~]` "RNS interop foundation" and "LXMF bridge" slices
  continue as the chosen path. No new production dependency is added.
- **Next slices (unchanged direction):** (1) a transport/interface layer + the
  Burrow-as-Reticulum-destination adapter that drives the sans-IO core; (2)
  reconciling the seven SPEC-CHECK points + two divergences behind their single
  seams; (3) a CI conformance rig standing up a Python `RNS` sidecar as the
  reference peer (option (b)); (4) the DMs↔LXMF and boards↔propagation-node
  bridges over the shared dupe subsystem.
- **Supply chain:** stays lean and all-Rust in the shipped artifact. The Python
  sidecar is confined to test/dev tooling and never becomes a runtime
  requirement for operators.
- **Risk owned:** we accept the reconciliation burden of tracking upstream
  Python across releases. This is bounded and made cheap by the sans-IO design +
  the one-place SPEC-CHECK seams; the conformance rig turns "did upstream move?"
  into a failing test rather than a field incident.
- **Re-evaluation trigger:** if a community `reticulum-rs` later demonstrates
  upstream parity, active maintenance, and a security audit, option (a) may be
  reconsidered for the transport layer — but the pure-Rust model core and its
  SPEC-CHECK discipline remain the compatibility reference either way.
