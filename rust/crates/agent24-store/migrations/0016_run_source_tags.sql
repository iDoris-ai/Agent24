-- K1-6b.1 (ADR-K1-02 §6, slice 6b.1): source tags & persistent run metadata.
--
-- A trusted-host label attached to a block of content entering a run's
-- context — user input at run entry today; "selected material" once a
-- future ID-2 entry point wires one (see
-- `agent24-store/src/source_policy.rs` module docs). This table ONLY stores
-- the label; nothing in Agent24 yet reads it to gate any output or routing
-- decision (that is K1-6b.2/6b.3 — out of scope here). Reading it back is
-- itself fail-closed: a row whose `schema_version` does not match the
-- crate's current `SOURCE_TAG_SCHEMA_VERSION` is treated as `LocalOnly`
-- WITHOUT even attempting to parse `tag_json` (ADR-K1-02 §0: "未知、缺失、
-- 过期...一律按 LocalOnly 处理").
--
-- `tag_json` is the full serde_json encoding of
-- `agent24_store::source_policy::SourceRef`. `source_id` and
-- `schema_version` are pulled out as their own columns (rather than relying
-- on parsing `tag_json`) so a reader can address/identify a row and detect a
-- schema mismatch even when `tag_json`'s shape has drifted.
CREATE TABLE run_source_tags (
    run_id         TEXT    NOT NULL REFERENCES runs(id),
    seq            INTEGER NOT NULL,   -- ties to run_messages(run_id, seq)
    source_id      TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    tag_json       TEXT    NOT NULL,
    created_at     TEXT    NOT NULL,
    PRIMARY KEY (run_id, seq, source_id)
);
CREATE INDEX idx_run_source_tags_run ON run_source_tags(run_id);
