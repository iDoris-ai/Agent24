-- ME4-4.2.3a / docs/design/ME4-S2-model-callback.md §6.1 (verbatim, frozen
-- v3.1). Per-module, per-day, per-tier aggregate of model callback usage
-- (`_a24/model/complete`, ME4-4.1.1). A brand-new table, so this migration
-- has nothing to migrate forward — unlike 0007, there is no "old rows stay
-- untouched" concern here.
--
-- Aggregate, not a ledger: one row per (module, day, served_by), updated
-- in place by an upsert (`Store::record_module_model_usage`) rather than
-- appended to — a per-call ledger would grow unboundedly (§6.1's own
-- reasoning: 30 calls/min/module for a year). The three queries the design
-- needs (by module, by day, by tier) are all answerable from this shape.
--
-- `day` is a UTC 'YYYY-MM-DD' string, length-CHECKed (not further validated
-- as a real calendar date — the caller always derives it from a real clock,
-- and a stricter CHECK would need SQLite date functions this project avoids
-- elsewhere in this crate).
--
-- The composite CHECK encodes §6.2's counting rule: `served_by = 'none'`
-- means "never reached a provider" (routed away, cancelled before it got
-- there, or rejected up front) — it is a per-call ok/failure counter with no
-- notion of a serving tier, so it may carry `calls_failed`/`calls_cancelled`
-- but never `calls_ok` or tokens (nothing was ever served to bill against).
--
-- WITHOUT ROWID: the natural key (module, day, served_by) is the only access
-- path this table needs (see the two read queries in module_model_usage.rs);
-- no secondary index, no need for a separate rowid.

CREATE TABLE module_model_usage (
    module            TEXT    NOT NULL,
    day               TEXT    NOT NULL CHECK (length(day) = 10),   -- UTC 'YYYY-MM-DD', record time
    served_by         TEXT    NOT NULL CHECK (served_by IN ('local', 'remote', 'none')),
    calls_ok          INTEGER NOT NULL DEFAULT 0 CHECK (calls_ok >= 0),
    calls_failed      INTEGER NOT NULL DEFAULT 0 CHECK (calls_failed >= 0),
    calls_cancelled   INTEGER NOT NULL DEFAULT 0 CHECK (calls_cancelled >= 0),
    prompt_tokens     INTEGER NOT NULL DEFAULT 0 CHECK (prompt_tokens >= 0),
    completion_tokens INTEGER NOT NULL DEFAULT 0 CHECK (completion_tokens >= 0),
    CHECK (served_by <> 'none' OR (calls_ok = 0 AND prompt_tokens = 0 AND completion_tokens = 0)),
    PRIMARY KEY (module, day, served_by)
) WITHOUT ROWID;
