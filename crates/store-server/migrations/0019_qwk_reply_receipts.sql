-- Successful QWK REP imports, scoped to the uploading account and the
-- resolved board in this server database. This table is the QWK namespace;
-- other gateways' identical content must never suppress a user's reply.
CREATE TABLE qwk_reply_receipts (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    board_slug  TEXT NOT NULL COLLATE NOCASE,
    digest      BLOB NOT NULL CHECK(length(digest) = 32),
    event_id    BLOB NOT NULL CHECK(length(event_id) = 32),
    accepted_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, board_slug, digest)
) STRICT;
CREATE INDEX qwk_reply_receipts_expiry ON qwk_reply_receipts(accepted_at);

-- No foreign key to posts: pruning a thread must not make an old upload
-- fresh again. Receipts expire after 30 days, lazily on the next import.
