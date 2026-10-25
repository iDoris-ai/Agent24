-- K1-6a-G §2.5.1 (A5c): the ends of grants a module must be told of, one
-- row per revocation, replacement of a granted row, or enable epoch advance,
-- written in the transaction of the write that ended them: a crash before
-- the notice, or a later decision replacing the row the grant was, loses
-- none. `op` is the tool, or '*' for every tool of the module;
-- `authorization_refs` a JSON array of the ended grants' `ga1:` references
-- (empty when a revocation found none in force). `stop_state` is what the
-- module answered ('confirmed', 'unconfirmed', 'not_running'), NULL until
-- the kernel heard back. `routed` marks a revocation its route tells and
-- answers itself, so it is left to the route a while; any other end, a
-- revocation by a disable, stop or package change too, is pending at once.
-- `settled_by` is the revocation that carried the
-- references of a row not confirmed stopped and was itself confirmed
-- (#1202 G7): such a row is neither carried again nor still pending.
CREATE TABLE module_grant_end_notices (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    module             TEXT NOT NULL,
    op                 TEXT NOT NULL,
    authorization_refs TEXT NOT NULL,
    reason             TEXT NOT NULL CHECK (reason IN ('revoked', 'replaced', 'epoch')),
    ended_at           TEXT NOT NULL,
    generation         INTEGER NOT NULL UNIQUE,
    routed             INTEGER NOT NULL DEFAULT 0 CHECK (routed IN (0, 1)),
    stop_state         TEXT,
    settled_by         INTEGER REFERENCES module_grant_end_notices (id)
);
CREATE INDEX module_grant_end_notices_unanswered
    ON module_grant_end_notices (id) WHERE stop_state IS NULL;
