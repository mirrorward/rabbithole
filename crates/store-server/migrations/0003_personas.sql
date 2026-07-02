-- Wave 2.1: personas, invites, TOTP enrollment, identity keys.

CREATE TABLE personas (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    screen_name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    is_default        INTEGER NOT NULL DEFAULT 0,
    location          TEXT,
    interests         TEXT,
    quote             TEXT,
    plan              TEXT,
    pronouns          TEXT,
    avatar_hex        TEXT,
    banner_hex        TEXT,
    directory_visible INTEGER NOT NULL DEFAULT 1,
    created_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX personas_account ON personas(account_id);

-- Backfill: every existing account gets its screen_name as a default persona.
INSERT INTO personas (account_id, screen_name, is_default, created_at)
SELECT id, screen_name, 1, unixepoch() FROM accounts;

CREATE TABLE invites (
    code       TEXT PRIMARY KEY,
    created_by INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_by    INTEGER
) STRICT;

CREATE TABLE account_totp (
    account_id    INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    secret        BLOB NOT NULL,
    confirmed     INTEGER NOT NULL DEFAULT 0,
    -- JSON array of hex blake3 hashes of unused recovery codes.
    recovery_json TEXT NOT NULL DEFAULT '[]'
) STRICT;

CREATE TABLE account_keys (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    pubkey     BLOB NOT NULL UNIQUE,
    added_at   INTEGER NOT NULL
) STRICT;
