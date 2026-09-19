# RHP File Family (5)

Status: **Wave 4 complete** — file libraries (W4.1: areas, a
folder/file/alias tree, metadata, hide-vs-deny ACLs, drop boxes, indexed
search), the resumable transfer engine (W4.2), and quotas / rate policy /
the persistent client queue (W4.3). Bytes are content-addressed in the blob
store.

Small-blob transfer (avatars, banners, theme assets) shares this family at
**types 100+** (below); the file library uses types 1-19 and the transfer
engine 20-42.

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
| 27 | AreaUpdate | Request | FILE_MANAGE: `slug`, `title`, `description` → empty ack. The slug is in every path and download link and never changes; `BadRequest` for an empty title, `NotFound` for an unknown area |
| 28 | AreaDelete | Request | FILE_MANAGE: `slug` → empty ack. `BadRequest` while the area has anything in it: an area takes its whole tree with it |
| 29 | NodeRename | Request → `NodeReply` | `id`, `name`. The uploader of a file, or FILE_MANAGE on the area. In place; a folder's descendants keep their place under the new name (their paths are rewritten in one transaction). `BadRequest` for an empty name, one with a slash, `.` or `..`; `AlreadyExists` when something by that name is beside it |
| 30 | NodeMove | Request → `NodeReply` | `id`, `folder?` (`None` or empty: the area root). FILE_MANAGE on the area, checked where it is and where it goes. Same area only. `BadRequest` for a folder moved into itself or below, or a destination that is not a folder; `NotFound` for a destination that is not there; `AlreadyExists` when the destination has something by that name |
| 31/32 | UploadLimitsRequest → UploadLimits | Request/Reply | Any session: `max_file_bytes`, `quota_bytes`, `used_bytes` (0 = no limit; a guest's `used_bytes` is 0). Asked before a file is sent so a refusal can say which limit and by how much; a burrow that predates it answers `Unsupported`; its own limits still hold there |
| 33/34 | PullGrantRequest → PullGrantIssued | Request/Reply | At the **source**: `fetcher_key` (the destination's server key), `nodes` (files, or folders taken whole) → `grant` (opaque signed bytes), `files`, `bytes`, `skipped`, `expires_unix`. Only files the person may download: per-path `FILE_DOWNLOAD`, drop boxes they may not view refused or skipped, quarantined content never (and not counted, so it reads as absent). Two requested things of one name are told apart here (`tapes`, `tapes (2)`). The grant is signed for one hour, and the source keeps serving a pull that started in time for six hours past that. `Unsupported` when `s2s_grants_enabled` is off; `Unavailable` when the fetcher is this burrow, or is not an approved federation peer and `s2s_grants_to_any` is off; `Forbidden` when nothing is theirs to send; `NotFound` for a node that is gone, quarantined, or an empty folder; `BadRequest` for no nodes, over 100 nodes, over 1000 files, or a grant over 384 KiB encoded; `RateLimited` on the transfer budget |
| 35/36 | RemotePull → RemotePullAccepted | Request/Reply | At the **destination**: `grant`, `area`, `folder?` → `pull_id`, `files`, `bytes`, `source` (its federation name). Everything is checked before a byte moves: the grant's signature against the source key in this burrow's own peer registry, a live federation session with it, that the grant names this burrow, its expiry (`SessionExpired`) and single use (`AlreadyExists`), `FILE_UPLOAD` on the folder (`Forbidden`), and `FILE_MANAGE` there when the grant recreates folders, which never go into a drop box (`Forbidden`), the folder (`NotFound`), each file against the largest file (`TooLarge`) and the deny list (`Forbidden`), the total against the person's space and `s2s_max_bytes` (`TooLarge`; `Internal` if the space in use cannot be read), and `s2s_max_concurrent` or the burrow's 16 pulls at once (`RateLimited`). The grant's nonce is spent last, in the store, so a refusal leaves it usable and a restart does not make it good again. `Unsupported` when `s2s_pull_enabled` is off; `Unavailable` when the source is not an approved, connected peer and `s2s_pull_from_any` is off, or, after every other check, when this burrow could not reach the source at an address the grant carries (see FILE 39). Reaching a source that is not a peer is charged to the person's transfer budget (`RateLimited`) and takes the pull's place before anything is dialed |
| 39 | PullGrantAsk → PullGrantIssued | Request/Reply | At the **source**, as FILE 33 with `reach_host` too: the host the person's app reached this burrow at (a name or IP literal, no port). The grant then also carries this burrow's certificate fingerprint and up to four `host:port` addresses (`advertise_host` first, then `reach_host`, each with the QUIC client port), so a destination that is not a federation peer can connect to it: it resolves each address once, refuses private ones unless `s2s_private_addresses`, pins the certificate, proves its server key, and opens a pull session (SESSION 15). A host that is not a plain name or literal is left out. A grant for an approved peer stays in the first format (version 1: no certificate, no addresses), which releases before 0.227 read; every burrow reads both. Refusals as FILE 33 |
| 37 | RemotePullCancel | Request → ack | `pull_id`; the person's own running pull, else `NotFound`. Takes effect before the next file or chunk |
| 38 | RemotePullStatus | Push | To every session of the person who started it: `pull_id`, `state` (0 running, 1 done, 2 failed), `files_done`/`files_total`, `bytes_done`/`bytes_total`, `reason` (1 source refused, 2 source unreachable or too slow, 3 too large, 4 over space, 5 refused content, 6 did not verify, 7 cancelled, 8 internal, 9 this burrow stopped taking it: pulls switched off or the account closed), `missing` (files the source no longer had), `source`, `area`, `landed` (the path of the first thing filed). Progress is not replayed after a reconnect; the ending is |

## Small blobs (types 100+, Wave 2)

Avatars, banners, and theme assets — things comfortably under the 1 MiB
frame cap — ride the control stream inline:

| type | name | direction | payload |
|---|---|---|---|
| 100/101 | BlobPut → BlobRef | Request/Reply | `purpose` (enum: Avatar / Banner / ThemeAsset — servers enforce per-purpose size caps, `avatar_max_bytes`/`banner_max_bytes`), `bytes` → `id: [u8;32]` (blake3) or `TooLarge` |
| 102/103 | BlobGet → BlobData | Request/Reply | `id: [u8;32]` → `bytes` or `NotFound` |

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
| 20/21 | TransferOpen → TransferTicket | Request/Reply | open: `direction` (0 down / 1 up), download `node_id`, or upload dest (`area`/`parent`/`name`/`mime`/`comment`) + `size`/`root`; ticket: `transfer_id`, `root`, `size`, `server_have`, `token: [u8;16]`, `supports_bulk: bool` |
| 22/21 | TransferResume → TransferTicket | Request/Reply | `transfer_id`, `token`, `local_have` — re-authorize after reconnect; re-reports `server_have` |
| 23 | UploadFinish | Request → `NodeReply` | `transfer_id` — verify staged blake3 == root, commit to blob store, record node |
| 24 | TransferAbort | Request → ack | `transfer_id` — drop ticket + staging |
| 25/26 | FolderManifestRequest → FolderManifest | Request/Reply | `area`, `path?` → whole subtree in one round trip; entries {node_id, rel_path, root, size, mime} |
| — | BulkPreamble | (stream, **not a frame**) | first bytes on a dedicated QUIC bulk stream: a length-prefixed postcard `{transfer_id, token, offset, direction}` binding the raw stream to a ticket. It carries no message-type number — it never rides the control framing |
| 40/41 | FileChunkRequest → FileChunk | Request/Reply | `transfer_id`, `offset`, `len` → `{transfer_id, offset, last, bytes}` — ranged download (WS / control-stream path) |
| 42 | FileChunkPut | Request → ack | `{transfer_id, offset, last, bytes}` — ranged upload |

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

## Pull streams between burrows

A destination fetches what a grant (FILE 33/39) names over bulk streams:
its federation session's, or a pull session's (SESSION 15). One stream per
question, each opened by the destination with a length-prefixed postcard
frame (u32 big-endian length, as the transfer preamble):

- **A file's bytes**: `PullStreamRequest { grant, item, offset }`. The source
  answers one status byte (0 OK, 1 denied, 2 gone or changed, 3 off, 4 bad),
  then on OK the raw bytes from `offset` to the end. The grant must be the
  source's, name the fetching burrow, and still stand (six hours' grace for a
  pull under way); on a pull session, only the grant it was opened with; and
  the person who asked for it (whom the grant's nonce names, readable only
  by the source) must still have a working account there, checked again
  every 15 seconds while a file streams. A grant from before 0.228 names no
  one and is served without that check.
- **The swarm peers holding a file** (0.228): an **empty frame**, then
  `PullStreamAsk::Sources { grant, item }`. The source answers the status
  byte a request for that file's bytes would get, or 3 when it does not share
  its swarm (`s2s_swarm_sources`) or the person who asked for the grant may
  not, now, find and fetch from it; on OK, one frame of `PullSources { token,
  expires_unix, sources: [SwarmSource { endpoint, cert_fp }] }`: at most 16
  seeders that are visible, still connected and hold the whole file (not
  partial seeds, which a destination before 0.229 cannot use), each an
  IP address and port, and an `S2sCapToken` for the file naming the fetching
  burrow, good for ten minutes and never past the grant's serving time.
  A source from before 0.228 reads the empty frame as a bad request and
  answers 4, so asking costs an older source nothing.

The destination asks for sources only when `s2s_swarm` is on and the file
is at least two swarm units (2 MiB). It takes seeders only as addresses
(never names to look up), public unless `s2s_private_addresses`, at most 32
in one send and not again one that gave nothing; fetches the pieces with the
swarm engine; verifies every block and the whole file; asks again for a file
still coming when its token nears its end, resuming it while the swarm is
getting somewhere; and fetches from the source whatever the seeders do not
give. Once the swarm fails a file, or the source says it does not share it
or refuses, the rest of that send comes from the source alone. Files over
64 GiB always come from the source.

## Rate policy & the client queue (Wave 4.3)

Four server-side limits (`0` = unlimited), all live, settable from the
console's Files & transfers section or `ctl config set`:

| config key | scope | effect |
|---|---|---|
| `upload_max_file_bytes` | per file | the largest single file, **50 MiB by default** (Hotline and ZMODEM also stop at their own 64 MiB in-memory ceiling). Checked where the size is first declared (upload `TransferOpen`, inline `FileUpload`, Hotline's `TRANSFER_SIZE`, ZMODEM's header) and again on the bytes that arrived (`UploadFinish`, the Hotline and ZMODEM finalizers, and the streaming ceilings beneath them) → `TooLarge` |
| `upload_quota_bytes` | per account | total stored upload bytes; checked at the same points → `TooLarge`. Off by default |
| `max_concurrent_transfers` | per account | live tickets (up + down); a further `TransferOpen` is refused with `RateLimited` |
| `transfer_rate_bytes_per_sec` | per transfer | download bandwidth cap, applied to both the ranged-chunk and dedicated-stream paths |

A download has no explicit "finish" message, so its ticket is retired when
the last chunk is served (chunk path) or the dedicated stream drains (bulk
path); **session teardown** is the backstop, removing any ticket a session
left open and deleting its staging file — so an abandoned transfer never
permanently holds a concurrency slot. Per-class overrides of these limits are
a later refinement; today they are server-wide defaults.

**The client queue** (`rabbithole-store-client` `transfer_queue`, driven by
`rabbithole_core::queue::drain`) makes transfers durable and unattended:
enqueue downloads/uploads, and a driver runs them highest-priority-first,
marking each `ACTIVE`→`DONE`/`FAILED` and resuming from disk (download) or the
server's staged prefix (upload) across restarts. Bandwidth is capped
client-side via `Client::set_rate_limit`. The `rabbit queue`
(`get`/`put`/`list`/`run`/`pause`/`resume`/`prio`/`rm`/`clear`) subcommands
are the CLI surface; enqueue/list/prioritize work offline, `run` dials the
cached session and drains.

## Ratings & the index

Ratings are one row per (node, account); the average and count are computed
on read, so re-rating just overwrites. Search is a substring match over
name/comment/uploader against the `file_nodes` projection (the "background
indexer" is the projection itself); FTS5 can slot in behind the same repo
API later. This projection also feeds the cross-server catalog in Wave 5.
