-- K1-6a-G §2.1 F3: a module's enable epoch, and what the daemon last saw
-- of its enablement.
--
-- `epoch` advances whenever the module is (re-)enabled: through a route or
-- the CLI, at a stop, or when a daemon start finds it enabled after last
-- seeing it disabled, never, or with another os.json entry. Each value is a
-- fresh generation from `module_consent_generations`, so it only grows and
-- is never reused. A grant holds only in the epoch it was written in.
--
-- `observed_enabled` and `observed_entry` (the digest of the module's
-- os.json entry and its enable mark) are what the last observation saw;
-- NULL entry is none seen yet.
CREATE TABLE module_enable_epochs (
    module           TEXT PRIMARY KEY,
    epoch            INTEGER NOT NULL DEFAULT 0,
    observed_enabled INTEGER NOT NULL DEFAULT 0,
    observed_entry   TEXT
);

-- The epoch a decision was written in; rows from before this migration
-- carry 0, which matches a module no observation has reached yet.
ALTER TABLE module_consents ADD COLUMN enable_epoch INTEGER NOT NULL DEFAULT 0;
