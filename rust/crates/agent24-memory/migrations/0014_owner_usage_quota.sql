-- T8.5b: authoritative event usage counting + quota gate, rekey-safe.
--
-- See docs/design/T8.5b-authoritative-quota.md for the full design and the
-- verification behind every choice below. Summary of what this migration is
-- and is not:
--
-- `mem_owner_usage` is a DERIVED projection of `mem_events` (row_count +
-- byte_count per scope_owner), kept authoritative by triggers that attach to
-- `mem_events` ITSELF rather than to any Rust call site. That is the whole
-- point: `EventLog::append` (pool, ON CONFLICT DO NOTHING), `EventLog::append_tx`
-- (caller's transaction, plain INSERT, used by writer.rs::commit_with_audit),
-- `KvStore::rekey_os_partition`'s batch `UPDATE mem_events SET scope_owner = ?`,
-- and even a raw SQL statement that never goes through `EventLog` at all (the
-- one existing example is the #[cfg(test)] `DELETE FROM mem_events` in
-- consolidator.rs) are ALL covered automatically, because none of them can
-- avoid the table these triggers are attached to.
--
-- `mem_owner_quota` is plain CONFIGURATION, not usage — a max_rows/max_bytes
-- ceiling per owner, with `'*'` as the fallback default. Deliberately a
-- separate table from `mem_owner_usage` (see the design doc's "why not one
-- table"): usage is written by triggers on every event write; quota is written
-- by whatever future config layer wants to change a limit. Mixing the two
-- would make "is this row a count or a limit" a matter of guessing from the
-- column name.

CREATE TABLE mem_owner_usage (
    owner      TEXT PRIMARY KEY,
    row_count  INTEGER NOT NULL DEFAULT 0,
    -- L1: PAYLOAD BYTES ONLY — `LENGTH(CAST(mem_events.payload AS BLOB))`, the
    -- JSON body column. This is NOT the row's total on-disk footprint: the
    -- `scope` JSON, `causal` JSON, `origin_source`/`origin_trust`/`id`/`at`
    -- columns and all index overhead are excluded. A future consumer of this
    -- column must not read it as "how much disk this owner occupies" — it
    -- answers "how much event-body content this owner has written".
    byte_count INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

-- Quota configuration. Not the real config system (see design doc's "not
-- designing"): a two-column table with a `'*'` fallback row and optional
-- per-owner overrides. Any future config layer only ever needs to
-- `INSERT ... ON CONFLICT(owner) DO UPDATE` this table — no trigger SQL, no
-- migration, ever needs to change to plug one in.
CREATE TABLE mem_owner_quota (
    owner     TEXT PRIMARY KEY,
    max_rows  INTEGER NOT NULL,
    max_bytes INTEGER NOT NULL
);

-- M1: protect the `'*'` fallback row from deletion. Without this, an owner
-- with no override row falls through BOTH levels of the COALESCE chain in
-- `mem_events_bi_quota` below to NULL — and `WHERE ... > NULL` is false in
-- SQLite, so the RAISE simply never fires. Quota would degrade from "reject
-- over-limit writes" to "silently accept everything", with no error and no
-- trace that it happened. This trigger makes that degradation impossible
-- instead of merely documenting the operational rule not to delete the row.
CREATE TRIGGER mem_owner_quota_bd_protect_default
BEFORE DELETE ON mem_owner_quota
WHEN old.owner = '*'
BEGIN
    SELECT RAISE(ABORT, 'cannot delete default quota');
END;

-- Medium (Codex code review): DELETE isn't the only way to make the `'*'`
-- row stop being `'*'` — `UPDATE mem_owner_quota SET owner = 'x' WHERE owner
-- = '*'` renames it away just as effectively, and the DELETE guard above
-- doesn't fire for an UPDATE. Same failure mode as the original M1 (every
-- owner with no override row falls through to NULL, quota silently stops
-- being enforced), just via a different statement shape.
CREATE TRIGGER mem_owner_quota_bu_protect_default
BEFORE UPDATE OF owner ON mem_owner_quota
WHEN old.owner = '*' AND new.owner IS NOT '*'
BEGIN
    SELECT RAISE(ABORT, 'cannot rename default quota');
END;

-- Guessed, not measured (design doc decision 7): MAX_BODY_BYTES is 64 KiB per
-- event (os_memory.rs), so 200000 rows at a few hundred bytes average lands in
-- the tens-of-MB range — enough headroom that a runaway module cannot fill
-- memory.db in minutes, nothing more precise than that. Any owner can be given
-- a tighter or looser limit later with a plain UPDATE of this table.
INSERT INTO mem_owner_quota (owner, max_rows, max_bytes)
VALUES ('*', 200000, 268435456);

-- Backfill: this migration runs on databases that already hold `mem_events`
-- rows written before `mem_owner_usage` existed. A one-time GROUP BY over the
-- full table, not "let the triggers below catch up" — the triggers only see
-- FUTURE writes. Same shape as MD-3b's FTS backfill (0005) for the same
-- reason: an upgraded instance must not silently show zero usage for owners
-- who already have history.
INSERT INTO mem_owner_usage (owner, row_count, byte_count, updated_at)
SELECT scope_owner, COUNT(*), SUM(LENGTH(CAST(payload AS BLOB))), datetime('now')
FROM mem_events
GROUP BY scope_owner;

-- AFTER INSERT: the only path that grows a partition's usage.
CREATE TRIGGER mem_events_ai_usage AFTER INSERT ON mem_events BEGIN
    INSERT INTO mem_owner_usage (owner, row_count, byte_count, updated_at)
    VALUES (new.scope_owner, 1, LENGTH(CAST(new.payload AS BLOB)), datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + 1,
        byte_count = byte_count + LENGTH(CAST(new.payload AS BLOB)),
        updated_at = excluded.updated_at;
END;

-- AFTER UPDATE OF scope_owner: covers `KvStore::rekey_os_partition`'s batch
-- `UPDATE mem_events SET scope_owner = ? WHERE scope_owner = ?`. SQLite
-- triggers are row-level (there is no statement-level trigger), so a rekey
-- that moves N rows fires this exactly N times — the usage row for the new
-- owner ends up an exact match for the events actually moved, without
-- `rekey_os_partition` needing to know `mem_owner_usage` exists or move it
-- itself.
--
-- L3: this makes a rekey's constant factor bigger — 2 extra DML statements
-- (one decrement on the old owner, one increment on the new owner) per row
-- moved, on top of the `UPDATE mem_events` row itself — but the growth is
-- still O(N) in the partition size, not a new order of complexity. Rekey only
-- runs once per partition, at `MemoryLease::open` startup, before any module
-- is mounted — never on a request-serving hot path — so this cost is paid
-- once at boot, not per write.
CREATE TRIGGER mem_events_au_owner_usage AFTER UPDATE OF scope_owner ON mem_events
WHEN old.scope_owner IS NOT new.scope_owner
BEGIN
    -- L2: MAX(x - 1, 0) / MAX(x - n, 0) clamp the decrement side so a counter
    -- can never go negative even under an unforeseen bookkeeping bug —
    -- defense in depth, not a fix for a known way to trigger it.
    UPDATE mem_owner_usage SET
        row_count = MAX(row_count - 1, 0),
        byte_count = MAX(byte_count - LENGTH(CAST(old.payload AS BLOB)), 0),
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
    INSERT INTO mem_owner_usage (owner, row_count, byte_count, updated_at)
    VALUES (new.scope_owner, 1, LENGTH(CAST(new.payload AS BLOB)), datetime('now'))
    ON CONFLICT(owner) DO UPDATE SET
        row_count = row_count + 1,
        byte_count = byte_count + LENGTH(CAST(new.payload AS BLOB)),
        updated_at = excluded.updated_at;
END;

-- BEFORE UPDATE OF payload: `mem_events` is an append-only log — nothing is
-- meant to ever change a row's content after it's written — but SQLite does
-- not enforce that on its own, and none of the three usage-maintaining
-- triggers above fire on (or account for) a payload change: there is no
-- `AFTER UPDATE OF payload` trigger, and the quota gate only runs on INSERT.
-- Without this guard, a hypothetical future `UPDATE mem_events SET payload =
-- ...` would silently desync `byte_count` from reality and bypass the byte
-- quota entirely — the same "authoritative means every SQL shape that CAN
-- touch this table" argument used everywhere else in this file, applied to
-- the one write shape that would otherwise fall through the cracks. Refusing
-- it outright (rather than trying to account for it) matches the existing
-- append-only invariant instead of quietly making it real for the first time.
CREATE TRIGGER mem_events_bu_payload_immutable
BEFORE UPDATE OF payload ON mem_events
BEGIN
    SELECT RAISE(ABORT, 'mem_events.payload is immutable');
END;

-- AFTER DELETE: no production path deletes from `mem_events` today — it is an
-- append-only log by design. The one existing deleter is the #[cfg(test)]
-- raw `DELETE FROM mem_events` in consolidator.rs that simulates an
-- externally-repaired log. Covered defensively anyway, because "authoritative"
-- means every SQL statement that CAN touch this table, not every Rust call
-- site that does today (see the module comment above).
CREATE TRIGGER mem_events_ad_usage AFTER DELETE ON mem_events BEGIN
    -- L2: same MAX(x - 1, 0) clamp as the UPDATE trigger above.
    UPDATE mem_owner_usage SET
        row_count = MAX(row_count - 1, 0),
        byte_count = MAX(byte_count - LENGTH(CAST(old.payload AS BLOB)), 0),
        updated_at = datetime('now')
    WHERE owner = old.scope_owner;
END;

-- BEFORE INSERT: the quota gate. A single INSERT statement (including the
-- BEFORE/AFTER triggers it fires) is SQLite's unit of atomicity — under the
-- single-writer serialization this crate already depends on (WAL +
-- busy_timeout), there is no "check then write" window for a concurrent
-- writer to land in between. `RAISE(ABORT, ...)` fails the whole statement,
-- so a rejected insert leaves `mem_owner_usage` untouched.
--
-- `WHEN NOT EXISTS (SELECT 1 FROM mem_events WHERE id = new.id)` is
-- load-bearing, not decorative (see design doc decision 5): a BEFORE trigger
-- runs BEFORE `ON CONFLICT(id) DO NOTHING` is resolved, so a naive version of
-- this trigger with no WHEN guard would fail a harmless exact replay of an
-- id that already exists once its owner is at quota — turning a legitimate
-- no-op into a spurious QuotaExceeded that Rust-side "is this a replay" logic
-- never gets a chance to run, because the SQL statement has already errored.
-- The guard costs one extra lookup on `id`, which already carries a UNIQUE
-- index — not a new table scan.
CREATE TRIGGER mem_events_bi_quota BEFORE INSERT ON mem_events
WHEN NOT EXISTS (SELECT 1 FROM mem_events WHERE id = new.id)
BEGIN
    -- H1 (Codex code review): a scalar subquery with ZERO matching rows
    -- evaluates to NULL — the inner `COALESCE(row_count, 0)` never even runs,
    -- because there is no row to evaluate it against. A brand-new owner (no
    -- `mem_owner_usage` row yet) therefore made the whole `(...)+1` expression
    -- NULL, and `NULL > x` is false in SQLite, so the RAISE never fired: a
    -- new owner's FIRST write always bypassed both the row and byte quota,
    -- once. Fixed by moving the COALESCE outside the subquery, so a missing
    -- row (not just a NULL column) still coalesces to 0.
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
