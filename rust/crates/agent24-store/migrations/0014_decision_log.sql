-- D0-2 决策日志（PLAN-DECIDE.md §2.1-§2.3；docs/decision.md ADR-034）.
--
-- Local-only, no telemetry: nothing in this file is ever sent off-device.
-- `decision_log` is one row per `DecisionRequest` the (future, D1+) decision
-- service ran; `decision_outcome` is zero or more event-sourced labels that
-- arrive later for the SAME decision (a user answered a clarifying
-- question, retracted a write, said "that's wrong", an approval was
-- granted/denied, or a write was recalled and never corrected — §2.2). One
-- decision can accumulate several outcomes over time, hence the separate
-- table rather than a single mutable column.
--
-- Retention (§2.3, "保留期默认有界…超期只保留去掉原文的统计"): the ONLY
-- columns an expiry sweep may null out are `input` (the user's raw text) and
-- `context` (structured-but-still-content features) — `point`,
-- `schema_version`, `question`, `layers`, `final_action`, and `hw_tier` are
-- "统计字段" (no raw text) and are kept forever so the row still counts
-- toward D1's accumulation targets (§2.4) after scrubbing. `scrubbed_at`
-- records when that happened (NULL = never scrubbed) so a sweep is
-- idempotent and callers can tell a genuinely-empty `input` from a scrubbed
-- one. Nothing in THIS migration runs the sweep — see
-- `agent24-store/src/decision_log.rs`'s `scrub_expired_decision_log`; no
-- caller invokes it yet (D0: no call sites touched).
--
-- `decision_id` is an opaque caller-supplied string (future D1 caller's own
-- id generator), not an autoincrement — the row must be addressable by the
-- SAME id the decision service already handed back to its caller, not by a
-- storage-internal number.
CREATE TABLE decision_log (
    decision_id     TEXT    NOT NULL PRIMARY KEY,
    ts              TEXT    NOT NULL, -- ISO 8601 UTC
    schema_version  INTEGER NOT NULL,
    point           TEXT    NOT NULL, -- e.g. 'retain.intent', 'recall.gate'
    -- User's original text. Local-only, never leaves the device, never in
    -- telemetry (PLAN-DECIDE.md §2.3). Nulled by retention scrubbing.
    input           TEXT,
    -- Structured context features (JSON object) — NOT the whole
    -- conversation (§2.1). Nulled by retention scrubbing, same as `input`.
    context         TEXT,
    -- The question(s) put to the backend (JSON; choice/noul/score + candidate
    -- labels) — a prompt template/shape, not the user's raw text, so it
    -- survives retention scrubbing.
    question        TEXT    NOT NULL,
    -- layers[]: each backend layer's backend kind, model id+revision (if
    -- any), label, probability, and latency (JSON array, §2.1).
    layers          TEXT    NOT NULL,
    final_action    TEXT    NOT NULL CHECK (final_action IN ('execute', 'abstain', 'ask', 'escalate')),
    -- Hardware tier (D0-3) at decision time. Nullable: D0-2 ships before
    -- D0-3's HardwareProbe/TierPolicy lands, and a caller that never
    -- determined a tier (e.g. a test) has nothing to put here.
    hw_tier         TEXT,
    -- NULL = never scrubbed. Set together with nulling input/context.
    scrubbed_at     TEXT
);

-- `decide export --jsonl [--point X] [--since T]` filters/orders by these.
CREATE INDEX idx_decision_log_point ON decision_log(point);
CREATE INDEX idx_decision_log_ts ON decision_log(ts);
-- The retention sweep's own WHERE (ts < cutoff AND scrubbed_at IS NULL).
CREATE INDEX idx_decision_log_scrub ON decision_log(scrubbed_at, ts);

CREATE TABLE decision_outcome (
    id           INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    decision_id  TEXT    NOT NULL REFERENCES decision_log(decision_id) ON DELETE CASCADE,
    ts           TEXT    NOT NULL,
    signal       TEXT    NOT NULL CHECK (signal IN (
                     'clarify_answer', 'user_retract', 'user_says_wrong',
                     'approval_denied', 'approval_granted', 'recalled_uncorrected'
                 )),
    label        TEXT    NOT NULL, -- JSON
    quality      TEXT    NOT NULL CHECK (quality IN ('high', 'medium', 'low'))
);

-- Export's per-decision outcome fetch; `ON DELETE CASCADE` above (combined
-- with `Store::open`'s `foreign_keys(true)`) is what makes all three delete
-- paths (by id / by point / all) remove a decision's outcomes for free —
-- deleting `decision_log` rows is always sufficient, this table is never
-- deleted from directly.
CREATE INDEX idx_decision_outcome_decision_id ON decision_outcome(decision_id);
