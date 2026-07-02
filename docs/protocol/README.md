# The RabbitHole Protocol (RHP)

The protocol specification is a first-class deliverable, maintained in lockstep
with the implementation in `crates/proto`. If the code and this spec disagree,
that is a bug in one of them.

## Documents

| Doc | Status | Contents |
|---|---|---|
| [`framing.md`](framing.md) | Wave 0, shipped | Transports, frame layout, length delimiting, size limits, families, error codes |
| [`session.md`](session.md) | Waves 1-2, shipped | Hello, auth (password/guest/resume/register), keepalive, welcome/agreement, push replay, personas, TOTP 2FA, key enrollment |
| [`presence.md`](presence.md) | Waves 1-2, shipped | Who-list, join/leave pushes, buddy lists, states (Cheshire mode), profiles, directory |
| [`chat.md`](chat.md) | Waves 1-2, shipped | Lobby chat, history, chat pushes, rooms (public/private/ad-hoc) |
| [`admin.md`](admin.md) | Wave 2, shipped | Remote admin family (capability-gated, audited) |
| [`dm.md`](dm.md) | Wave 2, shipped | Direct messages: threads, receipts, attachments, offline queue |
| [`welcome.md`](welcome.md) | Wave 2, shipped | Welcome screen composer, signed theme bundle, keyword `/go` |
| [`board.md`](board.md) | Wave 3, shipped | Message bases: board tree, signed post events, threading, read pointers |
| [`wish.md`](wish.md) | Wave 3, shipped | The Wishing Well: request board with voting, claim, fulfillment |
| [`file.md`](file.md) | Wave 4, shipped | File libraries: areas, folder tree, metadata, drop boxes, aliases, search, small blobs, resumable transfers, quotas |
| [`swarm.md`](swarm.md) | Wave 5, shipped (NAT traversal pending) | The Warren: advertise (list-without-upload), find-sources, capability tokens, Bao peer wire, multi-source fetch |
| [`federation.md`](federation.md) | Wave 9, S2S handshake + catalog sync shipped; rest model-only | Tunnels: dedicated QUIC endpoint, nonce-bound Ed25519 handshake, admin approval, signed catalogs, attestation model |
| [`../legacy-surfaces.md`](../legacy-surfaces.md) | Waves 6-11, shipped | Operator matrix for every legacy listener (telnet, finger, NNTP, Hotline, FTN, QWK, radio, syndication) |
| radio family doc | future wave | Family 9 is reserved; no native radio messages exist yet |

## Families

Family numbers from `crates/proto/src/frame.rs` (`Family` is a `u8`
newtype; unknown families decode and answer `Unsupported`):

| # | Name | Doc | Notes |
|---|---|---|---|
| 0 | SESSION | [`session.md`](session.md), [`welcome.md`](welcome.md) | auth, personas, 2FA, welcome/theme/keyword (42-47) |
| 1 | PRESENCE | [`presence.md`](presence.md) | who-list, buddies, profiles, directory |
| 2 | CHAT | [`chat.md`](chat.md) | rooms |
| 3 | DM | [`dm.md`](dm.md) | direct messages |
| 4 | BOARD | [`board.md`](board.md) | message bases |
| 5 | FILE | [`file.md`](file.md) | libraries (1-19), transfers (20-42), blobs (100-103) |
| 6 | SWARM | [`swarm.md`](swarm.md) | the Warren |
| 7 | ADMIN | [`admin.md`](admin.md) | remote administration |
| 8 | FEDERATION | [`federation.md`](federation.md) | **S2S-only** — dedicated QUIC endpoint, never on client connections |
| 9 | RADIO | — | **reserved**, no messages defined yet |
| 10 | WISHING_WELL | [`wish.md`](wish.md) | requests |

## Ground rules

1. **One framing for everything.** Every message is a `Request`, `Reply`, or
   `Push` in the same frame shape (Hotline's transaction lesson).
2. **Route pushes by type, not by request id.** Server-initiated frames use
   id 0 and are dispatched on `(kind, family, message_type)`.
3. **Unknown ≠ broken.** Unknown families/types decode fine and are answered
   with `Unsupported`. Payload schemas are `#[non_exhaustive]`.
4. **Version bumps are rare.** Additive features are negotiated by capability
   strings in Hello/HelloAck; the version only bumps on incompatible framing
   changes.
5. **Bulk data never rides the control stream.** Anything big gets a transfer
   ticket and its own QUIC stream (or WS side channel). Control frames are
   capped at 1 MiB.
