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
    dims         INTEGER NOT NULL,
    normalized   INTEGER NOT NULL,
    vec          BLOB    NOT NULL,          -- dims × f32, little-endian
    at           TEXT    NOT NULL,
    PRIMARY KEY (assertion_id, model_id, revision)
);

CREATE INDEX idx_emb_owner_model ON mem_embeddings (scope_owner, model_id, revision);
