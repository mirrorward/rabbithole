# RHP Presence Family (1)

Status: **Wave 1** — the who-list and join/leave pushes. Buddy lists,
away/idle states, and Cheshire mode land in Wave 2 on this family.

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
registry feeds every surface — the native list, and later the telnet who
screen, finger, and the Hotline compat user list.
