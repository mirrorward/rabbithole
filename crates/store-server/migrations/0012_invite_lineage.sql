-- Wave 13: invite lineage. Records who invited each account so an operator can
-- trace — and act on — an invite subtree (e.g. disable a spammer's whole
-- downline). NULL = open registration, the first account, or a pre-lineage
-- account. Invites already track `created_by` (the inviter) and `used_by` (the
-- redeemer, now finalised to the real account id instead of the 0 placeholder).
ALTER TABLE accounts ADD COLUMN invited_by INTEGER;
CREATE INDEX accounts_invited_by ON accounts(invited_by);
