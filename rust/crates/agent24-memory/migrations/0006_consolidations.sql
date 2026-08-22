-- MD-5: the consolidation projection — background "sleep synthesis" that folds
-- episodic events into importance-ranked insights (SPEC-MD-ME §3 MD-5;
-- memobase/MemoryScope observation→insight + importance).
--
-- A PROJECTION over the event log, not an authority: each consolidation is a pure
-- function of all events sharing its key, so re-running the consolidator
-- reproduces it (idempotent) and an incremental run equals a full rebuild. The id
-- is a stable `consol-{owner}-{key}`, so re-consolidation UPSERTs the same row.

-- The relational identity is the PAIR (scope_owner, consol_key), NOT a
-- concatenated string: `consol-{owner}-{key}` would alias `alice`+`x-y` with
-- `alice-x`+`y`, letting one owner overwrite another's row (review #122 B1).
-- `id` is a collision-free DERIVED display handle (hash of owner+key), never the
-- key that isolation depends on.
CREATE TABLE mem_consolidations (
    scope_owner   TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    consol_key    TEXT    NOT NULL,                      -- the grouping key (event kind)
    id            TEXT    NOT NULL,                      -- derived display handle
    insight       TEXT    NOT NULL,
    importance    REAL    NOT NULL,
    source_events TEXT    NOT NULL,                      -- JSON array of event ids
    at            TEXT    NOT NULL,                      -- latest source event's time (deterministic)
    PRIMARY KEY (scope_owner, consol_key)
);

CREATE INDEX idx_consol_owner_importance ON mem_consolidations (scope_owner, importance DESC);
