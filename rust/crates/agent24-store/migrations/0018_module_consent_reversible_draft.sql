-- L-APPR-5 / K1-6a.5: the consent row must record whether the permission
-- summary the user accepted included reversible-draft semantics. The scope
-- fingerprint already hashes this bit, but the gate has to read the consented
-- fact back. A later request's reversible_draft_declared is only a claim.
--
-- A new migration, not an edit to the already-shipped 0015_module_consents.sql
-- (that file may already have run against a real database). DEFAULT 0
-- backfills every existing row as "not a draft consent", so an old grant
-- cannot start qualifying for the draft exemption.
ALTER TABLE module_consents ADD COLUMN reversible_draft INTEGER NOT NULL DEFAULT 0
    CHECK (reversible_draft IN (0, 1));
