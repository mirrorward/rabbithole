# RHP Session Family (0)

Status: **Wave 0** — hello/version/capability negotiation only.
Authentication, keepalive, and resume land in Wave 1.

## Messages

| type | name | direction | payload |
|---|---|---|---|
| 1 | Hello | client → server (Request) | `version: u16`, `capabilities: [string]`, `client_name: string`, `client_version: string` |
| 2 | HelloAck | server → client (Reply) | `version: u16` (negotiated), `capabilities: [string]`, `server_name: string`, `server_version: string`, `server_key: [u8; 32]` |

## Sequence

1. Transport connects (QUIC control stream opens / WS upgrades).
2. Client sends `Hello` as its first frame, offering its highest protocol
   version and its capability set.
3. Server negotiates `min(client_version, server_version)`; if below its
   minimum supported version it replies with error `VersionMismatch` and MAY
   close.
4. On success the server replies `HelloAck` carrying the negotiated version,
   its own capabilities, display name, software version, and its Ed25519
   identity key (the anchor for federation signatures; all-zero until Wave 1
   ships persistent server identity).
5. The effective capability set is the **intersection** of both sides'.
   Everything after this point (Wave 1+: authenticate, agreement gate,
   welcome bundle) is gated on negotiated capabilities.

## Capability names (registry)

| name | meaning |
|---|---|
| `session-resume` | server supports token + replay-cursor session resumption (Wave 1) |
| `key-auth` | Ed25519 challenge/response login (Wave 1) |
| `guest` | guest sign-in permitted |

Third-party extensions use an `x-` prefix.
