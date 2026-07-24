-- H4: target-scoped standing grants.
--
-- One row = "this tool, aimed at exactly this target, is pre-authorised for
-- this session or schedule". Never "this tool, whatever the arguments" — that
-- is the broad `approve_for_session` grant, which stays in memory, dies with
-- the process, and is no longer offered for `external` tools at all.
--
-- These rows are PERSISTENT on purpose: the case they exist for is the 3am
-- unattended run, which by definition happens in a process the user was not
-- present for. An in-memory grant would be gone by then and the automation
-- would block on a human who is asleep.
--
-- scope_kind = 'schedule' when the run was fired by one. Ownership follows what
-- the user was actually consenting to: they approved *this automation* sending
-- to *this address*, so deleting the automation must take the grant with it
-- (see `delete_schedule`).
CREATE TABLE standing_grants (
    id         TEXT PRIMARY KEY,
    scope_kind TEXT NOT NULL,   -- session | schedule
    scope_id   TEXT NOT NULL,
    tool       TEXT NOT NULL,
    target     TEXT NOT NULL,   -- exact value of the tool's declared target arg
    created_at TEXT NOT NULL,
    UNIQUE (scope_kind, scope_id, tool, target)
);

CREATE INDEX standing_grants_scope ON standing_grants (scope_kind, scope_id);

-- The approval row records the target its `approve_for_target` decision would
-- bind to, so a client that lists pending approvals (or a restart) can label
-- the choice with what it actually authorises rather than an unqualified
-- "always allow".
ALTER TABLE approvals ADD COLUMN standing_target TEXT;
