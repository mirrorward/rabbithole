# RHP Chat Family (2)

Status: **Wave 1** — the single public lobby (`"lobby"`). Multiple,
ad-hoc, and private rooms arrive in Wave 2 using these same messages.

## Messages

| type | name | direction | payload |
|---|---|---|---|
| 1 | ChatSend | Request | `room: string`, `text: string` (empty ack reply) |
| 2 | ChatMessage | Push | `room: string`, `from: string`, `text: string`, `at_unix_ms: i64` |
| 3 | ChatHistoryRequest | Request | `room: string`, `limit: u32` (server caps at 500) |
| 4 | ChatHistory | Reply | `messages: [ChatMessage]` (oldest first) |

## Semantics

- Sending requires `CHAT_SEND` on resource `chat/<room>` and an accepted
  agreement; reading history requires `CHAT_READ`.
- The sender receives their own line back as a `ChatMessage` push — the
  push is the confirmation of broadcast order (the ack only confirms
  acceptance).
- Text is trimmed of trailing whitespace; empty text is `BadRequest`;
  lines over the server's `chat_max_len` are `TooLarge`; unknown rooms
  are `NotFound`.

## Rooms (Wave 2.2b)

| type | name | direction | payload |
|---|---|---|---|
| 10/11 | RoomListRequest → RoomList | Request/Reply | public rooms + private ones you belong to / are invited to; lobby first |
| 12 | RoomCreate | Request | `name` (≤32), `category`, `topic`, `private`; needs CHAT_CREATE_ROOM; creator auto-joins → RoomInfoReply |
| 13 | RoomJoin | Request | case-insensitive; `Forbidden` for uninvited private rooms / bans → RoomInfoReply |
| 14 | RoomLeave | Request | the lobby refuses; empty ad-hoc rooms are reaped |
| 15 | RoomInvite | Request | members only; target gets a `RoomInvited` push; an invite forgives a ban |
| 16 | RoomInvited | Push | `room`, `from` |
| 17 | RoomTopicSet | Request | creator or CHAT_MODERATE |
| 18 | RoomKick | Request | creator or CHAT_MODERATE; `ban` blocks rejoin; creators can't be kicked → target gets `RoomKicked` |
| 19 | RoomInfoReply | Reply (to 12/13) | `room: RoomInfo` {name, category, topic, private, member_count, created_by} |
| 20 | RoomKicked | Push | `room`, `banned` |
| 21/22 | RoomMembersRequest → RoomMemberList | Request/Reply | private rooms require membership |

`ChatSend`/`ChatMessage` are membership-scoped: pushes are delivered only
to member sessions (the lobby is everyone). Public-room scrollback is
open; private scrollback requires membership. Rooms are in-memory
(lobby permanent; persistence of operator rooms is future work).
