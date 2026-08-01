-- Authorization lifecycle generation. Existing bearer rows predate this
-- binding and are deliberately invalidated during migration.
ALTER TABLE accounts ADD COLUMN authz_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN authz_epoch INTEGER NOT NULL DEFAULT 0;
DELETE FROM sessions;
