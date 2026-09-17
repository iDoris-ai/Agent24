-- T7b/ME-3e: module (gate/advise) approvals — a PARALLEL, independent table
-- from `approvals` above (agent24-policy's run_id-tied, synchronous-wait
-- model; see docs/design/T7b-ME3e-approvals.md "现状" 4). Async
-- submit-then-poll (decision 3): one decision dimension, no "delivered"
-- state — a decision is a terminal fact the instant it is made.
--
-- (module, request_id, kind) is UNIQUE: both a data-integrity constraint and
-- the mechanism the wire handler's idempotent resubmission relies on
-- (decision 4) — a lost response, retried by the module with the same
-- request_id, lands on the same row rather than a second one.
CREATE TABLE module_approvals (
    id             TEXT PRIMARY KEY,  -- 32 bytes random, hex — NOT a ULID,
                                       -- unlike every other id in this system
                                       -- (decision 3): it must not be
                                       -- derivable from request_id or
                                       -- anything else predictable.
    module         TEXT NOT NULL,
    request_id     TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('gate', 'advise')),
    binding        INTEGER NOT NULL,  -- 0/1; always 0 for advise. gate is
                                       -- unreachable this round (T7b: the
                                       -- kernel-executable closed set is
                                       -- empty, so no gate row is ever
                                       -- inserted) but the column exists so a
                                       -- future non-empty closed set needs no
                                       -- wire/schema change.
    action         TEXT NOT NULL,
    target         TEXT,
    payload        TEXT NOT NULL,     -- JSON: the kernel's own record of what
                                       -- it received at submission time, not
                                       -- a promise the module acts on it
    payload_digest TEXT NOT NULL,     -- approval_digest(&payload) (decision
                                       -- 6), computed once, at submission
    decision       TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'denied', 'timed_out')),
    created_at     TEXT NOT NULL,
    decided_at     TEXT,              -- NULL iff decision = 'pending'
    expires_at     TEXT NOT NULL,     -- decision 5's periodic scan judges
                                       -- this field
    UNIQUE (module, request_id, kind),
    CHECK (
        (decision = 'pending' AND decided_at IS NULL)
        OR (decision != 'pending' AND decided_at IS NOT NULL)
    )
);

-- Decision 5's periodic scan filters `decision = 'pending' AND expires_at <
-- ?`; the decision CAS filters `decision = 'pending' AND expires_at >= ?`.
-- Same two columns, so one partial index (pending rows only — the ones that
-- can still change) serves both without a full-table scan as history grows.
CREATE INDEX idx_module_approvals_pending_expiry
    ON module_approvals (decision, expires_at)
    WHERE decision = 'pending';
