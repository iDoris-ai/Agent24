-- K1-6b.4d (ADR-K1-02 §2.2): the source classification a kernel-created
-- schedule carries from the run that created it.
--
-- A self-wake's prompt is written by the model from its run's context, so
-- the run it later starts is classified as that context was, not as fresh
-- user input. Only the kernel writes this table — it is not part of
-- `ScheduleAction` or any REST/wire shape — so a client can neither forge
-- nor drop a row. A schedule with no row here was created by the user.
CREATE TABLE schedule_source_origin (
    schedule_id    TEXT NOT NULL PRIMARY KEY,
    origin_run_id  TEXT NOT NULL,
    classification TEXT NOT NULL CHECK (classification IN ('ordinary', 'unknown', 'restricted')),
    created_at     TEXT NOT NULL
);
