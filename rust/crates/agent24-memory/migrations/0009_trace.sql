-- MD-8: the symbolic task trace (H1/H2) — full tool output is spilled to a `ref`
-- blob and the prompt keeps only a compact SYMBOLIC node that can drill back down
-- (SPEC-MD-ME §3 MD-8; TencentDB's symbolic graph + node_id drill-down).
--
-- The key property is that compression is RECOVERABLE, not truncating: the full
-- body is stored verbatim in `mem_trace_refs` and the node in `mem_trace_nodes`
-- carries a one-line symbol plus that ref. 100% of the original is retrievable —
-- nothing is discarded.
--
-- Identity choices (review #125):
-- * refs are keyed by (scope_owner, ref_id), NOT ref_id alone. A pure content
--   address would dedupe ACROSS owners while both read paths join within an
--   owner, so the second owner to record an identical body could not read it
--   back — drill → None and expand_run SILENTLY SHORTER. Dedup stays where it is
--   meaningful (within an owner); identical bodies across owners store one row
--   each.
-- * a node's natural key is (scope_owner, run_id, seq) — one step of one run —
--   and that is the PRIMARY KEY, so re-recording a step is a clean idempotent
--   upsert. `node_id` is a derived handle with a UNIQUE index, not a second,
--   competing conflict identity.
-- * the composite FOREIGN KEY makes a node's ref unresolvable-by-construction
--   impossible, so the reader's join can never silently drop a step.

CREATE TABLE mem_trace_refs (
    scope_owner TEXT NOT NULL CHECK (trim(scope_owner) <> ''),
    ref_id      TEXT NOT NULL,               -- content hash of body (within owner)
    body        TEXT NOT NULL,
    -- BYTE length, so the check matches Rust's `body.len()`. SQLite's `length()`
    -- on TEXT counts CHARACTERS; casting to BLOB counts bytes (review #125: the
    -- column was previously written but never validated).
    bytes       INTEGER NOT NULL CHECK (bytes = length(CAST(body AS BLOB))),
    at          TEXT NOT NULL,
    PRIMARY KEY (scope_owner, ref_id)
);

CREATE TABLE mem_trace_nodes (
    scope_owner TEXT NOT NULL CHECK (trim(scope_owner) <> ''),
    run_id      TEXT NOT NULL CHECK (trim(run_id) <> ''),
    seq         INTEGER NOT NULL CHECK (seq >= 0),
    node_id     TEXT NOT NULL,               -- derived handle for drill-down
    kind        TEXT NOT NULL,
    symbol      TEXT NOT NULL,               -- the compact line kept in-prompt
    ref_id      TEXT NOT NULL,
    at          TEXT NOT NULL,
    PRIMARY KEY (scope_owner, run_id, seq),
    FOREIGN KEY (scope_owner, ref_id) REFERENCES mem_trace_refs (scope_owner, ref_id)
);

CREATE UNIQUE INDEX idx_trace_nodes_node_id ON mem_trace_nodes (scope_owner, node_id);
