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
