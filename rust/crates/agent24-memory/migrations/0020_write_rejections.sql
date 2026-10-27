-- P1-02c c3: failed-write audit trail (design D3) and session gap ranges (D4).
--
-- Both tables are deliberately OUTSIDE the quota: they are not `mem_events`
-- rows, so `mem_events_bi_quota` / `mem_owner_usage` never see them, and a full
-- partition can still record why it is full. Neither holds user content: only
-- an opaque owner id (a digest, never the partition key), a closed error code,
-- the rejected KIND and counters.
--
-- NOTE on numbering: 0019 is intentionally absent on this branch (another branch
-- renumbers memory-p1's 0018 to 0019; kernel owns 0018). Gaps in the sequence are
-- legal for sqlx; versions only need to be unique and increasing.

CREATE TABLE mem_write_rejections (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_kind       TEXT    NOT NULL CHECK (owner_kind IN ('personal', 'module')),
    owner_id         TEXT    NOT NULL,
    code             TEXT    NOT NULL,
    dimension        TEXT,
    rejected_kind    TEXT,
    -- Usage of the rejected dimension at the moment of rejection, when known.
    count_at         INTEGER,
    at_epoch         INTEGER NOT NULL,
    -- Rejections folded into this row by the 60 s rate limit.
    suppressed_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX mem_write_rejections_owner ON mem_write_rejections(owner_id, id);

-- One row per CONTIGUOUS run of unsaved turns in a session. No ring: the row
-- count is bounded by the session's own turn count.
CREATE TABLE mem_session_gaps (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT    NOT NULL,
    turn_from  INTEGER NOT NULL,
    turn_to    INTEGER NOT NULL,
    code       TEXT    NOT NULL,
    first_at   INTEGER NOT NULL,
    last_at    INTEGER NOT NULL,
    CHECK (turn_from <= turn_to)
);
CREATE INDEX mem_session_gaps_session ON mem_session_gaps(session_id, turn_to);
