# RHP Admin Family (7)

Status: **Wave 2.1**. Every operation requires a capability bit and is
written to the audit log. Any authorized client is an admin console.

| type | name | direction | requires | payload |
|---|---|---|---|---|
| 1 | ClassListRequest | Request | ACCOUNT_ADMIN | — |
| 2 | ClassList | Reply | | `classes: [{name, base_mask: u64, members: u64}]` |
| 3 | ClassSet | Request | ACCOUNT_ADMIN | `name`, `base_mask` — creates or updates; **applies to all members immediately** (live inheritance). `Forbidden` when the mask carries a capability the operator does not hold: nobody grants what they do not have |
| 4 | AccountListRequest | Request | ACCOUNT_ADMIN | `offset: u32`, `limit: u32` (≤200) |
| 5 | AccountList | Reply | | `accounts: [{id, login, role, class?, disabled}]`, `total` |
| 6 | AccountSet | Request | ACCOUNT_ADMIN | `login` + optional `role`/`class` (empty string clears)/`disabled`. Subject to the standing rules below; disabling signs the account out at once |
| 7 | InviteCreate | Request | ACCOUNT_ADMIN | `ttl_secs` (clamped 60s–90d) |
| 8 | InviteCode | Reply | | `code`, `expires_at_unix` |
| 9 | Broadcast | Request | BROADCAST | `text` — sessions receive a `ServerNotice` push |
| 10 | Kick | Request | USER_KICK | `session_id`; refused against `>=` roles (superusers exempt) |
| 11/12 | ConfigGet → ConfigValue | Request/Reply | CONFIG_ADMIN | key/value |
| 13/14 | ConfigSet → ConfigApplied | Request/Reply | CONFIG_ADMIN | `applied_live: bool` (false = restart needed). The change is written to the config file before it goes live, or it is refused: `BadRequest` for a value the key will not take, `Internal` when the file could not be written. Only the changed key is written; env and command-line overrides are never baked into the file. A credential's value never reaches the audit log |
| 15/16 | ConfigDescribeRequest → ConfigDescription | Request/Reply | CONFIG_ADMIN | every key an operator can see, as `entries: [ConfigKeyInfo]`: `key`, `value`, `default`, `kind` (0 text, 1 bool, 2 number, 3 choice), `flags` (1 LIVE: applies without a restart; 2 SECRET: `value` is withheld; 4 SET: a secret is stored; 8 READ_ONLY: shown, not settable), `choices` (the accepted values of a choice). Derived from the setter itself on a scratch copy of the defaults, so it cannot disagree with what `ConfigSet` accepts. A client needs no list of keys, shapes or defaults of its own, and a key it has no words for is still reachable |
| 17/18 | SurfaceStatusRequest → SurfaceStatus | Request/Reply | CONFIG_ADMIN | what each optional surface is actually doing, as `surfaces: [SurfaceInfo]`: `key` (the `*_enabled` key that switches it, so a console can put the report beside the switch), `state` (0 off, 1 listening, 2 running with no address of its own, 3 failed, 4 on with nothing to do), `addr` (the bound address when listening), `detail` (why it failed, or what it is waiting for). A setting says what was asked for; this says what happened. The supervisor reconciles before `ConfigApplied` is sent, so a request made after a save sees its result |
| 19 | AccountCreate | Request | ACCOUNT_ADMIN | `login`, `password`, `role` (ordinal) → empty ack. `AlreadyExists` when the login or a persona of that name is taken; `BadRequest` for a login with whitespace or over 32 characters, a password under 8, or a role that is not one; `Forbidden` for a role above the operator's own. The password never reaches a log (`Debug` omits it) |
| 20 | AccountPasswordSet | Request | ACCOUNT_ADMIN | `login`, `password` → empty ack. Every saved sign-in of the account is revoked and every open session closed: whoever had the old password is out too |
| 21 | AccountTotpReset | Request | ACCOUNT_ADMIN | `login` → empty ack; `NotFound` when the account has no two-factor |
| 22/23 | InviteListRequest → InviteList | Request/Reply | ACCOUNT_ADMIN | `invites: [InviteEntry]`, newest first: `code`, `created_by` (login), `expires_at` (unix seconds), `used_by` (login, when used) |
| 24 | InviteRevoke | Request | ACCOUNT_ADMIN | `code` → empty ack; `NotFound` for an unknown code or one already used |
| 25/26 | AuditListRequest → AuditList | Request/Reply | AUDIT_READ | `limit` (clamped to 500) → `entries: [AuditEntry]`, oldest first: `at` (unix seconds), `actor` (a login, or `ctl`), `action`, `detail`. Never a credential. The bit was granted to admins and moderators from the permission wave on and checked by nothing until now: the log was reachable from the command line only |

Types 30..40 are the Wave 13 moderation suite (reports, quarantine,
hash-deny list); see `rabbithole-proto::admin`.

### Standing: who may change whom

`ACCOUNT_ADMIN` says an operator may manage accounts, not *which*. Every account
operation (`AccountSet`, `AccountCreate`, `AccountPasswordSet`,
`AccountTotpReset`) applies the same ordering:

- an operator acts on accounts **below** their own role: never a peer, never
  someone above (`Forbidden`);
- an operator hands out roles **up to** their own, never above it;
- an operator never acts on **themselves** here (their own password and
  two-factor have their own doors; disabling or demoting oneself is a lockout);
- a superuser is exempt from the ordering, and from nothing else.

Before 0.218.0 `AccountSet` had no ordering at all (an account admin could make
anyone, themselves included, a superuser) and `ClassSet` no capability check.

## Theme bundle application (Wave 8): types 41..44

| type | name | direction | requires | payload |
|---|---|---|---|---|
| 41 | ThemeBundleSet | Request | CONFIG_ADMIN | `bundle` (postcard `ThemeBundle`, the exact bytes a `ThemeReply` carries; art as blob refs uploaded via `BlobPut` first), `signature` (optional Ed25519 by the server key — the re-import path) → ThemeBundleInfo |
| 42 | ThemeBundleClear | Request | CONFIG_ADMIN | — (empty ack; clients fall back to default tokens) |
| 43/44 | ThemeBundleGet → ThemeBundleInfo | Request/Reply | CONFIG_ADMIN | `present`, `id` (blake3 of canonical bundle bytes), `name`, `applied_at_unix`, `applied_by`, accent/logo/banner flags, icon + token counts |

`ThemeBundleSet` validates hard before applying, because a server theme
hits everyone: structured tokens only (colour tokens hex, metric tokens
from a small CSS-length grammar, `--rh-bg-image` only `none`; anything
unknown or free-form is refused), WCAG rails (text-on-bg and accent-on-bg
must clear **4.5:1 in both modes** — below that the bundle is *rejected*
with the computed ratio, stricter than the client editor's warn-only), and
art size caps (`banner_max_bytes` / `avatar_max_bytes`). Rejections are
audited with the reason. Users can opt out per account via the session
family's `ThemePrefSet` (57..59) — their `ThemeGet` then answers
`NotFound` and the client renders default tokens.

## Gateway/feed statistics (Wave 10): types 45..46

| type | name | direction | requires | payload |
|------|------|-----------|----------|---------|
| 45 | GatewayStatsRequest | Request | CONFIG_ADMIN | — → GatewayStatsReply |
| 46 | GatewayStatsReply | Reply | | `generated_at_ms`, `feeds: [{url, last_poll_ms, last_status, items_seen, items_posted, dupes_dropped}]`, `gateways: [{name, enabled, counters: [(name, u64)]}]` |

A point-in-time snapshot of the in-memory syndication/legacy-gateway
activity counters (they reset on restart — activity meters, not durable
accounting). `last_status` is `"ok"` / `"not_modified"` / `"error"` / `""`
(never polled). Gateway `counters` are string-keyed so the set can grow
without a protocol bump; known keys today: `nntp.sessions`, `nntp.posts`,
`nntp_feed.accepted`, `ftn.echomail_posts`, `qwk.packets_built`,
`qwk.replies_ingested`, `hotline.logins`, `radio.sources_connected`,
`telnet.logins`. Read-only, not audited. Also exposed as
`burrow ctl gateway-stats` (JSON). This fills the "live stats" seam the web
admin's syndication panel documented.
