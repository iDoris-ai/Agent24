-- SQLite cannot change a CHECK in place. Copy origin rows only.
CREATE TABLE run_message_origins_next (
    run_id          TEXT    NOT NULL,
    seq             INTEGER NOT NULL,
    schema_version  INTEGER NOT NULL,
    kind            TEXT    NOT NULL CHECK (kind IN ('HostRecall', 'UserPrompt', 'SessionHistory', 'Assistant', 'ToolResult')),
    instance_digest TEXT    NOT NULL,
    recall_ref      TEXT,
    PRIMARY KEY (run_id, seq),
    FOREIGN KEY (run_id, seq) REFERENCES run_messages (run_id, seq) ON DELETE CASCADE
);

INSERT INTO run_message_origins_next (
    run_id, seq, schema_version, kind, instance_digest, recall_ref
)
SELECT run_id, seq, schema_version, kind, instance_digest, recall_ref
FROM run_message_origins;

DROP TABLE run_message_origins;

ALTER TABLE run_message_origins_next RENAME TO run_message_origins;
