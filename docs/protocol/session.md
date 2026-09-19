# RHP Session Family (0)

Status: **Wave 1** — hello, authentication (password/guest/resume),
keepalive, welcome, agreement, push replay. **Wave 2** added registration,
personas, TOTP 2FA, and key enrollment (below); the welcome screen / theme /
keyword messages (types 42-47) also live on this family — see
[`welcome.md`](welcome.md).

## Messages

| type | name | direction | payload |
|---|---|---|---|
| 1 | Hello | client → server (Request) | `version: u16`, `capabilities: [string]`, `client_name: string`, `client_version: string`, `client_pubkey: option<[u8; 32]>` (a key to prove) |
| 2 | HelloAck | server → client (Reply) | `version: u16` (negotiated), `capabilities: [string]`, `server_name: string`, `server_version: string`, `server_key: [u8; 32]`, `challenge: option<[u8; 32]>` (a fresh nonce when a key was offered) |
| 3 | KeyProof | Request (pre-auth) | `signature: bytes`: the offered key's Ed25519 signature over `rabbithole-key-auth-v2`, the channel binder (the server's certificate fingerprint over QUIC, zeros over WebSocket) and the challenge. Always acked; a wrong proof simply proves nothing |
| 10 | AuthPassword | Request | `login: string`, `password: string`, `totp: option<string>` (current TOTP code or a recovery code, when 2FA is enrolled) |
| 11 | AuthGuest | Request | `desired_name: option<string>` |
| 12 | AuthResume | Request | `token: string`, `replay_cursor: u64` |
| 13 | AuthOk | Reply (to 10/11/12/14) | `token: string` (empty for guests), `account_id: i64`, `screen_name: string`, `role: u8`, `caps: u64`, `resumed: bool` |
| 14 | Register | Request (pre-auth) | `login`, `password`, `invite_code: option<string>` — honors the registration mode (open/invite/closed); success auto-signs-in → `AuthOk`; else `Forbidden`/`AlreadyExists` |
| 15 | PullSessionOpen | Request (pre-auth) | `grant: bytes`: a pull grant this burrow signed. A burrow fetching files it was granted, with no account here (see below) |
| 20 | Ping | Request | — |
| 21 | Pong | Reply | — |
| 30 | AgreementAccept | Request | — (empty ack reply) |
| 40 | Welcome | Push | `motd: string`, `agreement: option<string>` |
| 41 | ServerNotice | Push | `text: string`, `from: string` — operator notice (admin `Broadcast`, shutdown warning) |

Roles: 0 guest, 1 user, 2 moderator, 3 admin, 4 superuser.

## Personas (Wave 2)

An account holds up to `persona_max` personas; a session is bound to one at
a time and may switch live (presence broadcasts the change).

| type | name | direction | payload |
|---|---|---|---|
| 50/55 | PersonaListRequest → PersonaList | Request/Reply | `personas: [PersonaInfo]`, `active_id: i64` |
| 51 | PersonaCreate | Request | `screen_name` → `PersonaReply`; `AlreadyExists`/`TooLarge`; cap reached → `Forbidden` |
| 52 | PersonaUpdate | Request | `id`, `profile?`, `avatar: option<option<[u8;32]>>` (`Some(None)` clears), `banner` likewise, `directory_visible?` → `PersonaReply` |
| 53 | PersonaDelete | Request | `id` — not the last one; not while another session uses it |
| 54 | PersonaSwitch | Request | `id` → `PersonaReply` |
| 56 | PersonaReply | Reply | `persona: PersonaInfo` |

`PersonaInfo`: `id: i64`, `screen_name`, `is_default: bool`, `profile:
Profile`, `avatar: option<[u8;32]>` / `banner: option<[u8;32]>` (blob ids,
fetched via `BlobGet`), `directory_visible: bool`. `Profile`: `location?`,
`interests?`, `quote?`, `plan?` (also the finger `.plan`), `pronouns?`.

## 2FA & key enrollment (Wave 2)

| type | name | direction | payload |
|---|---|---|---|
| 60/61 | TotpEnrollBegin → TotpEnrollInfo | Request/Reply | `secret_base32`, `provisioning_url` |
| 62/63 | TotpEnrollConfirm → RecoveryCodes | Request/Reply | `code` → `codes: [string]` (shown once) |
| 64 | TotpDisable | Request | `password` (empty ack) |
| 65 | KeyEnroll | Request | `pubkey: [u8; 32]` — Ed25519 key for key auth / event signing (empty ack) |

Once enrolled, `AuthPassword` with valid credentials but no/wrong `totp`
answers `TotpRequired`; a recovery code is accepted in the `totp` field.

## Sequence

1. Client sends `Hello` first; server answers `HelloAck` (version =
   `min(client, server)`, error `VersionMismatch` if below the server's
   floor). Capabilities are intersected.
2. Client authenticates with exactly one of `AuthPassword`, `AuthGuest`,
   `AuthResume`, `Register`. Before auth, only `Hello`, auth requests, and
   `Ping` are honored; anything else gets `Unauthenticated` (or `BadRequest`
   before hello). Failed auths reply `Unauthenticated` (credentials — the
   server deliberately does not reveal whether the login exists),
   `TotpRequired` (2FA enrolled, code missing/wrong), `Forbidden` (guests
   disabled / account disabled), `SessionExpired` (bad token), or
   `RateLimited` (auth budget drained); the connection stays open for
   another attempt.
3. On `AuthOk`, the server pushes `Welcome`. If `agreement` is non-null,
   the client must send `AgreementAccept` before participating (until
   then, participating requests answer `Forbidden`).
4. Re-`Hello` or re-auth on an authenticated session is a `BadRequest`.

## Pull sessions

A burrow that is not this one's federation peer fetches files it was granted
over a pull session on the client QUIC port (see FILE 39 in
[`file.md`](file.md)). It sends `Hello` offering its server key, `KeyProof`
bound to this burrow's certificate, then `PullSessionOpen` with the grant.
The session opens only over QUIC, only for the key proved on that connection,
and only when the grant names that key, verifies, and has not run out (five
minutes' leeway for the two clocks); refusals are `Unauthenticated` (no key
proved over QUIC), `Unsupported` (grants off, or grants only to peers and the
key is not one), `BadRequest` (unreadable or over 384 KiB), `Forbidden` (not
this burrow's grant, not for that key, run out, or its person's account is
disabled), `AlreadyExists` (a session for that grant is already open) and
`RateLimited` (32 open already, or 4 for grants one person asked for).
Each refusal is charged to the address's sign-in budget. Once open, the
connection answers only `Ping` on the control stream and pull streams for
that grant on bulk streams, eight at a time, until the grant's six-hour grace
ends, it closes, the burrow shuts down, or (checked every 15 seconds) the
grant's person's account is disabled or the operator stops sending to it.

## Push replay

Push frames carry a **per-account monotonically increasing sequence
number in the frame `id` field**. Clients track the highest seen and
present it as `replay_cursor` in `AuthResume`; the server replays pushes
newer than the cursor from a bounded in-memory ring (best-effort — a
long-offline client re-syncs from state instead). Guests are not
resumable (`AuthOk.token` is empty).

## Errors

`Unauthenticated` bad credentials · `TotpRequired` 2FA code needed ·
`Forbidden` guests disabled, account disabled, agreement pending ·
`SessionExpired` bad/expired resume token · `VersionMismatch` protocol floor
· `RateLimited` auth budget · `Kicked` operator disconnect · `Unsupported`
unknown message type.
