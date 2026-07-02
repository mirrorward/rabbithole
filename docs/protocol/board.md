# RHP Board Family (4)

Status: **Wave 3.1** — message bases with a category/bundle/board tree,
append-only signed post events, threading, read pointers, moderation.

| type | name | direction | payload |
|---|---|---|---|
| 1/2 | BoardListRequest → BoardList | Request/Reply | tree nodes {slug, title, kind (0 cat/1 bundle/2 board), parent, unread} |
| 3/4 | ThreadListRequest → ThreadList | Request/Reply | `board`, `limit` (clamped to 200); summaries {root PostView, replies, last_activity} newest first |
| 5/6 | ThreadRequest → ThreadPosts | Request/Reply | `root`, `limit` (clamped to 1000); full thread (root + descendants, oldest first) |
| 7/8 | PostCreate → PostReply | Request/Reply | `board`, optional `parent`, subject/body/mime; needs BOARD_POST |
| 9/8 | PostEdit → PostReply | Request/Reply | author or BOARD_MODERATE |
| 10 | PostDelete | Request | tombstone; author or BOARD_MODERATE |
| 11 | MarkRead | Request | advance read pointer (`up_to_unix_ms`, 0 = now) |
| 12/13 | BoardCreate → BoardCreated | Request/Reply | admin (BOARD_MODERATE) builds the tree |
| 14 | PostPosted | Push | `board`, `id`, `root` — broadcast so unread counts stay live |

## Signed events

Every post is a `SignedEvent` (server-core): `id = blake3(canonical
EventCore)`, signed by both the author key and the origin server key.
The store keeps the postcard-encoded signed blob as the **federation
source of truth** plus a denormalized projection for querying. Edits and
tombstones are follow-up events (`EventBody::Edit`/`Tombstone`), never
mutations — Wave 9 floods all of these between servers unchanged.

Wave-3 author keys are derived deterministically from the server key +
account id; Wave 9 swaps in each account's enrolled Ed25519 identity key.
Top-level posts carry `root == self`. Retention: a board with
`max_threads > 0` drops its oldest overflow threads on new top-level posts.
