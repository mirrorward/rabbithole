-- A board's identity is ASCII case-insensitive, but projection lookups are
-- exact. Repair imported spellings using the stored board slug, never by
-- rewriting signed event blobs, ids, author/origin provenance, or content.
-- Unknown boards remain untouched. This repair does not prune any history;
-- the next normal post enforces the board's configured retention cap.
UPDATE posts
   SET board_slug = (SELECT b.slug FROM boards b WHERE b.slug = posts.board_slug)
 WHERE EXISTS (
     SELECT 1 FROM boards b
      WHERE b.slug = posts.board_slug
        AND b.slug COLLATE BINARY <> posts.board_slug
 );

UPDATE board_followups
   SET board_slug = (SELECT b.slug FROM boards b WHERE b.slug = board_followups.board_slug)
 WHERE EXISTS (
     SELECT 1 FROM boards b
      WHERE b.slug = board_followups.board_slug
        AND b.slug COLLATE BINARY <> board_followups.board_slug
 );

-- An edit arriving before a reply used the reply id as its temporary root.
-- Once the target exists, its real root must own the follow-up for retention.
-- Leave unresolved targets, unknown/cross-board rows and the applied flag alone.
UPDATE board_followups
   SET root_id = (
       SELECT COALESCE(p.root_id, p.event_id) FROM posts p
        WHERE p.event_id = board_followups.target_id
   )
 WHERE EXISTS (
     SELECT 1 FROM posts p JOIN boards b ON b.slug = p.board_slug
      WHERE p.event_id = board_followups.target_id
        AND b.slug = board_followups.board_slug
        AND board_followups.root_id <> COALESCE(p.root_id, p.event_id)
 );
