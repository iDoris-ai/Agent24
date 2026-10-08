-- K1-6a.3: durable revocations are separate from consent decisions. A
-- module row (op='*') covers every tool; a later explicit consent supersedes
-- an older revocation by its decided_at timestamp.
CREATE TABLE module_consent_revocations (
    module      TEXT NOT NULL,
    op          TEXT NOT NULL,
    revoked_at  TEXT NOT NULL,
    PRIMARY KEY (module, op)
);
