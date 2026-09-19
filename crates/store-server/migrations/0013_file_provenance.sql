-- Pulls between burrows: a file fetched from another burrow on a person's
-- behalf remembers where it came from. Empty/NULL for everything uploaded here.
ALTER TABLE file_nodes ADD COLUMN source_burrow TEXT NOT NULL DEFAULT '';
ALTER TABLE file_nodes ADD COLUMN source_key BLOB;

-- Grants already used for a pull here, until they would have lapsed anyway:
-- a grant is single-use, restart or no restart.
CREATE TABLE s2s_spent_grants (
    nonce        BLOB PRIMARY KEY,
    expires_unix INTEGER NOT NULL
) STRICT;
