-- RH-5: preserve substring search while indexing file metadata. A filtered
-- content view keeps folders/aliases out of rebuilds as well as live writes.
-- Trigram LIKE (not MATCH syntax) preserves the existing query contract.
-- https://www.sqlite.org/fts5.html#the_trigram_tokenizer
CREATE VIEW file_search_content AS
    SELECT id, name, comment, uploader FROM file_nodes WHERE kind = 1;

CREATE VIRTUAL TABLE file_search USING fts5(
    name, comment, uploader,
    content = 'file_search_content', content_rowid = 'id',
    tokenize = 'trigram', detail = 'none'
);

-- External-content indexes require explicit maintenance. These triggers
-- commit or roll back with the projection, including cascade deletions.
-- https://www.sqlite.org/fts5.html#external_content_tables
CREATE TRIGGER file_search_insert AFTER INSERT ON file_nodes WHEN new.kind = 1 BEGIN
    INSERT INTO file_search(rowid, name, comment, uploader)
        VALUES (new.id, new.name, new.comment, new.uploader);
END;
CREATE TRIGGER file_search_delete AFTER DELETE ON file_nodes WHEN old.kind = 1 BEGIN
    INSERT INTO file_search(file_search, rowid, name, comment, uploader)
        VALUES ('delete', old.id, old.name, old.comment, old.uploader);
END;
CREATE TRIGGER file_search_update AFTER UPDATE OF name, comment, uploader, kind ON file_nodes BEGIN
    INSERT INTO file_search(file_search, rowid, name, comment, uploader)
        SELECT 'delete', old.id, old.name, old.comment, old.uploader WHERE old.kind = 1;
    INSERT INTO file_search(rowid, name, comment, uploader)
        SELECT new.id, new.name, new.comment, new.uploader WHERE new.kind = 1;
END;

-- Backfill files already stored before the upgrade, in the migration's
-- transaction; an index failure cannot leave a half-upgraded library.
INSERT INTO file_search(file_search) VALUES ('rebuild');
