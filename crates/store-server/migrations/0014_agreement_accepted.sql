-- Which agreement each person has accepted here, by the hash of the text
-- they were shown. Accepting is remembered across reconnects and restarts;
-- an operator who changes the wording is asking again, which is the point
-- of an agreement, so the hash is what is stored rather than a flag.
ALTER TABLE accounts ADD COLUMN agreed_hash BLOB;
ALTER TABLE accounts ADD COLUMN agreed_at INTEGER;
