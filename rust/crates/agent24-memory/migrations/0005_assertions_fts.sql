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
