-- Wave 0: schema bootstrap.
-- Real domain tables (accounts, personas, classes, sessions, rooms, …)
-- arrive with their waves; this migration proves the harness and pins
-- global pragmas/conventions.

CREATE TABLE server_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

INSERT INTO server_meta (key, value) VALUES ('schema_epoch', 'wave-0');
