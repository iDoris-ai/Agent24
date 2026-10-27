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

-- Shape guard: `julianday()` on a non-deterministic input (e.g. the literal
-- 'now') is a hard error inside an index (`non-deterministic use of
-- julianday() in an index`), which would abort this migration and every later
-- write of such a value. Rows whose `next_run_at` does not start with a
-- 4-digit year are therefore kept OUT of the expression index by a
-- deterministic GLOB, and served by a second, tiny partial index instead; the
-- query reads them through a third arm so the tick still logs them.
-- The WHERE terms must match the query's, verbatim.
CREATE INDEX idx_schedules_tick_due
    ON schedules (julianday(next_run_at))
    WHERE enabled = 1 AND next_run_at IS NOT NULL
      AND next_run_at GLOB '[0-9][0-9][0-9][0-9]-*';

CREATE INDEX idx_schedules_tick_odd_shape
    ON schedules (id)
    WHERE enabled = 1 AND next_run_at IS NOT NULL
      AND next_run_at NOT GLOB '[0-9][0-9][0-9][0-9]-*';
