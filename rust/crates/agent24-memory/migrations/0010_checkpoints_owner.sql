-- Fix: projection checkpoints were NOT owner-scoped (found by the #126 review).
--
-- 0002 created `mem_checkpoints(name PRIMARY KEY, ...)` — no owner column, and
-- `EventStore::checkpoint*` took no owner argument. Two owners using the same
-- checkpoint name (e.g. "condenser") therefore shared ONE row: whichever side
-- advanced it made the OTHER side's incremental `scan(after_seq)` skip events —
-- silent loss for a consumer that never did anything wrong. Worse,
-- `checkpoint(name)` recorded `MAX(seq)` over the WHOLE table, so one owner's
-- bookmark could be set past another owner's events.
--
-- The identity is (scope_owner, name), so a checkpoint belongs to one owner's
-- projection.
--
-- LEGACY ROWS ARE DROPPED, deliberately. An existing row cannot be attributed to
-- an owner after the fact, and a checkpoint is a rebuildable BOOKMARK, not an
-- authority: losing one makes its consumer re-fold from seq 0 — correct, just
-- slower. Keeping an ambiguous row would instead risk skipping events for
-- whoever it did not belong to. Re-scanning beats silent loss.

CREATE TABLE mem_checkpoints_v2 (
    scope_owner TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    name        TEXT    NOT NULL CHECK (trim(name) <> ''),
    up_to_seq   INTEGER NOT NULL,
    at          TEXT    NOT NULL,
    PRIMARY KEY (scope_owner, name)
);

DROP TABLE mem_checkpoints;
ALTER TABLE mem_checkpoints_v2 RENAME TO mem_checkpoints;
