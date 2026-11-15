-- X11-2a (INBOUND-ADMISSION-PLAN §3.1): inbound admission tables.
-- New tables only. This migration does not alter any existing table.
--
-- `id` is `<source>:<native_id>` with no edit version (§3.4). The partial
-- unique index is the in-flight cap: at most one explicit row per
-- (source, actor) while run_state is starting or started. SQLite treats
-- NULL as distinct in a unique index, so an explicit row with no actor
-- would skip that cap; the table CHECK rejects it. Callers insert with
-- ON CONFLICT(id) DO NOTHING so a replay of the same id does not
-- overwrite; a clash on this index is a different error and is not swallowed.
-- `category` and `disposition_reason` stay open text: callers own those words.

CREATE TABLE inbound_events (
    id                 TEXT PRIMARY KEY NOT NULL,
    source             TEXT NOT NULL,
    category           TEXT NOT NULL,
    recipient_identity TEXT,
    thread_key         TEXT,
    actor              TEXT,
    human              INTEGER NOT NULL CHECK (human IN (0, 1)),
    addressed          TEXT NOT NULL CHECK (addressed IN ('none', 'mention', 'direct')),
    at                 TEXT NOT NULL,
    received_at        TEXT NOT NULL,
    text               TEXT,
    meta               TEXT,
    text_scrubbed_at   TEXT,
    disposition        TEXT NOT NULL CHECK (
        disposition IN ('explicit', 'ignore', 'filtered', 'internal')
    ),
    disposition_reason TEXT,
    policy_version     TEXT,
    run_state          TEXT CHECK (
        run_state IS NULL OR run_state IN (
            'starting', 'started', 'done', 'interrupted', 'start_failed'
        )
    ),
    CHECK (disposition <> 'explicit' OR (actor IS NOT NULL AND actor <> ''))
);

CREATE TABLE inbound_runs (
    run_id   TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL UNIQUE REFERENCES inbound_events(id)
);

CREATE INDEX idx_inbound_events_at ON inbound_events(at);
CREATE INDEX idx_inbound_events_actor ON inbound_events(source, actor, at);
CREATE INDEX idx_inbound_events_thread ON inbound_events(source, thread_key, at);
CREATE INDEX idx_inbound_events_scrub ON inbound_events(received_at) WHERE text_scrubbed_at IS NULL;
CREATE INDEX idx_inbound_events_recover ON inbound_events(run_state) WHERE run_state = 'starting';
CREATE UNIQUE INDEX idx_inbound_events_inflight ON inbound_events(source, actor)
    WHERE disposition = 'explicit' AND run_state IN ('starting', 'started');

CREATE TABLE inbound_allowlist (
    source   TEXT NOT NULL,
    actor    TEXT NOT NULL,
    added_at TEXT NOT NULL,
    note     TEXT,
    PRIMARY KEY (source, actor)
);

CREATE TABLE inbound_policy (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    accept_mode TEXT NOT NULL DEFAULT 'observe' CHECK (accept_mode IN ('off', 'observe', 'auto')),
    updated_at  TEXT NOT NULL
);

CREATE TABLE inbound_source_watermark (
    source       TEXT PRIMARY KEY NOT NULL,
    activated_at TEXT NOT NULL,
    max_age_secs INTEGER NOT NULL
);
