-- ME4-1.2.1 / docs/design/ME4-S1-scheduler-callback.md §2.1 (verbatim, frozen
-- v3.1). Gives `schedules` ownership/identity columns for module-owned rows
-- (owner_module/module_key/revision/user_suspended/system_disabled_reason)
-- and adds `schedule_deliveries`, the per-fire delivery state machine table
-- for module targets (§4).
--
-- `user_suspended` / `system_disabled_reason` are CHECKed to module rows
-- only: user rows already have `enabled` as their one switch, and letting a
-- user row also carry `user_suspended` would grow a second "paused" meaning
-- that the REST PATCH `enabled` and suspend/resume would fight over.
--
-- Old rows: every new column defaults to NULL/0, so a pre-0007 AgentRun row
-- is untouched (test: `migration_keeps_old_rows_and_checks_hold`).
--
-- Column name `fire_trigger`, not `trigger`: TRIGGER is a SQLite keyword.
--
-- ME4-1.2.1a note: the full table (schema AND `schedule_deliveries`) lands
-- in one migration here, even though this cut's code does not write to
-- `schedule_deliveries` yet (`MODULE_STATE_SELECT` already reads it for
-- `last_fire`). ME4-1.2.1b (deliveries) is the first to write it.

ALTER TABLE schedules ADD COLUMN owner_module TEXT;
ALTER TABLE schedules ADD COLUMN module_key TEXT
    CHECK ((owner_module IS NULL) = (module_key IS NULL));
ALTER TABLE schedules ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE schedules ADD COLUMN user_suspended INTEGER NOT NULL DEFAULT 0
    CHECK (user_suspended IN (0, 1) AND (user_suspended = 0 OR owner_module IS NOT NULL));
ALTER TABLE schedules ADD COLUMN system_disabled_reason TEXT
    CHECK (system_disabled_reason IS NULL OR owner_module IS NOT NULL);

CREATE UNIQUE INDEX idx_schedules_module_key
    ON schedules (owner_module, module_key)
    WHERE owner_module IS NOT NULL;

CREATE TABLE schedule_deliveries (
    fire_id         TEXT PRIMARY KEY,
    schedule_id     TEXT NOT NULL REFERENCES schedules (id) ON DELETE CASCADE,
    owner_module    TEXT NOT NULL,
    module_key      TEXT NOT NULL,
    scheduled_for   TEXT NOT NULL,
    fired_at        TEXT NOT NULL,
    fire_trigger    TEXT NOT NULL CHECK (fire_trigger IN ('tick', 'run_now')),
    status          TEXT NOT NULL
        CHECK (status IN ('pending', 'deferred', 'delivered', 'failed', 'expired')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    expires_at      TEXT NOT NULL,
    last_error      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    CHECK ((status IN ('pending', 'deferred')) = (next_attempt_at IS NOT NULL))
);
CREATE INDEX idx_schedule_deliveries_due
    ON schedule_deliveries (next_attempt_at)
    WHERE status IN ('pending', 'deferred');
CREATE INDEX idx_schedule_deliveries_schedule
    ON schedule_deliveries (schedule_id);
