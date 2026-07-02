# RHP Presence Family (1)

Status: **Wave 1** — the who-list and join/leave pushes. **Wave 2** added
buddy lists, presence states (incl. Cheshire mode), profile lookup, and the
member directory (below).

## Messages

| type | name | direction | payload |
|---|---|---|---|
| 1 | Who | Request | — (requires the `WHO` capability) |
| 2 | WhoList | Reply | `users: [UserSummary]` |
| 3 | UserJoined | Push | `user: UserSummary` |
| 4 | UserLeft | Push | `session_id: u64`, `screen_name: string` |

`UserSummary`: `session_id: u64`, `screen_name: string`, `role: u8`,
`transport: string` ("quic", "websocket"; later "telnet", "hotline", …),
`connected_secs: u64`.

The who-list is ordered by join time (regulars first). One presence
registry feeds every surface — the native list, the telnet who screen,
finger, and the Hotline compat user list.

## Wave 2.2 additions

| type | name | direction | payload |
|---|---|---|---|
| 20 | PresenceSet | Request | `state` (0 online / 1 away / 2 idle / 3 invisible), `status?` (≤200) |
| 21/22 | BuddyListRequest → BuddyList | Request/Reply | buddies `[{screen_name, group, online, state, status?}]` + `blocked: [string]` |
| 23/24 | BuddyAdd / BuddyRemove | Request | add takes a group (upsert moves groups) |
| 25/26 | BlockAdd / BlockRemove | Request | blocks are account-level, resolved via persona name |
| 5 | UserChanged | Push | `session_id`, `screen_name`, `state`, `status?` — persona switch, state, or status change |

## Profiles & directory (Wave 2)

| type | name | direction | payload |
|---|---|---|---|
| 10/11 | ProfileGet → ProfileCard | Request/Reply | `screen_name` → `profile: Profile`, `avatar?`/`banner?` (blob ids), `online_transport: option<string>` (present iff online — "locate a member"). `NotFound` for unknown *and* directory-hidden personas |
| 12/13 | DirectorySearch → DirectoryResults | Request/Reply | `query`, `limit` (clamped to 100) — substring search over name/profile of `directory_visible` personas → `personas: [PersonaInfo]` |

**Cheshire mode** (invisible): sub-moderator viewers get a synthetic
`UserLeft` when a user vanishes and `UserJoined` when they reappear; the
who-list and buddy lists show them offline. Moderators+ see the truth,
marked invisible. `UserSummary` gained `state`/`status` fields.
