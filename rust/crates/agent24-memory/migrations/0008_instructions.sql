-- MD-7: the knowledge/instruction layer (L4) — hierarchical, CLAUDE.md-style
-- instructions merged by precedence, with trigger injection and a REVIEW-GATED
-- auto-memory inbox (SPEC-MD-ME §3 MD-7; gemini-cli's "never auto-apply").
--
-- `status = 'active'` instructions are human-authored or human-approved and are
-- what `merged()`/`triggered()` return. `status = 'pending'` is an auto-memory
-- PROPOSAL in the inbox: it is NEVER part of the merged instructions until a human
-- approves it — the whole point of the review gate.

CREATE TABLE mem_instructions (
    id          TEXT    NOT NULL PRIMARY KEY,
    scope_owner TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    layer       TEXT    NOT NULL,                       -- e.g. global | project | session
    priority    INTEGER NOT NULL,                       -- higher = later = takes precedence
    body        TEXT    NOT NULL,
    triggers    TEXT    NOT NULL,                        -- JSON array of trigger strings
    status      TEXT    NOT NULL CHECK (status IN ('active', 'pending')),
    at          TEXT    NOT NULL
);

CREATE INDEX idx_instr_owner_status ON mem_instructions (scope_owner, status, priority);
