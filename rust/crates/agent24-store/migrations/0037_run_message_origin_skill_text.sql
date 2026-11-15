-- SQLite cannot change a CHECK in place. Copy origin rows only.
-- Adds SkillText (SL-5: the skill segment injected from a pinned revision).
-- Existing rows are copied as they are. The K1 source label is a separate
-- record: this table alone does not gate outbound.
CREATE TABLE run_message_origins_next (
    run_id          TEXT    NOT NULL,
    seq             INTEGER NOT NULL,
    schema_version  INTEGER NOT NULL,
    kind            TEXT    NOT NULL CHECK (kind IN ('HostRecall', 'UserPrompt', 'SessionHistory', 'Assistant', 'ToolResult', 'HostSystem', 'SkillText')),
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
