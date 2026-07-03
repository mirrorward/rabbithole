-- Wave 9.x: board follow-up events (Edit / Tombstone) as stored, floodable
-- signed events. Posts live in `posts` (with their signed blob); their
-- follow-ups live here so the federation flood can advertise + serve them the
-- same way, and so out-of-order delivery (an edit arriving before its post)
-- can be reconciled when the target lands.

CREATE TABLE board_followups (
    event_id   BLOB PRIMARY KEY,          -- blake3 content id of the follow-up
    target_id  BLOB NOT NULL,             -- the post it edits/retracts
    root_id    BLOB NOT NULL,             -- target's thread root (retention cascade)
    board_slug TEXT NOT NULL,
    kind       INTEGER NOT NULL,          -- 1 = edit, 2 = tombstone
    origin     TEXT NOT NULL,             -- origin server name (flood origin-key path)
    -- 0 until the projection has been caught up: a follow-up delivered before
    -- its target post is stored here and applied when the post arrives.
    applied    INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,          -- unix ms
    -- The full signed event, postcard-encoded (federation source of truth).
    event_blob BLOB NOT NULL
) STRICT;
CREATE INDEX board_followups_target ON board_followups(target_id);
CREATE INDEX board_followups_root ON board_followups(root_id);
CREATE INDEX board_followups_board ON board_followups(board_slug, created_at);
