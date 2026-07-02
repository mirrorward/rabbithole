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

## Ratings & the index

Ratings are one row per (node, account); the average and count are computed
on read, so re-rating just overwrites. Search is a substring match over
name/comment/uploader against the `file_nodes` projection (the "background
indexer" is the projection itself); FTS5 can slot in behind the same repo
API later. This projection also feeds the cross-server catalog in Wave 5.
