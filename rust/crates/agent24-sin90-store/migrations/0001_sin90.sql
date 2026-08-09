-- Sin90 domain schema v1 (SIN90-domain.md §3.1), its OWN database (sin90.db),
-- physically isolated from the kernel's agent24.db. Statuses are TEXT matching
-- the snake_case wire enums in agent24-sin90.

CREATE TABLE sin90_directions (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    status        TEXT NOT NULL,
    target_window TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX idx_sin90_dir_status ON sin90_directions(status);

CREATE TABLE sin90_weeks (
    id         TEXT PRIMARY KEY,
    status     TEXT NOT NULL,
    iso_week   TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE sin90_rhythms (
    id          TEXT PRIMARY KEY,
    status      TEXT NOT NULL,
    allocations TEXT NOT NULL,          -- JSON: Vec<Alloc>
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE sin90_tasks (
    id           TEXT PRIMARY KEY,
    direction_id TEXT REFERENCES sin90_directions(id),
    week_id      TEXT REFERENCES sin90_weeks(id),
    title        TEXT NOT NULL,
    status       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    energy       TEXT NOT NULL,
    est_minutes  INTEGER,
    sort_key     INTEGER NOT NULL DEFAULT 0,
    carried_from TEXT REFERENCES sin90_tasks(id),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_sin90_task_status ON sin90_tasks(status);
CREATE INDEX idx_sin90_task_week   ON sin90_tasks(week_id);
-- One task is carried over at most once (SIN90-domain.md §3.1, Codex #High).
CREATE UNIQUE INDEX idx_sin90_task_carried
    ON sin90_tasks(carried_from) WHERE carried_from IS NOT NULL;

CREATE TABLE sin90_schedule_blocks (
    id              TEXT PRIMARY KEY,
    direction_id    TEXT REFERENCES sin90_directions(id),
    task_id         TEXT REFERENCES sin90_tasks(id),
    status          TEXT NOT NULL,
    planned_minutes INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX idx_sin90_block_status ON sin90_schedule_blocks(status);

CREATE TABLE sin90_reviews (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    status     TEXT NOT NULL,
    week_id    TEXT REFERENCES sin90_weeks(id),
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Source of truth: append-only event log. seq is a monotonic total order (the
-- anchor for replay + watermark); payload is SELF-CONTAINED so replay never
-- joins mutable tables (SIN90-domain.md §3.1, Codex #High).
CREATE TABLE sin90_events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    entity     TEXT NOT NULL,          -- direction|task|week|block|rhythm|review
    entity_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- created|transitioned|reordered|adjusted
    from_state TEXT,
    to_state   TEXT,
    payload    TEXT NOT NULL,          -- self-contained JSON snapshot
    schema_ver INTEGER NOT NULL DEFAULT 1,
    at         TEXT NOT NULL           -- occurred_at
);
CREATE INDEX idx_sin90_events_entity ON sin90_events(entity, entity_id);

-- Proposal gate (persistent): pending -> applying -> applied | rejected.
-- applying is the CAS-claimed state that makes a re-tried accept idempotent.
CREATE TABLE sin90_proposals (
    id         TEXT PRIMARY KEY,
    status     TEXT NOT NULL,
    source     TEXT NOT NULL,
    ops        TEXT NOT NULL,          -- JSON: Vec<Sin90Op>
    rationale  TEXT,
    result     TEXT,                   -- AppliedProposal JSON (idempotent replay)
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX idx_sin90_proposals_status ON sin90_proposals(status);

-- Kernel side-effect outbox (SIN90-domain.md §0.2): apply writes here only;
-- a reconciler idempotently lands each onto the kernel (register cron, etc.).
CREATE TABLE sin90_outbox (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    dedup_key  TEXT NOT NULL,
    desired    TEXT NOT NULL,          -- JSON: desired kernel state
    status     TEXT NOT NULL,          -- pending|done
    created_at TEXT NOT NULL,
    done_at    TEXT
);
CREATE INDEX idx_sin90_outbox_status ON sin90_outbox(status);

-- Three-brain routing audit: one row per AI decision.
CREATE TABLE sin90_ai_calls (
    id            TEXT PRIMARY KEY,
    task_kind     TEXT NOT NULL,
    engine        TEXT NOT NULL,        -- reflex|local|executive
    fallback_from TEXT,
    latency_ms    INTEGER,
    ok            INTEGER NOT NULL,
    at            TEXT NOT NULL
);

-- Materialized attention view: purely derived from sin90_events, guarded by a
-- watermark so incremental == full rebuild (SIN90-domain.md §3.2, Codex #High).
CREATE TABLE sin90_attention_daily (
    day          TEXT NOT NULL,
    direction_id TEXT NOT NULL,
    actual_min   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, direction_id)
);
-- Single-row watermark: highest event seq already folded into the view.
CREATE TABLE sin90_attention_watermark (
    only_row          INTEGER PRIMARY KEY CHECK (only_row = 1),
    applied_event_seq INTEGER NOT NULL
);
INSERT INTO sin90_attention_watermark (only_row, applied_event_seq) VALUES (1, 0);
