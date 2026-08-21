-- MD-3a: AssertionLedger — the semantic authority, an immutable BI-TEMPORAL
-- ledger of assertions (SPEC-MD-ME §1/§2, ADR-028; borrowed from graphiti's two
-- intervals + invalidate-in-place-never-delete).
--
-- Two independent time intervals travel with every assertion:
--   valid-time    (valid_from / valid_to)       — WHEN the fact is true in the
--                                                  world (domain time).
--   recorded-time (recorded_from / recorded_to) — WHEN the system believed it
--                                                  (transaction time).
-- A NULL `*_to` means the interval is still open. A contradiction is a NEW row
-- that supersedes the old one and CLOSES the old row's recorded_to — the old row
-- is never deleted, so `beliefs_as_of(valid_at, recorded_at)` can still see what
-- we believed at an earlier recorded time. Evidence (event ids) is likewise never
-- dropped.
--
-- `qualified = 0` is an unconfirmed candidate: it must NOT enter default recall
-- (the write-gate's job in MD-4 keys on this).

CREATE TABLE mem_assertions (
    id             TEXT    NOT NULL PRIMARY KEY,
    scope_owner    TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    scope          TEXT    NOT NULL,          -- full Scope JSON
    subject        TEXT    NOT NULL,
    predicate      TEXT    NOT NULL,
    object         TEXT    NOT NULL,          -- JSON value
    valid_from     TEXT    NOT NULL,
    valid_to       TEXT,                      -- NULL = still valid
    recorded_from  TEXT    NOT NULL,
    recorded_to    TEXT,                      -- NULL = still believed
    evidence       TEXT    NOT NULL,          -- JSON array of event ids
    confidence     REAL    NOT NULL,
    modality       TEXT    NOT NULL,          -- said | observed | derived
    speaker        TEXT,
    writer_version TEXT    NOT NULL,
    supersedes     TEXT,                      -- the assertion id this replaces
    qualified      INTEGER NOT NULL           -- 0 = candidate (not in default recall)
);

-- beliefs_as_of filters by owner + subject then by the two intervals; this index
-- covers the owner/subject prefix.
CREATE INDEX idx_assertions_owner_subject ON mem_assertions (scope_owner, subject);
