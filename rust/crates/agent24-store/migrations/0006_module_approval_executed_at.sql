-- T7c/ME-3e: `executed_at` — the "reached its point" flag for the first
-- non-empty gate closed-set entry, `schedule_callback`
-- (docs/design/T7c-ME3e-gate-execution.md). A NEW migration rather than an
-- edit to the already-shipped 0005_module_approvals.sql (Codex round 2
-- Medium 1) — 0005 may already have run against a real database.
--
-- NULL means "not executed" — for every `advise` row, every `gate` row that
-- is not `approved`, and every `approved` `gate` row whose target has not
-- been reached yet. `ALTER TABLE ... ADD COLUMN` with no DEFAULT backfills
-- existing rows with NULL, so every pre-T7c row (T7b's `advise`-only build)
-- comes through unaffected.
ALTER TABLE module_approvals ADD COLUMN executed_at TEXT;

-- The periodic scan's new query (module_approval_broker.rs::scan_once) filters
-- exactly this predicate; without an index it degrades to a full table scan
-- as approval history grows (Codex round 2 Medium 3). Mirrors
-- idx_module_approvals_pending_expiry's shape: a partial index over only the
-- rows that can still change.
CREATE INDEX idx_module_approvals_pending_schedule
    ON module_approvals (target)
    WHERE kind = 'gate' AND decision = 'approved' AND action = 'schedule_callback'
      AND executed_at IS NULL;
