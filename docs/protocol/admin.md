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
