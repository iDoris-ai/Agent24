-- MD-7: the knowledge/instruction layer (L4) — hierarchical, CLAUDE.md-style
-- instructions merged by precedence, with trigger injection and a REVIEW-GATED
-- auto-memory inbox (SPEC-MD-ME §3 MD-7; gemini-cli's "never auto-apply").
--
-- `status = 'active'` instructions are human-authored or human-approved and are
-- what `merged()`/`triggered()` return. `status = 'pending'` is an auto-memory
-- PROPOSAL in the inbox: it is NEVER part of the merged instructions until a human
-- approves it — the whole point of the review gate.
--
-- The row identity is the PAIR (scope_owner, id), NOT the caller-minted id alone:
-- ids are caller-chosen, so a bare-id primary key lets one owner overwrite
-- another's instruction while the row keeps the victim's scope_owner —
-- cross-tenant rewrite plus forged attribution (review #124 B1).
--
-- Content columns are non-empty-checked; `triggers` must be valid JSON.

CREATE TABLE mem_instructions (
    scope_owner TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    id          TEXT    NOT NULL CHECK (trim(id) <> ''),
    layer       TEXT    NOT NULL CHECK (trim(layer) <> ''),  -- e.g. global | project | session
    priority    INTEGER NOT NULL,                            -- higher = later = takes precedence
    body        TEXT    NOT NULL CHECK (trim(body) <> ''),
    triggers    TEXT    NOT NULL CHECK (json_valid(triggers)),
    status      TEXT    NOT NULL CHECK (status IN ('active', 'pending')),
    at          TEXT    NOT NULL,
    PRIMARY KEY (scope_owner, id)
);

CREATE INDEX idx_instr_owner_status ON mem_instructions (scope_owner, status, priority);
