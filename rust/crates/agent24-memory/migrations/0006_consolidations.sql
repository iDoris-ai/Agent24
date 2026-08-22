-- MD-5: the consolidation projection — background "sleep synthesis" that folds
-- episodic events into importance-ranked insights (SPEC-MD-ME §3 MD-5;
-- memobase/MemoryScope observation→insight + importance).
--
-- A PROJECTION over the event log, not an authority: each consolidation is a pure
-- function of all events sharing its key, so re-running the consolidator
-- reproduces it (idempotent) and an incremental run equals a full rebuild. The id
-- is a stable `consol-{owner}-{key}`, so re-consolidation UPSERTs the same row.

CREATE TABLE mem_consolidations (
    id            TEXT    NOT NULL PRIMARY KEY,          -- consol-{owner}-{key}
    scope_owner   TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    consol_key    TEXT    NOT NULL,                      -- the grouping key (event kind)
    insight       TEXT    NOT NULL,
    importance    REAL    NOT NULL,
    source_events TEXT    NOT NULL,                      -- JSON array of event ids
    at            TEXT    NOT NULL                       -- latest source event's time (deterministic)
);

CREATE INDEX idx_consol_owner_importance ON mem_consolidations (scope_owner, importance DESC);
