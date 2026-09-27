# Hotline upload checkpoints

Hotline single-file uploads persist their DATA fork under
`legacy-upload-staging/` in the burrow data directory. An authenticated
UploadFile request claims a checkpoint before the server returns its reference
and optional RFLT offset. That lease remains exclusive while the reference is
pending and while its HTXF transfer runs. Another request cannot reset or claim
that destination until the lease is released. Pending references expire after
ten minutes and are pruned on later negotiations or use; reference numbers
remain transient. After a restart, log in and negotiate another upload.

The checkpoint identifies the protocol, account ID, area ID, folder ID and file
name. Hotline and ZMODEM checkpoints cannot substitute for one another. Area
aliases resolve to the stored canonical spelling before permission checks.
Publication requires the original area and folder identities and paths in the
same database insertion; moving or replacing a folder cannot redirect bytes.
A fresh upload request resets only an idle checkpoint. A resume request returns
the committed DATA length, or starts at zero when no valid checkpoint remains.

Hotline resume requests commonly omit TRANSFER_SIZE. The first DATA header
therefore binds the complete DATA length in the authenticated checkpoint. A
resumed DATA fork must contain exactly the remaining length. INFO is resent by
the client, and its final type/comment metadata is used as before. There is no
remote whole-file hash in this handshake: matching length and target do not
prove that a client selected the same local file. The metadata MAC and prefix
hash protect the local checkpoint's integrity, not remote authenticity.

Each bounded DATA read is flushed with its checkpoint before the receiver
continues. HTXF has no per-chunk acknowledgement; an interrupted connection's
server FIN follows its final checkpoint. A complete upload is published only
after every declared fork arrives. The receiver accepts exactly one DATA fork,
at most one INFO fork, and at most eight forks total. INFO is capped at 64 KiB;
resource and other auxiliary forks are drained with an aggregate 64 MiB cap.
Compressed forks, duplicate DATA, oversized claims and malformed envelopes are
refused. A truncated later fork cannot publish an otherwise complete DATA fork;
the committed prefix remains available to a valid subsequent transfer.

Current account status, class rights and FILE_UPLOAD permissions are checked
at negotiation, HTXF start and final publication. Quota, file-size, denied-hash
and no-clobber checks still apply at publication; quarantine visibility is
unchanged. Definitive rejection and success remove the checkpoint. Transport
interruption or a database/blob storage failure preserves it for retry. A
checkpoint storage error suspends staging until repair and restart.

Hotline and ZMODEM share the existing bounds: 64 MiB per DATA upload, 256 MiB
aggregate with pending/active leases reserving their admitted cap, 128 records,
and 4 KiB of authenticated metadata per record. Progress extends a checkpoint's
30-minute expiry; negotiation and length checks do not. Expired/invalid records
are cleaned at startup and lazily on later claims. Existing version-one ZMODEM
checkpoints remain readable. Use one active Burrow instance per data directory,
including embedded servers in the same process. An embedded restart must wait
for old transfer tasks to end before starting another instance; the staging
store does not provide a cross-instance or cross-process lock.
Checkpoints survive daemon restart but are excluded from operator backups.
Unix also flushes directory changes; no cross-platform machine-power-loss
promise is made.

The control/RFLT conventions follow the existing
[Mobius UploadFile implementation documentation](https://pkg.go.dev/github.com/jhalter/mobius@v0.23.1/internal/mobius#HandleUploadFile)
and [flattened-file codec](../crates/legacy-hotline/src/flatten.rs). Automated
fixtures use real control and HTXF TCP connections, including an abruptly killed
child daemon. Real vintage-client interoperability remains separate evidence.
