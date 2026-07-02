# RHP File Family (5)

Status: **Wave 4.1** — file libraries: areas, a folder/file/alias tree,
metadata (icons, comments, uploader, dates, download counters, ratings),
hide-vs-deny ACLs, drop boxes, and indexed search. Bytes are content-
addressed in the blob store.

Small-blob transfer (avatars, banners, theme assets) shares this family at
**types 100+** (see [`blob`](../../crates/proto/src/blob.rs)); the file
library uses the low type numbers. Large-file streaming with resume is
**Wave 4.2** — until then, uploads/downloads ride the control stream inline
(server caps them well under the 1 MiB frame limit).

| type | name | direction | payload |
|---|---|---|---|
| 1/2 | AreaListRequest → AreaList | Request/Reply | libraries {slug, title, description}; needs FILE_LIST |
| 3/4 | FolderListRequest → NodeList | Request/Reply | `area`, `path` (None = root); folders first |
| 5/6 | NodeGet → NodeReply | Request/Reply | one node's metadata |
| 7/8 | AreaCreate → AreaReply | Request/Reply | needs FILE_MANAGE |
| 9/6 | FolderCreate → NodeReply | Request/Reply | `is_dropbox` = write-only; needs FILE_MANAGE |
| 10/6 | FileUpload → NodeReply | Request/Reply | inline bytes → blob; needs FILE_UPLOAD |
| 11/12 | FileDownloadRequest → FileContent | Request/Reply | bumps counter; needs FILE_DOWNLOAD |
| 13 | NodeDelete | Request | uploader or FILE_MANAGE |
| 14/6 | SetMetadata → NodeReply | Request/Reply | icon/comment; uploader or FILE_MANAGE |
| 15/16 | SearchRequest → SearchResults | Request/Reply | name/comment/uploader substring; FILE_LIST |
| 17/6 | RateFile → NodeReply | Request/Reply | 1..5, one per account; FILE_DOWNLOAD |
| 18/6 | AliasCreate → NodeReply | Request/Reply | link to an existing node; FILE_MANAGE |
| 19 | FileAdded | Push | `area`, `id` — broadcast so listings/search stay live |

## The tree

Each area holds a tree of nodes (`kind`: 0 folder / 1 file / 2 alias) keyed
by a slash-joined virtual `path` unique within the area. Files reference a
blake3 `blob_id` in the content-addressed store plus denormalized metadata
(size, mime, icon, comment, uploader, download counter). Aliases carry a
`target_id`; downloads and `resolve` follow one alias hop. Deleting a node
cascades to its children and any aliases pointing at it.

## Permissions

Resources are `files/<area>` and `files/<area>/<path>`, evaluated with the
standard nearest-ancestor, deny-wins, hide-vs-deny ACLs. Capabilities:
`FILE_LIST` (browse/search), `FILE_DOWNLOAD`, `FILE_UPLOAD`, `FILE_MANAGE`
(create areas/folders/aliases, edit/delete anyone's nodes), `DROPBOX_VIEW`.

**Drop boxes** are folders flagged write-only: anyone with `FILE_UPLOAD`
can drop files in, but the contents are hidden — `FolderListRequest`
returns an empty list and downloads are refused — unless the caller holds
`DROPBOX_VIEW` (or `FILE_MANAGE`). The classic Hotline upload folder.

## Bulk transfers (Wave 4.2)

Inline `FileUpload`/`FileDownload` (above) are for *small* files. Real
transfers negotiate a **ticket** on the control stream, then move bytes in a
resumable, integrity-checked way. Messages (types 20-42):

| type | name | direction | payload |
|---|---|---|---|
| 20/21 | TransferOpen → TransferTicket | Request/Reply | download `node_id`, or upload dest + `size`/`root`; ticket has `transfer_id`, `server_have`, `token` |
| 22/21 | TransferResume → TransferTicket | Request/Reply | re-authorize after reconnect; re-reports `server_have` |
| 23 | UploadFinish | Request → `NodeReply` | verify staged blake3 == root, commit to blob store, record node |
| 24 | TransferAbort | Request → ack | drop ticket + staging |
| 25/26 | FolderManifestRequest → FolderManifest | Request/Reply | whole subtree in one round trip (pipelining) |
| 30 | BulkPreamble | (stream) | length-prefixed first bytes on a dedicated QUIC bulk stream |
| 40/41 | FileChunkRequest → FileChunk | Request/Reply | ranged download (WS / control-stream path) |
| 42 | FileChunkPut | Request → ack | ranged upload |

**Transports.** On QUIC, `Connection::bulk()` yields a `BulkStreams` handle:
the client opens a dedicated bi-stream, writes a length-prefixed
`BulkPreamble` (binding it to the ticket), then bytes flow **off the control
channel** — the server streams the range (download) or consumes the
remainder into staging then acks one byte so the client knows staging is
durable before `UploadFinish` (upload). On WebSocket/wasm there are no extra
streams, so the same byte ranges ride the control connection as windowed
`FileChunk` frames (bounded well under the 1 MiB cap so chat/presence still
interleave). One transfer protocol, one verification path, two framings.

**Resume.** A download resumes from the client's local partial offset; an
upload resumes from the server's verified staged prefix (`server_have`).
Either way the finished file is verified whole against its blake3 root
(== blob id) before it's accepted — so a partial transfer is always safe to
continue and a corrupt one is rejected.

**Verification scope.** W4.2 verifies the whole-file root against the
authenticated origin server. Per-chunk Bao merkle verification — needed when
bytes come from *untrusted* peers — lands with the swarm in Wave 5, over the
same byte ranges (`bao-tree`).

## Ratings & the index

Ratings are one row per (node, account); the average and count are computed
on read, so re-rating just overwrites. Search is a substring match over
name/comment/uploader against the `file_nodes` projection (the "background
indexer" is the projection itself); FTS5 can slot in behind the same repo
API later. This projection also feeds the cross-server catalog in Wave 5.
