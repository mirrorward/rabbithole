# RHP Admin Family (7)

Status: **Wave 2.1**. Every operation requires a capability bit and is
written to the audit log. Any authorized client is an admin console.

| type | name | direction | requires | payload |
|---|---|---|---|---|
| 1 | ClassListRequest | Request | ACCOUNT_ADMIN | — |
| 2 | ClassList | Reply | | `classes: [{name, base_mask: u64, members: u64}]` |
| 3 | ClassSet | Request | ACCOUNT_ADMIN | `name`, `base_mask` — creates or updates; **applies to all members immediately** (live inheritance) |
| 4 | AccountListRequest | Request | ACCOUNT_ADMIN | `offset: u32`, `limit: u32` (≤200) |
| 5 | AccountList | Reply | | `accounts: [{id, login, role, class?, disabled}]`, `total` |
| 6 | AccountSet | Request | ACCOUNT_ADMIN | `login` + optional `role`/`class` (empty string clears)/`disabled` |
| 7 | InviteCreate | Request | ACCOUNT_ADMIN | `ttl_secs` (clamped 60s–90d) |
| 8 | InviteCode | Reply | | `code`, `expires_at_unix` |
| 9 | Broadcast | Request | BROADCAST | `text` — sessions receive a `ServerNotice` push |
| 10 | Kick | Request | USER_KICK | `session_id`; refused against `>=` roles (superusers exempt) |
| 11/12 | ConfigGet → ConfigValue | Request/Reply | CONFIG_ADMIN | key/value |
| 13/14 | ConfigSet → ConfigApplied | Request/Reply | CONFIG_ADMIN | `applied_live: bool` (false = restart needed) |

Types 30..40 are the Wave 13 moderation suite (reports, quarantine,
hash-deny list); see `rabbithole-proto::admin`.

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
