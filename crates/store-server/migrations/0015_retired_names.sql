-- 0.264.0: names that belonged to an account somebody removed.
--
-- Board authorship is a name, not a link: a post carries "handle@origin",
-- and the burrow lets the person of that name edit or withdraw it. If a
-- removed account's login or personas went back into the pool, whoever
-- took the name next would inherit the byline on everything that person
-- ever wrote, and the right to change it. So the names are kept out of
-- use instead, and what was written stays theirs.
CREATE TABLE retired_names (
    name TEXT PRIMARY KEY COLLATE NOCASE,
    -- The login the name belonged to, for an operator reading the record.
    was  TEXT NOT NULL,
    at   INTEGER NOT NULL
) STRICT;
