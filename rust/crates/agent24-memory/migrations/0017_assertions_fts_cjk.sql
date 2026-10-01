-- CJK bigrams are computed by Rust, so rebuild the FTS projection on open.
DROP TRIGGER mem_assertions_fts_ai;
DROP TABLE mem_assertions_fts;

CREATE VIRTUAL TABLE mem_assertions_fts USING fts5(
    id UNINDEXED,
    scope_owner UNINDEXED,
    subject,
    predicate,
    object,
    cjk,
    tokenize = 'unicode61'
);

CREATE TABLE mem_fts_state(k TEXT PRIMARY KEY, v TEXT);
INSERT INTO mem_fts_state (k, v) VALUES ('needs_rebuild', '1');
