# RHP Framing

Status: **Wave 0** — implemented in `crates/proto` (`frame.rs`, `codec.rs`).

## Transports

| Transport | Role | Delimiting |
|---|---|---|
| QUIC (quinn, TLS 1.3, ALPN `rhp/1`), default port **4653** | primary | 4-byte big-endian length prefix per frame on the control stream |
| WebSocket (`/rhp` on the web port, default **4654**) | mandatory fallback (browsers, UDP-hostile networks) | one binary message = one frame, no prefix |

On QUIC, the **first client-opened bidirectional stream is the control
stream**. Server→client push streams and per-transfer bulk streams are added
by later waves on the same connection.

TLS: ACME certificates where possible; otherwise self-signed with the
certificate's **blake3 fingerprint pinned** by clients (fingerprints travel in
rabbit links, Looking Glass listings, and `.well-known`).

## Frame

Postcard-encoded structure (field order is normative):

```text
Frame {
  version:      u16   protocol version (negotiated in Hello)
  kind:         enum  Request(0) | Reply(1) | Push(2)
  family:       u8    namespace (see below)
  message_type: u16   operation within the family
  id:           u64   request id; replies echo it; 0 for pushes
  error:        Option<ErrorCode>   replies only; None = success
  payload:      bytes postcard-encoded body, keyed by (family, message_type)
}
```

- The payload is **opaque at the framing layer** — a receiver that doesn't
  know `(family, message_type)` still parses the frame and replies
  `Unsupported`. This is the forward-compatibility backbone.
- Maximum encoded frame size: **1 MiB** (`MAX_FRAME_SIZE`). Oversized frames
  are a protocol error; bulk data uses transfer streams instead.
- Requests may be **pipelined**; replies may arrive out of order relative to
  other requests. Exactly one reply per request.

## Families

Numbers are the `Family` constants in `frame.rs`. `Family` is a `u8` newtype,
not a Rust enum, so unknown families still decode.

| # | Family | Content |
|---|---|---|
| 0 | session | hello, auth (password/guest/resume/register), keepalive, personas, 2FA, welcome/theme/keyword |
| 1 | presence | roster, buddy lists, states (Cheshire mode), profiles, directory |
| 2 | chat | rooms |
| 3 | dm | direct messages |
| 4 | board | message bases |
| 5 | file | areas, metadata, transfers (20+), small blobs (100+) |
| 6 | swarm | the Warren |
| 7 | admin | remote administration |
| 8 | federation | Tunnels (S2S) — **server-to-server only**, on a dedicated QUIC endpoint (default port **4655**); never spoken on a client connection. See [`federation.md`](federation.md) |
| 9 | radio | stations — **reserved**; no messages defined yet (the radio surface today is the legacy ICY listener, see [`../legacy-surfaces.md`](../legacy-surfaces.md)) |
| 10 | wishing-well | requests |

## Error codes

`BadRequest, Unauthenticated, Forbidden, NotFound, AlreadyExists,
RateLimited, Internal, VersionMismatch, Unsupported, TooLarge,
SessionExpired, Unavailable, TotpRequired, Kicked, Muted,
SlowMode { retry_after_secs: u32 }, Other(u16)` —
non-exhaustive; unknown codes decode as `Other` and must be treated as
generic failures. `TotpRequired`: credentials were valid but the account has
2FA enrolled and no (or a wrong) code was supplied. `Kicked`: the session was
disconnected by an operator. `Muted`: the principal is muted in the target
chat room. `SlowMode`: the room's slow-mode window hasn't elapsed — retry
after the carried number of seconds (distinct from `RateLimited`, the global
budget).
