-- Wave 3.2: the Wishing Well — a request board for wanted files/boards/
-- features, with voting, claiming, and fulfillment.

CREATE TABLE wishes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 0 file, 1 board, 2 feature, 3 other.
    kind         INTEGER NOT NULL,
    title        TEXT NOT NULL,
    details      TEXT NOT NULL DEFAULT '',
    requester    TEXT NOT NULL,               -- persona@origin
    requester_id INTEGER NOT NULL,            -- account id (for edit rights)
    -- 0 open, 1 claimed, 2 fulfilled, 3 declined.
    status       INTEGER NOT NULL DEFAULT 0,
    claimed_by   TEXT,
    fulfillment  TEXT,                         -- link / note when fulfilled
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX wishes_status ON wishes(status, updated_at);

CREATE TABLE wish_votes (
    wish_id    INTEGER NOT NULL REFERENCES wishes(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL,
    PRIMARY KEY (wish_id, account_id)
) STRICT;
