-- Wave 4.1: file libraries — areas, folder trees, file metadata, drop
-- boxes, aliases, and per-account ratings. Bytes live in the content-
-- addressed blob store (blake3); this schema is the browsable/searchable
-- projection over them.

-- Top-level libraries. slug is a single token (e.g. "warez", "docs").
CREATE TABLE file_areas (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL
) STRICT;

-- The tree: folders, files, and aliases. `path` is the slash-joined
-- virtual path within an area (unique per area), e.g. "utils/zip.lha".
-- kind: 0 folder, 1 file, 2 alias.
CREATE TABLE file_nodes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    area_id      INTEGER NOT NULL REFERENCES file_areas(id) ON DELETE CASCADE,
    parent_id    INTEGER REFERENCES file_nodes(id) ON DELETE CASCADE,
    kind         INTEGER NOT NULL,           -- 0 folder, 1 file, 2 alias
    name         TEXT NOT NULL,
    path         TEXT NOT NULL,              -- virtual path within the area
    -- Folder-only: write-only drop box (contents hidden without DROPBOX_VIEW).
    is_dropbox   INTEGER NOT NULL DEFAULT 0,
    -- File-only: content reference + metadata.
    blob_id      BLOB,                       -- blake3 content id (in BlobStore)
    size         INTEGER NOT NULL DEFAULT 0,
    mime         TEXT NOT NULL DEFAULT '',
    icon         TEXT NOT NULL DEFAULT '',   -- retro icon key or custom ref
    comment      TEXT NOT NULL DEFAULT '',
    uploader     TEXT NOT NULL DEFAULT '',   -- persona@origin
    uploader_id  INTEGER,                    -- account id (for edit rights)
    downloads    INTEGER NOT NULL DEFAULT 0,
    -- Alias-only: the node this alias points at.
    target_id    INTEGER REFERENCES file_nodes(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    UNIQUE (area_id, path)
) STRICT;
CREATE INDEX file_nodes_children ON file_nodes(area_id, parent_id);
CREATE INDEX file_nodes_blob ON file_nodes(blob_id);
CREATE INDEX file_nodes_name ON file_nodes(name);

-- Per-account ratings (1..5), one per node, so the average is honest.
CREATE TABLE file_ratings (
    node_id    INTEGER NOT NULL REFERENCES file_nodes(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL,
    stars      INTEGER NOT NULL,
    PRIMARY KEY (node_id, account_id)
) STRICT;
