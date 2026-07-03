-- Wave 8: per-account server-theme opt-out — the user safety valve for
-- server-applied theme bundles (ThemeGet serves defaults when set).

ALTER TABLE accounts ADD COLUMN theme_server_disabled INTEGER NOT NULL DEFAULT 0;
