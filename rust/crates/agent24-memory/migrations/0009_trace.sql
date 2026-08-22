-- MD-8: the symbolic task trace (H1/H2) — full tool output is spilled to a `ref`
-- blob and the prompt keeps only a compact SYMBOLIC node that can drill back down
-- (SPEC-MD-ME §3 MD-8; TencentDB's symbolic graph + node_id drill-down).
--
-- The key property is that compression is RECOVERABLE, not truncating: the full
-- body is stored verbatim in `mem_trace_refs` (keyed by a content hash), and the
-- node in `mem_trace_nodes` carries a one-line symbol plus that ref id. 100% of
-- the original is retrievable via the node's ref — nothing is discarded.

CREATE TABLE mem_trace_refs (
    ref_id      TEXT NOT NULL PRIMARY KEY,      -- content hash of body
    scope_owner TEXT NOT NULL CHECK (trim(scope_owner) <> ''),
    body        TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    at          TEXT NOT NULL
);

CREATE TABLE mem_trace_nodes (
    node_id     TEXT NOT NULL PRIMARY KEY,
    scope_owner TEXT NOT NULL CHECK (trim(scope_owner) <> ''),
    run_id      TEXT NOT NULL,
    seq         INTEGER NOT NULL,               -- order within the run
    kind        TEXT NOT NULL,                  -- e.g. tool name
    symbol      TEXT NOT NULL,                  -- the compact line kept in-prompt
    ref_id      TEXT NOT NULL,                  -- drill-down target (mem_trace_refs)
    at          TEXT NOT NULL,
    UNIQUE (scope_owner, run_id, seq)
);

CREATE INDEX idx_trace_nodes_run ON mem_trace_nodes (scope_owner, run_id, seq);
CREATE INDEX idx_trace_refs_owner ON mem_trace_refs (scope_owner);
