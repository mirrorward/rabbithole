# RHP DM Family (3)

Status: **Wave 2.2** — 1:1 persona-addressed threads, persisted with
offline delivery.

| type | name | direction | payload |
|---|---|---|---|
| 1 | DmSend | Request | `to`, `text`, `quote_of?`, `attachments: [[u8;32]]` (≤8 blob refs, uploaded via BlobPut first) |
| 2 | DmSent | Reply | `id`, `at_unix_ms` |
| 3 | DmReceived | Push | full `DmMessage` (live, or on login for queued mail; `is_auto` marks away auto-responses) |
| 4/5 | DmHistoryRequest → DmHistory | Request/Reply | `with`, `before_id` (0 = newest), `limit` ≤200; oldest-first pages |
| 6/7 | DmThreadsRequest → DmThreads | Request/Reply | conversations with `unread` counts, newest first |
| 8 | DmMarkRead | Request | `with`, `up_to_id` |
| 9 | DmReadReceipt | Push | sent only if the reader's account has receipts on |

Semantics: blocked pairs answer `Forbidden` in both directions without
revealing which side blocked. If every session of the recipient is
away/idle with a status message, the sender gets it back once per away
period as an `is_auto` DM. Unread mail (≤100) is pushed right after the
Welcome on sign-in; DMs never ride the replay ring (the store is the
durable queue).
