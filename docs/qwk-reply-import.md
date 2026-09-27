# Importing offline replies

`qwk-build` returns an `export_id` with its packet path and member list. Keep
that id with the downloaded QWK packet. Import the corresponding REP ZIP with
the same account and id:

```json
{"cmd":"qwk-build","login":"alice"}
{"cmd":"qwk-ingest","login":"alice","path":"/incoming/WARREN.REP","export_id":"<id from that build>"}
```

These are requests to the existing local operator control interface. This does
not add a web upload, native upload, HTTP file-serving route, or ZMODEM handoff.
The response reports accepted and duplicate counts and rejected reply subjects
with reasons. A malformed archive fails before any reply is posted. A valid
archive may contain both accepted and rejected messages.

Each successful build saves a durable account-owned routing snapshot. Numeric
conferences name the original board id and canonical slug; nonzero reply
references name the original post event id. Adding boards or changing article
ordering cannot reroute an old reply. Missing, foreign, expired, or evicted
exports fail closed. Deleted/recreated boards and missing, tombstoned, held, or
unexported parent references reject the affected reply instead of turning it
into a new thread. Current account, class, and board posting permissions still
apply. Authors and signatures come from the uploading account, not the REP
From field. Existing durable account/board content deduplication remains in use.

Classic REP does not echo a unique export nonce. The operator must supply the
id for the packet actually used by the reader; the server cannot prove that a
same-account id supplied by a caller belongs to those bytes. The BBS id in both
the member filename and the message header must match that export. Server name
changes do not invalidate an otherwise-current snapshot.

At most eight usable export snapshots are retained per account; each expires
after 30 days. Each holds
at most 255 conferences and 2000 reference identities (up to 1000 packed
messages plus their emitted parent references). Expired snapshots cannot be
used; the next successful build removes expired/evicted snapshots and their
artifact directories. Files live under `<qwk_spool_dir>/<login>/<export_id>/`,
so concurrent or later builds do not overwrite an earlier packet. The routing
snapshot and read-pointer advances commit in one transaction only after every
spool write succeeds. Admission rechecks board identity, and reply commit
rechecks export/account/board/parent identity and current expiry under the
SQLite write lock.

A process crash or uncertain database commit can leave an unregistered spool
directory. Such files are deliberately retained: deleting them on cancellation
could delete a packet whose snapshot and read pointers already committed.
Operator cleanup may remove unregistered directories while the server is
stopped; do not remove directories merely because an in-progress build has not
registered them yet. Failed eviction cleanup is logged and may likewise need
operator cleanup. These exceptional files are not covered by the eight-export
artifact bound.

## ZIP support and limits

The reader follows the classic records in
[PKWARE APPNOTE 6.3.10](https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT)
(§§4.3.7, 4.3.9, 4.3.12, 4.3.16, 4.4). It accepts STORE (0) and raw DEFLATE (8),
including data descriptors with or without their optional signature. The
expected `<BBSID>.MSG` name is ASCII and case-insensitive. At most 32 unique flat
ASCII names are permitted; paths, drive names, directories, symlinks, encrypted,
ZIP64, split, executable-prefixed, and other-method archives are refused.

Archive and individual member limits are 8 MiB; all declared uncompressed sizes
sum to at most 16 MiB; at most 1000 replies are decoded. The control interface
reads at most 8 MiB plus one byte from its opened file handle. Local and central
headers, descriptor fields, offsets, and nonoverlapping record coverage are
validated for every entry. Only the expected MSG member is decompressed and
checked for exact size, CRC, and complete DEFLATE termination; extra safe flat
members receive structural checks and are otherwise ignored. Nothing is
extracted to a filesystem path from an archive name.

## Raw-member compatibility

`ingest_rep_for` remains the low-level API for trusted, already-extracted MSG
bytes. Through ctl, this compatibility path requires a `.MSG` filename and no
`export_id`; ZIP-looking bytes are refused. It retains the historical current
conference/article map and permissive unresolvable-reference behavior, and
does not provide archive provenance validation. Use the archive API for actual
REP uploads. A malformed ZIP never falls back to this raw path.

Local scripted ZIP/REP fixtures verify codec and server behavior. They do not
claim interoperability with a particular external offline reader.
