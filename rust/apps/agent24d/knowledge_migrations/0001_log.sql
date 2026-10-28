-- The Provider's log and seal (CONTRACT-STATE-v1 A.1-A.3, KB-C1-04 design
-- F2). A record holds only what a change holds: no credential, ticket, JWT
-- or body (R10.3).

-- One row per number and kind: a number may hold a write and its void (A.1).
CREATE TABLE records (
    seq INTEGER NOT NULL CHECK (seq >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('changes', 'void')),
    body TEXT,
    CHECK ((kind = 'void') = (body IS NULL)),
    PRIMARY KEY (seq, kind)
) STRICT, WITHOUT ROWID;

-- The number the log begins at (A.3); no row before the log is begun.
CREATE TABLE log (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    start INTEGER NOT NULL CHECK (start >= 0)
) STRICT;

-- The seal (A.2): the number the log's next record took when it was
-- rebuilt; no row while it is deleted.
CREATE TABLE seal (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next INTEGER NOT NULL CHECK (next >= 0)
) STRICT;
