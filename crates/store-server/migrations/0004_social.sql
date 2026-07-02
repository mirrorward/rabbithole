-- Wave 2.2: buddy lists, blocks, direct messages, DM receipts pref.

CREATE TABLE buddies (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    screen_name TEXT NOT NULL COLLATE NOCASE,
    grp         TEXT NOT NULL DEFAULT 'Buddies',
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, screen_name)
) STRICT;

CREATE TABLE blocks (
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    blocked_account INTEGER NOT NULL,
    -- Display name at block time (personas can be renamed/deleted).
    screen_name     TEXT NOT NULL,
    added_at        INTEGER NOT NULL,
    PRIMARY KEY (account_id, blocked_account)
) STRICT;

CREATE TABLE dms (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_account  INTEGER NOT NULL,
    from_persona  TEXT NOT NULL,
    to_account    INTEGER NOT NULL,
    to_persona    TEXT NOT NULL,
    text          TEXT NOT NULL,
    quote_of      INTEGER,
    -- JSON array of hex blob ids.
    attachments   TEXT NOT NULL DEFAULT '[]',
    at            INTEGER NOT NULL,
    is_auto       INTEGER NOT NULL DEFAULT 0,
    read_at       INTEGER
) STRICT;
CREATE INDEX dms_to ON dms(to_account, id);
CREATE INDEX dms_thread ON dms(from_account, to_account, id);

-- Read-receipt opt-out lives on the account.
ALTER TABLE accounts ADD COLUMN dm_receipts INTEGER NOT NULL DEFAULT 1;
