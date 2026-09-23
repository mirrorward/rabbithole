-- 0.265.0: the order boards sit in.
--
-- Boards were read in slug order, so the only way to move one was to
-- rename it — and a slug is a board's identity, which addresses and
-- federation carry. A position of their own lets an operator arrange them
-- as a reader should meet them; ties still fall back to the slug, so a
-- burrow whose boards all sit at 0 reads exactly as it did.
ALTER TABLE boards ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

-- Keep what each burrow already had: the order they were read in.
UPDATE boards
   SET position = (
       SELECT COUNT(*) FROM boards AS other
        WHERE COALESCE(other.parent_slug, '') = COALESCE(boards.parent_slug, '')
          AND other.slug < boards.slug
   );
