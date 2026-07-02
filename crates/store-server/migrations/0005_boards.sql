-- Wave 3.1: message bases (boards) + signed post events + read pointers.

-- The board tree: categories/bundles hold children; boards hold posts.
-- kind: 0 category, 1 bundle, 2 board. slug is dotted (e.g. rabbit.general).
CREATE TABLE boards (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    kind        INTEGER NOT NULL,
    parent_slug TEXT,
    -- Retention: keep at most this many top-level threads (0 = unlimited).
    max_threads INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX boards_parent ON boards(parent_slug);

-- Posts are append-only signed events. The signed blob is the source of
-- truth; the columns are a denormalized projection for querying/threading.
CREATE TABLE posts (
    event_id   BLOB PRIMARY KEY,          -- blake3 content id
    board_slug TEXT NOT NULL,
    root_id    BLOB,                       -- thread root (NULL for top-level)
    parent_id  BLOB,                       -- immediate parent
    author     TEXT NOT NULL,             -- persona@server
    author_key BLOB NOT NULL,
    origin     TEXT NOT NULL,
    subject    TEXT NOT NULL,
    body       TEXT NOT NULL,
    mime       TEXT NOT NULL,
    created_at INTEGER NOT NULL,          -- unix ms
    -- Superseded/retracted state, set by Edit/Tombstone follow-ups.
    edited     INTEGER NOT NULL DEFAULT 0,
    tombstoned INTEGER NOT NULL DEFAULT 0,
    -- The full signed event, postcard-encoded (federation source of truth).
    event_blob BLOB NOT NULL
) STRICT;
CREATE INDEX posts_board ON posts(board_slug, created_at);
CREATE INDEX posts_root ON posts(root_id);

-- Per-account high-water read marks (also feed QWK lastread + NNTP in W10).
CREATE TABLE read_marks (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    board_slug  TEXT NOT NULL,
    -- Highest created_at (unix ms) the account has read in this board.
    last_read_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, board_slug)
) STRICT;
