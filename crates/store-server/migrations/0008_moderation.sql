-- Wave 13: the moderation suite — report queues, quarantine-for-review,
-- and blake3 hash-deny lists. The audit trail rides the existing
-- audit_log table (0002).

-- User reports. subject_ref is opaque bytes whose shape is fixed by
-- subject_kind: post = 32-byte event id, dm = 8-byte LE message id,
-- file = 32-byte blake3 blob hash, user = 8-byte LE account id.
-- state: 0 open, 1 reviewing (claimed), 2 resolved, 3 dismissed.
-- reporter_account has no FK: guests (negative ids) may report too.
CREATE TABLE reports (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    reporter_account INTEGER NOT NULL,
    subject_kind     INTEGER NOT NULL,
    subject_ref      BLOB NOT NULL,
    reason           TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    state            INTEGER NOT NULL DEFAULT 0,
    resolver         TEXT NOT NULL DEFAULT '',   -- moderator login; '' = none
    resolved_at      INTEGER,                    -- terminal states only
    resolution       TEXT NOT NULL DEFAULT ''    -- resolution / dismissal note
) STRICT;
CREATE INDEX reports_state ON reports(state, created_at);
CREATE INDEX reports_dedupe ON reports(reporter_account, subject_kind, subject_ref);

-- Content quarantined pending review: hidden from non-moderators on the
-- read/list paths that consult it. Keyed like reports (kind + ref bytes).
CREATE TABLE quarantine (
    subject_kind INTEGER NOT NULL,
    subject_ref  BLOB NOT NULL,
    reason       TEXT NOT NULL DEFAULT '',
    added_by     TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (subject_kind, subject_ref)
) STRICT;

-- blake3 hash-deny list: content that may never be (re)introduced. Checked
-- at upload finalize and attachment send.
CREATE TABLE deny_hashes (
    hash       BLOB PRIMARY KEY,               -- 32-byte blake3
    reason     TEXT NOT NULL DEFAULT '',
    added_by   TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
) STRICT;
