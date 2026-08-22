-- MD-3b: an FTS5 full-text PROJECTION over the assertion ledger. A projection,
-- not an authority: it is rebuildable from mem_assertions (see FtsRetriever::rebuild)
-- and could be dropped without losing data (SPEC-MD-ME §0.1 authority+projection).
--
-- The searchable text (subject / predicate / object) is IMMUTABLE for a given
-- assertion — only recorded_to changes over an assertion's life, and that does
-- not affect the text — so a single AFTER INSERT trigger keeps the index in sync;
-- no update/delete triggers are needed. The retriever filters to current +
-- qualified beliefs at query time by joining back to mem_assertions.

CREATE VIRTUAL TABLE mem_assertions_fts USING fts5(
    id UNINDEXED,
    scope_owner UNINDEXED,
    subject,
    predicate,
    object,
    tokenize = 'unicode61'
);

CREATE TRIGGER mem_assertions_fts_ai AFTER INSERT ON mem_assertions BEGIN
    INSERT INTO mem_assertions_fts (id, scope_owner, subject, predicate, object)
    VALUES (new.id, new.scope_owner, new.subject, new.predicate, new.object);
END;

-- BACKFILL existing assertions: the trigger only fires on FUTURE inserts, but
-- this migration runs on live databases that already hold assertions (MD-3a
-- shipped first). Without this, an upgraded instance would silently return zero
-- hits for every historical belief — indistinguishable from "doesn't exist"
-- (review #120 B1). Same statement as FtsRetriever::rebuild.
INSERT INTO mem_assertions_fts (id, scope_owner, subject, predicate, object)
SELECT id, scope_owner, subject, predicate, object FROM mem_assertions;
