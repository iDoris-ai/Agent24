-- MD-6: local vector index — an OPTIONAL, rebuildable projection over the
-- assertion ledger for semantic recall (SPEC-MD-ME §3 MD-6; local-first, no
-- mandatory vector service).
--
-- An assertion may hold embeddings under SEVERAL (model_id, revision) pairs at
-- once (mixed-version): search uses only the CURRENT model's rows, so a model
-- change does not corrupt recall — it just falls back to FTS until reindexed, and
-- old-model rows stay put (reindex never drops them). Vectors are stored as raw
-- little-endian f32 BLOBs; cosine similarity is computed in Rust (brute-force,
-- local-first — no external vector engine).

CREATE TABLE mem_embeddings (
    assertion_id TEXT    NOT NULL,
    scope_owner  TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    model_id     TEXT    NOT NULL,
    revision     TEXT    NOT NULL,
    dims         INTEGER NOT NULL CHECK (dims > 0),
    normalized   INTEGER NOT NULL CHECK (normalized IN (0, 1)),
    -- the blob must hold exactly dims × 4 bytes (dims f32s): a length mismatch is
    -- a corrupt/misbehaving embedder, not a silently-zero-scoring row (review #123 M2).
    vec          BLOB    NOT NULL CHECK (length(vec) = dims * 4),
    at           TEXT    NOT NULL,
    PRIMARY KEY (assertion_id, model_id, revision)
);

CREATE INDEX idx_emb_owner_model ON mem_embeddings (scope_owner, model_id, revision);
