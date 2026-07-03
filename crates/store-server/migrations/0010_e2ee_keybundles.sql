-- Wave 13: E2EE prekey bundles + opaque encrypted DM carriage.
--
-- The server stores ONLY public key material here; it never holds a private
-- key or any plaintext. One published bundle per account (the account's E2EE
-- identity), plus a pool of one-time prekeys the server hands out and deletes
-- one at a time.

CREATE TABLE e2ee_bundles (
    account_id        INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    identity_key      BLOB NOT NULL,   -- X25519 identity public (32 bytes)
    signing_key       BLOB NOT NULL,   -- Ed25519 verifying key (32 bytes)
    signed_prekey     BLOB NOT NULL,   -- X25519 signed prekey public (32 bytes)
    signed_prekey_sig BLOB NOT NULL,   -- Ed25519 signature over signed_prekey (64 bytes)
    updated_at        INTEGER NOT NULL
) STRICT;

CREATE TABLE e2ee_one_time_prekeys (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    prekey     BLOB NOT NULL,          -- X25519 one-time prekey public (32 bytes)
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX e2ee_otp_account ON e2ee_one_time_prekeys(account_id, id);

-- Opaque E2EE carriage for a DM row. NULL for the (unchanged) plaintext path;
-- a postcard-encoded EncryptedPayload blob for an end-to-end encrypted message,
-- whose `text` column is stored empty and is never indexed/searched.
ALTER TABLE dms ADD COLUMN encrypted BLOB;
