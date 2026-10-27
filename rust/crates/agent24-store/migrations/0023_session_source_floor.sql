-- K1-6b.4c (ADR-K1-02 §2.2): a session's monotonic source floor.
--
-- The strictest classification of every turn context this session has fed
-- to a model. Later turns carry their earlier turns' content (session
-- context on /runs, client-supplied history on /api/v1/chat), so each turn
-- merges its own classification in before its model call and reads the
-- floor back as one of its sources. It only ever rises; a session with prior
-- context but no row here is treated as `unknown`, never `ordinary`.
--
-- `session_id` is the memory session id shared by /runs and /api/v1/chat; a
-- chat session id is client-generated and need not exist in `sessions`, so
-- there is no foreign key.
CREATE TABLE session_source_floor (
    session_id     TEXT NOT NULL PRIMARY KEY,
    classification TEXT NOT NULL CHECK (classification IN ('ordinary', 'unknown', 'restricted')),
    updated_at     TEXT NOT NULL
);
