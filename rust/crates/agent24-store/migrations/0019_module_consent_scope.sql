-- K1-6a-G §2.2: a grant's resources and artifact identity, and the write
-- generation that orders grants against revocations.
--
-- `resources` is the canonical resource set as a JSON array of strings
-- (deduplicated, sorted by bytes); NULL reads as the empty set, never as
-- "everything". `artifact` is the identity the user confirmed; NULL is
-- none, which a dispatch treats as stale.
ALTER TABLE module_consents ADD COLUMN resources TEXT;
ALTER TABLE module_consents ADD COLUMN artifact TEXT;

-- Every grant, refusal and revocation takes the next generation from here:
-- increasing and never reused (AUTOINCREMENT), independent of any clock.
-- A revocation covers a decision when its generation is at least the
-- decision's, so rolling the clock back cannot undo it. Rows from before
-- this migration carry 0 on both sides, so an old revocation still covers
-- an old decision.
CREATE TABLE module_consent_generations (
    generation INTEGER PRIMARY KEY AUTOINCREMENT
);
ALTER TABLE module_consents ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE module_consent_revocations ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
