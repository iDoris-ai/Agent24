-- F5b: give `mem_events` the owner CHECK the whole schema INTENDED.
--
-- 0002 shipped `CHECK(scope_owner <> '')`; every table added afterwards uses
-- `CHECK(trim(scope_owner) <> '')`. A space-only owner therefore passed HERE and
-- was rejected everywhere else — an event the assertion ledger, the consolidation
-- projection and the trace could never own, so memory that only ONE of the eleven
-- tables can hold, quietly.
--
-- Writing the regression for this turned up something the siblings' rule does not
-- actually say: SQLite's one-argument `trim(X)` strips SPACES ONLY. A TAB-only
-- owner passes `trim(scope_owner) <> ''` — so the eight tables that look stricter
-- than 0002 share the same hole for every whitespace character except U+0020.
--
-- So this does not copy the siblings' rule; it states the intended one, naming
-- the characters. Bringing the other eight up to it needs a rebuild each and is
-- recorded as a follow-up (SPEC-ME-FOLLOWUPS F5b) rather than smuggled in here.
--
-- 0002 is a released migration and sqlx checksums it, so it cannot be edited —
-- the constraint is tightened by rebuilding the table here. SQLite cannot ALTER
-- a CHECK either, which is why this is a full copy rather than one statement.
--
-- Existing rows are carried over verbatim. Any row that would fail the new
-- constraint is carried over TOO, deliberately: this is an append-only log and
-- the authority for everything else, so silently dropping history to satisfy a
-- constraint would be a far worse trade than a stricter rule applying only to
-- new writes. (There should be none — the daemon has never written one — and if
-- there are, they stay visible rather than disappearing.)
PRAGMA foreign_keys = OFF;

CREATE TABLE mem_events_new (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    id            TEXT NOT NULL UNIQUE,
    -- Space, tab, LF, CR — spelled out, because one-argument trim() would only
    -- remove the first of them.
    scope_owner   TEXT NOT NULL
        CHECK (trim(scope_owner, char(32) || char(9) || char(10) || char(13)) <> ''),
    scope_session TEXT,
    scope         TEXT NOT NULL,
    kind          TEXT NOT NULL,
    payload       TEXT NOT NULL,
    origin_source TEXT NOT NULL,
    origin_trust  TEXT NOT NULL,
    causal        TEXT NOT NULL DEFAULT '[]',
    at            TEXT NOT NULL
);

-- `seq` is copied, not regenerated: it is the projection checkpoint order, and
-- renumbering it would make every stored checkpoint point at a different event.
INSERT INTO mem_events_new
    (seq, id, scope_owner, scope_session, scope, kind, payload,
     origin_source, origin_trust, causal, at)
SELECT
     seq, id, scope_owner, scope_session, scope, kind, payload,
     origin_source, origin_trust, causal, at
FROM mem_events;

DROP TABLE mem_events;
ALTER TABLE mem_events_new RENAME TO mem_events;

-- Recreate 0002's indexes, under THEIR names: a rebuild drops them with the
-- table, and recreating them under new names would leave 0002 describing indexes
-- that no longer exist.
CREATE INDEX mem_events_owner_seq   ON mem_events(scope_owner, seq);
CREATE INDEX mem_events_session_seq ON mem_events(scope_owner, scope_session, seq);

PRAGMA foreign_keys = ON;
