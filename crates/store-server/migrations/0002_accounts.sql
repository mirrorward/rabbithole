-- Wave 1: accounts, classes, sessions, ACLs, audit log.

-- Permission classes (KDX lesson): rights live on the class; every member
-- inherits changes instantly. base_mask is a u64 capability bitmask stored
-- in SQLite's i64 (bit-cast).
CREATE TABLE classes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT NOT NULL UNIQUE,
    base_mask INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE accounts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    login       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- Argon2id PHC string; NULL for key-only accounts (Wave 2).
    phc         TEXT,
    screen_name TEXT NOT NULL,
    -- Role ordinal: 0 guest, 1 user, 2 moderator, 3 admin, 4 superuser.
    role        INTEGER NOT NULL DEFAULT 1,
    class_id    INTEGER REFERENCES classes(id) ON DELETE SET NULL,
    -- Per-account overrides layered onto the class mask.
    grant_mask  INTEGER NOT NULL DEFAULT 0,
    revoke_mask INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    disabled    INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Resumable sessions. Only the blake3 hash of the token is stored, so a
-- database leak does not leak live sessions.
CREATE TABLE sessions (
    token_hash BLOB PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
) STRICT;
CREATE INDEX sessions_account ON sessions(account_id);
CREATE INDEX sessions_expiry ON sessions(expires_at);

-- ACL entries: nearest-ancestor evaluation, deny wins at the same level.
-- resource is a /-separated path (e.g. "files/uploads/dropbox").
-- principal_kind: 0 everyone, 1 role, 2 class, 3 account.
CREATE TABLE acl_entries (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    resource       TEXT NOT NULL,
    principal_kind INTEGER NOT NULL,
    principal_id   INTEGER NOT NULL DEFAULT 0,
    allow_mask     INTEGER NOT NULL DEFAULT 0,
    deny_mask      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (resource, principal_kind, principal_id)
) STRICT;
CREATE INDEX acl_resource ON acl_entries(resource);

-- Append-only audit log of privileged actions.
CREATE TABLE audit_log (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    at     INTEGER NOT NULL,
    actor  TEXT NOT NULL,
    action TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT ''
) STRICT;

-- Seed classes. Masks are set by server-core at first boot (the SQL layer
-- doesn't know capability bit meanings).
INSERT INTO classes (name, base_mask) VALUES
    ('guest', 0),
    ('member', 0),
    ('moderator', 0),
    ('admin', 0),
    ('superuser', 0);
