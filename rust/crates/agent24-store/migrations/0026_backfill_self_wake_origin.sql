-- K1-6b.4d (ADR-K1-02 §2.2), release review: a self-wake created before
-- 0024 has no row in `schedule_source_origin`, so it — and every run it
-- started — would read as created by the user. Its prompt was written by
-- the model from a context nobody recorded, so it carries `unknown`. The
-- runs count too: deleting a schedule leaves its runs (and their
-- `schedule_id`) in place. Only the kernel creates `wake_` schedules
-- (agent24-agent self_wake.rs); a user's schedule is `sch_`.
INSERT INTO schedule_source_origin (schedule_id, origin_run_id, classification, created_at)
SELECT id, '', 'unknown', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM (
    SELECT id FROM schedules
    UNION
    SELECT schedule_id FROM runs WHERE schedule_id IS NOT NULL
)
WHERE substr(id, 1, 5) = 'wake_'
  AND id NOT IN (SELECT schedule_id FROM schedule_source_origin);
