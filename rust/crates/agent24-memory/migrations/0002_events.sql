-- MD-2: the episodic authority — an append-only, immutable event log
-- (SPEC-MD-ME §1/§2, ADR-028). Events are never rewritten. `id` is the
-- client-stable idempotency key (a re-append with a seen id is a no-op); `seq`
-- is the monotonic total order used for scans and projection checkpoints.
-- `scope_owner` is mandatory (governance: no unowned memory); `scope_session`
-- is denormalized from the full `scope` JSON for efficient per-session scans.
CREATE TABLE mem_events (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    id            TEXT NOT NULL UNIQUE,
    scope_owner   TEXT NOT NULL,
    scope_session TEXT,
    scope         TEXT NOT NULL,            -- JSON: full Scope
    kind          TEXT NOT NULL,
    payload       TEXT NOT NULL,            -- JSON body
    origin_source TEXT NOT NULL,
    origin_trust  TEXT NOT NULL,            -- user_said|tool_output|web_fetch|model|system
    causal        TEXT NOT NULL DEFAULT '[]', -- JSON: Vec<EventId>
    at            TEXT NOT NULL
);
CREATE INDEX mem_events_owner_seq   ON mem_events(scope_owner, seq);
CREATE INDEX mem_events_session_seq ON mem_events(scope_owner, scope_session, seq);

-- Projection checkpoints: a named consumer (a Condenser view, an FTS/vector
-- index) records the max event seq it has folded in, so rebuilds are
-- incremental and deterministic.
CREATE TABLE mem_checkpoints (
    name      TEXT PRIMARY KEY,
    up_to_seq INTEGER NOT NULL,
    at        TEXT NOT NULL
);
