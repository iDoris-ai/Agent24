-- K1-6a-G §2.5.1 (A5c): the ends of grants a module must be told of, one
-- row per revocation, replacement of a granted row, or enable epoch advance,
-- written in the transaction of the write that ended them: a crash before
-- the notice, or a later decision replacing the row the grant was, loses
-- none. `op` is the tool, or '*' for every tool of the module;
-- `authorization_refs` a JSON array of the ended grants' `ga1:` references
-- (empty when a revocation found none in force). `stop_state` is what the
-- module answered ('confirmed', 'unconfirmed', 'not_running'), NULL until
-- the kernel heard back.
CREATE TABLE module_grant_end_notices (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    module             TEXT NOT NULL,
    op                 TEXT NOT NULL,
    authorization_refs TEXT NOT NULL,
    reason             TEXT NOT NULL CHECK (reason IN ('revoked', 'replaced', 'epoch')),
    ended_at           TEXT NOT NULL,
    generation         INTEGER NOT NULL UNIQUE,
    stop_state         TEXT
);
CREATE INDEX module_grant_end_notices_unanswered
    ON module_grant_end_notices (id) WHERE stop_state IS NULL;
