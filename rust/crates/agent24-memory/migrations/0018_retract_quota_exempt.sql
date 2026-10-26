-- P1-02c c1: personal-owner retraction is exempt from the A-class quota gate.
--
-- Design: docs/agent/P1-02C-QUOTA-FAILURE-DESIGN.md D1 (v3). Before this
-- migration `mem_events_bi_quota` rejected EVERY kind alike, so a full personal
-- partition could not even retract an assertion (the retraction is itself one
-- more `mem_events` row). ADR: retraction must always stay available.
--
-- What is exempt, and ONLY this: a row for which ALL of the following hold,
-- evaluated inside the trigger SQL on the normal INSERT path of `mem_events`
-- (every writer funnels through it, see 0014):
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
-- Counting: `mem_owner_usage` gets `exempt_row_count` / `exempt_byte_count`.
-- An exempt row is added to those only; `row_count` / `byte_count` stay the
-- A-class counters and are all the gate compares, so exempt rows never crowd out
-- content. Bound (design D1.3): `forget` writes a retraction only for a
-- currently-live assertion, and the only way back to live is a re-assert that
-- itself appended an A-class row, so exempt rows <= A-class activation rows,
-- independent of the CURRENT max_rows (lowering the limit later is legal).
--
-- NO BACKFILL, deliberately: pre-existing `assertion.retracted` rows stay inside
-- `row_count` / `byte_count`. Recomputing history would rewrite usage for every
-- owner in one migration and gain nothing — the old counts were legitimately
-- admitted under the old rule, and the exemption protects only FUTURE
-- retractions. 0014 and 0016 are left untouched.
--
-- Rekey (`UPDATE ... SET scope_owner`) keeps a row in the class it was counted
-- in: the class is decided from the OLD owner's catalog state, because the
-- catalog row is moved after the events and the new key is not yet recorded.
-- A raw DELETE of a pre-0018 retraction row of a personal owner would decrement
-- the exempt counters (clamped at 0) although it was counted as A-class; no
-- production path deletes events (0014), so this is accepted.

ALTER TABLE mem_owner_usage ADD COLUMN exempt_row_count  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mem_owner_usage ADD COLUMN exempt_byte_count INTEGER NOT NULL DEFAULT 0;

DROP TRIGGER mem_events_ai_usage;
DROP TRIGGER mem_events_au_owner_usage;
DROP TRIGGER mem_events_ad_usage;
DROP TRIGGER mem_events_bi_quota;

CREATE TRIGGER mem_events_ai_usage AFTER INSERT ON mem_events BEGIN
    INSERT INTO mem_owner_usage
        (owner, row_count, byte_count, exempt_row_count, exempt_byte_count, updated_at)
    VALUES (
        new.scope_owner,
        CASE WHEN (
            new.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = new.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(new.payload)
                 THEN json_type(new.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                      AND json_type(new.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(new.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 0 ELSE 1 END,
        CASE WHEN (
            new.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = new.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(new.payload)
                 THEN json_type(new.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                      AND json_type(new.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(new.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 0 ELSE LENGTH(CAST(new.payload AS BLOB)) END,
        CASE WHEN (
            new.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = new.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(new.payload)
                 THEN json_type(new.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                      AND json_type(new.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(new.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 1 ELSE 0 END,
        CASE WHEN (
            new.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = new.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(new.payload)
                 THEN json_type(new.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                      AND json_type(new.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(new.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN LENGTH(CAST(new.payload AS BLOB)) ELSE 0 END,
        datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + excluded.row_count,
        byte_count = byte_count + excluded.byte_count,
        exempt_row_count = exempt_row_count + excluded.exempt_row_count,
        exempt_byte_count = exempt_byte_count + excluded.exempt_byte_count,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER mem_events_au_owner_usage AFTER UPDATE OF scope_owner ON mem_events
WHEN old.scope_owner IS NOT new.scope_owner
BEGIN
    UPDATE mem_owner_usage SET
        row_count = MAX(row_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 0 ELSE 1 END, 0),
        byte_count = MAX(byte_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 0 ELSE LENGTH(CAST(old.payload AS BLOB)) END, 0),
        exempt_row_count = MAX(exempt_row_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 1 ELSE 0 END, 0),
        exempt_byte_count = MAX(exempt_byte_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN LENGTH(CAST(old.payload AS BLOB)) ELSE 0 END, 0),
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
    INSERT INTO mem_owner_usage
        (owner, row_count, byte_count, exempt_row_count, exempt_byte_count, updated_at)
    VALUES (
        new.scope_owner,
        CASE WHEN (
            old.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = old.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(old.payload)
                 THEN json_type(old.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                      AND json_type(old.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 0 ELSE 1 END,
        CASE WHEN (
            old.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = old.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(old.payload)
                 THEN json_type(old.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                      AND json_type(old.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 0 ELSE LENGTH(CAST(new.payload AS BLOB)) END,
        CASE WHEN (
            old.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = old.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(old.payload)
                 THEN json_type(old.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                      AND json_type(old.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN 1 ELSE 0 END,
        CASE WHEN (
            old.kind = 'assertion.retracted'
            AND EXISTS (SELECT 1 FROM mem_os_partitions
                        WHERE owner_key = old.scope_owner AND space_kind = 'personal')
            AND CASE WHEN json_valid(old.payload)
                 THEN json_type(old.payload) = 'object'
                      AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                      AND json_type(old.payload, '$.assertion_id') = 'text'
                      AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                 ELSE 0 END
        ) THEN LENGTH(CAST(new.payload AS BLOB)) ELSE 0 END,
        datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + excluded.row_count,
        byte_count = byte_count + excluded.byte_count,
        exempt_row_count = exempt_row_count + excluded.exempt_row_count,
        exempt_byte_count = exempt_byte_count + excluded.exempt_byte_count,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER mem_events_ad_usage AFTER DELETE ON mem_events BEGIN
    UPDATE mem_owner_usage SET
        row_count = MAX(row_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 0 ELSE 1 END, 0),
        byte_count = MAX(byte_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 0 ELSE LENGTH(CAST(old.payload AS BLOB)) END, 0),
        exempt_row_count = MAX(exempt_row_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN 1 ELSE 0 END, 0),
        exempt_byte_count = MAX(exempt_byte_count - CASE WHEN (
                old.kind = 'assertion.retracted'
                AND EXISTS (SELECT 1 FROM mem_os_partitions
                            WHERE owner_key = old.scope_owner AND space_kind = 'personal')
                AND CASE WHEN json_valid(old.payload)
                     THEN json_type(old.payload) = 'object'
                          AND (SELECT COUNT(*) FROM json_each(old.payload)) = 1
                          AND json_type(old.payload, '$.assertion_id') = 'text'
                          AND LENGTH(CAST(old.payload AS BLOB)) <= 256
                     ELSE 0 END
            ) THEN LENGTH(CAST(old.payload AS BLOB)) ELSE 0 END, 0),
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
END;

-- The gate: identical to 0014 (including the `id` replay guard and the
-- COALESCE-outside-the-subquery fix) except that an exempt row is skipped.
CREATE TRIGGER mem_events_bi_quota BEFORE INSERT ON mem_events
WHEN NOT EXISTS (SELECT 1 FROM mem_events WHERE id = new.id)
     AND NOT (
         new.kind = 'assertion.retracted'
         AND EXISTS (SELECT 1 FROM mem_os_partitions
                     WHERE owner_key = new.scope_owner AND space_kind = 'personal')
         AND CASE WHEN json_valid(new.payload)
              THEN json_type(new.payload) = 'object'
                   AND (SELECT COUNT(*) FROM json_each(new.payload)) = 1
                   AND json_type(new.payload, '$.assertion_id') = 'text'
                   AND LENGTH(CAST(new.payload AS BLOB)) <= 256
              ELSE 0 END
     )
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
