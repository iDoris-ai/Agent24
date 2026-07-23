-- Agent24 store schema v1 (C1). JSON-valued columns are TEXT holding the
-- protocol wire shapes; statuses are TEXT matching the snake_case wire enums.

CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    channel    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE runs (
    id          TEXT PRIMARY KEY,
    session_id  TEXT REFERENCES sessions(id),
    status      TEXT NOT NULL,
    input       TEXT NOT NULL,   -- RunInput JSON
    output      TEXT,            -- RunOutput JSON, null until completed
    error       TEXT,            -- ErrorBody JSON, null unless failed
    usage       TEXT NOT NULL,   -- Usage JSON
    schedule_id TEXT,
    created_at  TEXT NOT NULL,
    started_at  TEXT,
    ended_at    TEXT
);
CREATE INDEX idx_runs_status ON runs(status);
CREATE INDEX idx_runs_session ON runs(session_id);

CREATE TABLE tool_calls (
    id             TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL REFERENCES runs(id),
    tool           TEXT NOT NULL,
    input          TEXT NOT NULL,  -- full JSON, audit detail
    status         TEXT NOT NULL,
    output_summary TEXT,
    started_at     TEXT NOT NULL,
    ended_at       TEXT
);
CREATE INDEX idx_tool_calls_run ON tool_calls(run_id);

CREATE TABLE approvals (
    id                  TEXT PRIMARY KEY,
    run_id              TEXT NOT NULL REFERENCES runs(id),
    tool_call_id        TEXT NOT NULL,  -- no FK by design: the approval row may be
                                        -- written before its tool_call is persisted (C4)
    kind                TEXT NOT NULL,
    summary             TEXT NOT NULL,
    payload             TEXT NOT NULL,  -- JSON
    available_decisions TEXT NOT NULL,  -- JSON array of strings
    status              TEXT NOT NULL,
    decision            TEXT,           -- Decision JSON once resolved
    expires_at          TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    decided_at          TEXT
);
CREATE INDEX idx_approvals_status ON approvals(status);

CREATE TABLE schedules (
    id                   TEXT PRIMARY KEY,
    name                 TEXT NOT NULL,
    enabled              INTEGER NOT NULL,
    spec                 TEXT NOT NULL,  -- ScheduleSpec JSON
    action               TEXT NOT NULL,  -- ScheduleAction JSON
    delivery             TEXT NOT NULL,  -- JSON array
    last_run_at          TEXT,
    next_run_at          TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);

-- Hash-chained audit log (openfang-inspired): hash = sha256(prev_hash || ts ||
-- actor || action || detail). Chain verification detects tampering.
CREATE TABLE audit_log (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        TEXT NOT NULL,
    actor     TEXT NOT NULL,
    action    TEXT NOT NULL,
    detail    TEXT NOT NULL,   -- JSON, full fidelity (logs stay redacted; this table is local-only)
    prev_hash TEXT NOT NULL,
    hash      TEXT NOT NULL
);
