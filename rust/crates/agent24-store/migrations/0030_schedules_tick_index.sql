-- P-13: the scheduler tick (every 10 s) used to `SELECT * FROM schedules` and
-- filter in Rust. `Store::list_schedules_for_tick(now)` now selects only the
-- rows that can fire: enabled rows whose `next_run_at` is due.
--
-- `next_run_at` is free-form RFC-3339 TEXT (legacy rows may carry
-- milliseconds or a non-UTC offset), so plain string comparison is NOT a safe
-- due test. `julianday()` normalises all of those spellings to an instant and
-- yields NULL for an unparsable value; the index is on that expression so the
-- query can range-scan it. Partial: disabled rows and rows with no next slot
-- never fire and stay out of the index entirely.
--
-- Migration number note: main is at 0029; ab/kernel's pending
-- 0027_session_tool_result_origins must be renumbered past this one (skip 0030).

CREATE INDEX idx_schedules_tick_due
    ON schedules (julianday(next_run_at))
    WHERE enabled = 1 AND next_run_at IS NOT NULL;
