-- REP has no export nonce: the caller must supply the exact QWK export id.
-- Board/event identities deliberately outlive deletion of their source rows,
-- so deleting and recreating a slug can never redirect an old packet.
CREATE TABLE qwk_exports (
    id TEXT PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    bbs_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX qwk_exports_account ON qwk_exports(account_id, created_at DESC, id);
CREATE INDEX qwk_exports_expiry ON qwk_exports(expires_at);
CREATE TABLE qwk_export_boards (
    export_id TEXT NOT NULL REFERENCES qwk_exports(id) ON DELETE CASCADE,
    conference INTEGER NOT NULL,
    board_id INTEGER NOT NULL,
    slug TEXT NOT NULL,
    PRIMARY KEY(export_id, conference)
);
CREATE TABLE qwk_export_refs (
    export_id TEXT NOT NULL,
    conference INTEGER NOT NULL,
    number INTEGER NOT NULL,
    event_id BLOB NOT NULL CHECK(length(event_id) = 32),
    PRIMARY KEY(export_id, conference, number),
    FOREIGN KEY(export_id, conference) REFERENCES qwk_export_boards(export_id, conference) ON DELETE CASCADE
);
