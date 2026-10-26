-- P1-02c c1: personal-owner retraction is exempt from the A-class quota gate.
--
-- Design: docs/agent/P1-02C-QUOTA-FAILURE-DESIGN.md D1 (v3). Before this
-- migration `mem_events_bi_quota` rejected EVERY kind alike, so a full personal
-- partition could not even retract an assertion (the retraction is itself one
-- more `mem_events` row). ADR: retraction must always stay available.
--
-- What is exempt, and ONLY this: a row for which ALL of the following hold,
-- evaluated inside the trigger SQL on the INSERT path of `mem_events` (every
-- writer funnels through it, see 0014):
--   1. kind = 'assertion.retracted';
--   2. `scope_owner` is a PERSONAL partition: an exact match on
--      `mem_os_partitions.owner_key` with `space_kind = 'personal'` (0016).
--      Never a string-prefix test — module keys are NUL-separated, versioned
--      strings and a prefix that merely looks personal must not qualify;
--   3. the payload passes four checks: json_valid, a JSON object with exactly
--      one top-level key, that key is `assertion_id` holding a text value, and
--      LENGTH(CAST(payload AS BLOB)) <= 256 (the shape `assertion::forget`
--      writes). The CASE keeps json_each from ever seeing malformed JSON.
-- Anything else is an A-class row and goes through the unchanged row/byte gate.
--
-- THREE-VALUED LOGIC: in SQLite `NOT NULL` is NULL and a trigger whose WHEN is
-- NULL does not fire. `json_type(payload, '$.assertion_id')` is NULL when the
-- key is missing, so an unguarded exemption expression would be NULL for
-- `{"x":"a"}`, the BEFORE quota trigger would be skipped, and the AFTER usage
-- trigger would still count the row: an unbounded over-quota write. The
-- exemption expression is therefore wrapped in COALESCE(.., 0) so it can only be
-- 0 or 1, and the gate's WHEN uses COALESCE(NOT ..., 1): when in doubt, A gate.
--
-- Counting: `mem_owner_usage` gets `exempt_row_count` / `exempt_byte_count`.
-- An exempt row is added to those only; `row_count` / `byte_count` stay the
-- A-class counters and are all the gate compares, so exempt rows never crowd out
-- content. Bound (design D1.3): `forget` writes a retraction only for a
-- currently-live assertion, and the only way back to live is a re-assert that
-- itself appended an A-class row, so exempt rows <= A-class activation rows,
-- independent of the CURRENT max_rows (lowering the limit later is legal).
--
-- PERSISTED CLASS: the class a row was billed in at INSERT is recorded in
-- `mem_event_quota_exempt` (one row per exempt event; absent = A-class). UPDATE
-- (owner move) and DELETE read ONLY that table, never the current catalog or
-- payload, so a later catalog change cannot move a row between classes and a
-- row's decrement always hits the counter its increment hit. Counters are
-- therefore exact and no clamp is needed: a decrement that would go negative
-- means the bookkeeping is broken and aborts with 'mem_quota:underflow'.
-- A SQLite trigger cannot modify NEW, hence a side table rather than a column.
--
-- NO BACKFILL, deliberately: pre-existing `assertion.retracted` rows are not in
-- `mem_event_quota_exempt`, so they stay A-class forever (counted in
-- `row_count` / `byte_count` by 0014, and decremented from there). Recomputing
-- history would rewrite usage for every owner in one migration and gain nothing:
-- those rows were legitimately admitted under the old rule. 0014 and 0016 are
-- left untouched.

ALTER TABLE mem_owner_usage ADD COLUMN exempt_row_count  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mem_owner_usage ADD COLUMN exempt_byte_count INTEGER NOT NULL DEFAULT 0;

CREATE TABLE mem_event_quota_exempt (
    event_id TEXT PRIMARY KEY
);

DROP TRIGGER mem_events_ai_usage;
DROP TRIGGER mem_events_au_owner_usage;
DROP TRIGGER mem_events_ad_usage;
DROP TRIGGER mem_events_bi_quota;

CREATE TRIGGER mem_events_ai_usage AFTER INSERT ON mem_events BEGIN
    DELETE FROM mem_event_quota_exempt WHERE event_id = new.id;
    INSERT INTO mem_event_quota_exempt (event_id)
    SELECT new.id WHERE COALESCE((
        new.kind = 'assertion.retracted'
        AND EXISTS (SELECT 1 FROM mem_os_partitions
                    WHERE owner_key = new.scope_owner AND space_kind = 'personal')
        AND CASE WHEN json_valid(new.payload)
             THEN json_type(new.payload) = 'object'
                  AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                  AND json_type(new.payload, '$.assertion_id') = 'text'
                  AND LENGTH(CAST(new.payload AS BLOB)) <= 256
             ELSE 0 END
    ), 0) = 1;
    INSERT INTO mem_owner_usage
        (owner, row_count, byte_count, exempt_row_count, exempt_byte_count, updated_at)
    VALUES (new.scope_owner,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = new.id) THEN 0 ELSE 1 END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = new.id) THEN 0 ELSE LENGTH(CAST(new.payload AS BLOB)) END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = new.id) THEN 1 ELSE 0 END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = new.id) THEN LENGTH(CAST(new.payload AS BLOB)) ELSE 0 END,
        datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + excluded.row_count,
        byte_count = byte_count + excluded.byte_count,
        exempt_row_count = exempt_row_count + excluded.exempt_row_count,
        exempt_byte_count = exempt_byte_count + excluded.exempt_byte_count,
        updated_at = excluded.updated_at;
END;

-- Owner move (`KvStore::rekey_os_partition`): the row keeps the class it was
-- billed in; the counters move between the old and the new owner accordingly.
CREATE TRIGGER mem_events_au_owner_usage AFTER UPDATE OF scope_owner ON mem_events
WHEN old.scope_owner IS NOT new.scope_owner
BEGIN
    UPDATE mem_owner_usage SET
        row_count = row_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE 1 END,
        byte_count = byte_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE LENGTH(CAST(old.payload AS BLOB)) END,
        exempt_row_count = exempt_row_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 1 ELSE 0 END,
        exempt_byte_count = exempt_byte_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN LENGTH(CAST(old.payload AS BLOB)) ELSE 0 END,
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
    SELECT RAISE(ABORT, 'mem_quota:underflow')
    WHERE NOT EXISTS (
        SELECT 1 FROM mem_owner_usage
        WHERE owner = old.scope_owner
          AND row_count >= 0 AND byte_count >= 0
          AND exempt_row_count >= 0 AND exempt_byte_count >= 0);
    INSERT INTO mem_owner_usage
        (owner, row_count, byte_count, exempt_row_count, exempt_byte_count, updated_at)
    VALUES (new.scope_owner,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE 1 END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE LENGTH(CAST(old.payload AS BLOB)) END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 1 ELSE 0 END,
        CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN LENGTH(CAST(old.payload AS BLOB)) ELSE 0 END,
        datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + excluded.row_count,
        byte_count = byte_count + excluded.byte_count,
        exempt_row_count = exempt_row_count + excluded.exempt_row_count,
        exempt_byte_count = exempt_byte_count + excluded.exempt_byte_count,
        updated_at = excluded.updated_at;
END;

-- `mem_events_bu_id_immutable` below keeps `event_id` stable; payload is already
-- immutable (0014), so the persisted class cannot drift from the row.
CREATE TRIGGER mem_events_ad_usage AFTER DELETE ON mem_events BEGIN
    UPDATE mem_owner_usage SET
        row_count = row_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE 1 END,
        byte_count = byte_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 0 ELSE LENGTH(CAST(old.payload AS BLOB)) END,
        exempt_row_count = exempt_row_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN 1 ELSE 0 END,
        exempt_byte_count = exempt_byte_count - CASE WHEN EXISTS (SELECT 1 FROM mem_event_quota_exempt WHERE event_id = old.id) THEN LENGTH(CAST(old.payload AS BLOB)) ELSE 0 END,
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
    SELECT RAISE(ABORT, 'mem_quota:underflow')
    WHERE NOT EXISTS (
        SELECT 1 FROM mem_owner_usage
        WHERE owner = old.scope_owner
          AND row_count >= 0 AND byte_count >= 0
          AND exempt_row_count >= 0 AND exempt_byte_count >= 0);
    DELETE FROM mem_event_quota_exempt WHERE event_id = old.id;
END;

CREATE TRIGGER mem_events_bu_id_immutable
BEFORE UPDATE OF id ON mem_events
WHEN old.id IS NOT new.id
BEGIN
    SELECT RAISE(ABORT, 'mem_events.id is immutable');
END;

-- REPLACE GUARD. The quota gate below deliberately skips an already-existing id
-- (a same-id replay must not turn into a spurious QuotaExceeded). That skip would
-- also let `INSERT OR REPLACE` / `REPLACE INTO` swap an exempt row for an
-- over-quota ordinary one, and with recursive_triggers=OFF SQLite does NOT fire
-- AFTER DELETE for the row it replaces, so the usage counters and the
-- `mem_event_quota_exempt` record would be left describing a row that is gone.
-- Fix: a row whose `id` (or an explicitly supplied `seq`) already exists is
-- dropped with RAISE(IGNORE) before SQLite can resolve the conflict, so no
-- conflict resolution (REPLACE included) ever deletes a stored event.
--   * `INSERT .. ON CONFLICT(id) DO NOTHING` (EventLog::append) keeps its
--     semantics: no row, no RETURNING row, the Rust replay check runs.
--   * a PLAIN `INSERT` of an existing id used to fail with a UNIQUE error; it is
--     now ignored silently, so `EventLog::append_tx` checks rows_affected()
--     and reports the collision itself.
-- Unique columns of mem_events: `id` and `seq` (INTEGER PRIMARY KEY) only.
-- NEW.seq is not the supplied value when none is given (an auto rowid placeholder
-- that never matches a stored row), so the seq arm only fires for an explicit seq.
CREATE TRIGGER mem_events_bi_no_replace BEFORE INSERT ON mem_events
WHEN EXISTS (SELECT 1 FROM mem_events WHERE id = new.id)
     OR EXISTS (SELECT 1 FROM mem_events WHERE seq = new.seq)
BEGIN
    SELECT RAISE(IGNORE);
END;

-- The gate: identical to 0014 (including the `id` replay guard and the
-- COALESCE-outside-the-subquery fix) except that an exempt row is skipped.
CREATE TRIGGER mem_events_bi_quota BEFORE INSERT ON mem_events
WHEN NOT EXISTS (SELECT 1 FROM mem_events WHERE id = new.id)
     AND COALESCE(NOT COALESCE((
         new.kind = 'assertion.retracted'
         AND EXISTS (SELECT 1 FROM mem_os_partitions
                     WHERE owner_key = new.scope_owner AND space_kind = 'personal')
         AND CASE WHEN json_valid(new.payload)
              THEN json_type(new.payload) = 'object'
                   AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                   AND json_type(new.payload, '$.assertion_id') = 'text'
                   AND LENGTH(CAST(new.payload AS BLOB)) <= 256
              ELSE 0 END
     ), 0), 1)
BEGIN
    SELECT RAISE(ABORT, 'mem_quota:rows')
    WHERE COALESCE((SELECT row_count FROM mem_owner_usage WHERE owner = new.scope_owner), 0) + 1
          > COALESCE(
              (SELECT max_rows FROM mem_owner_quota WHERE owner = new.scope_owner),
              (SELECT max_rows FROM mem_owner_quota WHERE owner = '*'));
    SELECT RAISE(ABORT, 'mem_quota:bytes')
    WHERE COALESCE((SELECT byte_count FROM mem_owner_usage WHERE owner = new.scope_owner), 0)
          + LENGTH(CAST(new.payload AS BLOB))
          > COALESCE(
              (SELECT max_bytes FROM mem_owner_quota WHERE owner = new.scope_owner),
              (SELECT max_bytes FROM mem_owner_quota WHERE owner = '*'));
END;
