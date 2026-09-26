-- A door's daily budget belongs to the account, not a persona or connection.
-- Active reservations are prepaid in charged_ms, so reconnects, concurrent
-- sessions and crashes cannot spend the same remaining allowance twice.
CREATE TABLE door_daily_usage (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    door_id TEXT NOT NULL,
    utc_day INTEGER NOT NULL,
    charged_ms INTEGER NOT NULL CHECK (charged_ms >= 0),
    PRIMARY KEY (account_id, door_id, utc_day)
) STRICT;

CREATE TABLE door_daily_reservations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    door_id TEXT NOT NULL,
    utc_day INTEGER NOT NULL,
    granted_ms INTEGER NOT NULL CHECK (granted_ms > 0),
    FOREIGN KEY (account_id, door_id, utc_day)
        REFERENCES door_daily_usage(account_id, door_id, utc_day) ON DELETE CASCADE
) STRICT;
