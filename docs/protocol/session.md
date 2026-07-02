# RHP Session Family (0)

Status: **Wave 1** — hello, authentication (password/guest/resume),
keepalive, welcome, agreement, push replay.

## Messages

| type | name | direction | payload |
|---|---|---|---|
| 1 | Hello | client → server (Request) | `version: u16`, `capabilities: [string]`, `client_name: string`, `client_version: string` |
| 2 | HelloAck | server → client (Reply) | `version: u16` (negotiated), `capabilities: [string]`, `server_name: string`, `server_version: string`, `server_key: [u8; 32]` |
| 10 | AuthPassword | Request | `login: string`, `password: string` |
| 11 | AuthGuest | Request | `desired_name: option<string>` |
| 12 | AuthResume | Request | `token: string`, `replay_cursor: u64` |
| 13 | AuthOk | Reply (to 10/11/12) | `token: string` (empty for guests), `account_id: i64`, `screen_name: string`, `role: u8`, `caps: u64`, `resumed: bool` |
| 20 | Ping | Request | — |
| 21 | Pong | Reply | — |
| 30 | AgreementAccept | Request | — (empty ack reply) |
| 40 | Welcome | Push | `motd: string`, `agreement: option<string>` |

Roles: 0 guest, 1 user, 2 moderator, 3 admin, 4 superuser.

## Sequence

1. Client sends `Hello` first; server answers `HelloAck` (version =
   `min(client, server)`, error `VersionMismatch` if below the server's
   floor). Capabilities are intersected.
2. Client authenticates with exactly one of `AuthPassword`, `AuthGuest`,
   `AuthResume`. Before auth, only `Hello`, auth requests, and `Ping` are
   honored; anything else gets `Unauthenticated` (or `BadRequest` before
   hello). Failed auths reply `Unauthenticated` (credentials — the server
   deliberately does not reveal whether the login exists), `Forbidden`
   (guests disabled / account disabled), or `SessionExpired` (bad token);
   the connection stays open for another attempt.
3. On `AuthOk`, the server pushes `Welcome`. If `agreement` is non-null,
   the client must send `AgreementAccept` before participating (until
   then, participating requests answer `Forbidden`).
4. Re-`Hello` or re-auth on an authenticated session is a `BadRequest`.

## Push replay

Push frames carry a **per-account monotonically increasing sequence
number in the frame `id` field**. Clients track the highest seen and
present it as `replay_cursor` in `AuthResume`; the server replays pushes
newer than the cursor from a bounded in-memory ring (best-effort — a
long-offline client re-syncs from state instead). Guests are not
resumable (`AuthOk.token` is empty).

## Errors

`Unauthenticated` bad credentials · `Forbidden` guests disabled, account
disabled, agreement pending · `SessionExpired` bad/expired resume token ·
`VersionMismatch` protocol floor · `Unsupported` unknown message type.
