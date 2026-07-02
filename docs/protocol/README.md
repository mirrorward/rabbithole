# The RabbitHole Protocol (RHP)

The protocol specification is a first-class deliverable, maintained in lockstep
with the implementation in `crates/proto`. If the code and this spec disagree,
that is a bug in one of them.

## Documents

| Doc | Status | Contents |
|---|---|---|
| [`framing.md`](framing.md) | Wave 0 | Transports, frame layout, length delimiting, size limits |
| [`session.md`](session.md) | Wave 1 | Hello, auth (password/guest/resume), keepalive, welcome/agreement, push replay |
| [`presence.md`](presence.md) | Wave 1 | Who-list, join/leave pushes |
| [`chat.md`](chat.md) | Wave 1 | Lobby chat, history, chat pushes |
| families/*.md | future waves | One doc per family as it lands (dm, board, file, swarm, admin, federation, radio, wishing-well) |

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
