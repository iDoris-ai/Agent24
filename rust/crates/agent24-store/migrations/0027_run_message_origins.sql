-- R05 storage atom. Orthogonal to K1 run_source_tags.
-- One row per run_messages instance. A missing row is absence, not NoRecall.
-- This migration does not backfill old runs and does not insert privacy tags.
CREATE TABLE run_message_origins (
    run_id          TEXT    NOT NULL,
    seq             INTEGER NOT NULL,
    schema_version  INTEGER NOT NULL,
    kind            TEXT    NOT NULL CHECK (kind IN ('HostRecall', 'UserPrompt', 'SessionHistory')),
    instance_digest TEXT    NOT NULL,
    recall_ref      TEXT,
    PRIMARY KEY (run_id, seq),
    FOREIGN KEY (run_id, seq) REFERENCES run_messages (run_id, seq) ON DELETE CASCADE
);
