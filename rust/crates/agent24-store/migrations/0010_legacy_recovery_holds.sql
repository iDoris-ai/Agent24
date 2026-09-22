-- G4 dormant recovery cohort and hold markers; no writer or runtime behavior.
CREATE UNIQUE INDEX idx_workspaces_legacy_singleton
    ON workspaces(kind) WHERE kind = 'legacy_compat';
CREATE UNIQUE INDEX idx_approvals_id_run ON approvals(id, run_id);
CREATE TABLE legacy_recovery_cohorts (
    cohort_id TEXT PRIMARY KEY NOT NULL
        CHECK (length(cohort_id) > 0 AND instr(cohort_id, char(0)) = 0),
    migration_version INTEGER NOT NULL UNIQUE
        CHECK (typeof(migration_version) = 'integer' AND migration_version > 0),
    legacy_workspace_id TEXT NOT NULL,
    root_generation TEXT NOT NULL,
    created_at TEXT NOT NULL CHECK (length(created_at) = 24
        AND created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
        AND julianday(created_at) IS NOT NULL
        AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(created_at)) = created_at),
    completed_at TEXT CHECK (completed_at IS NULL OR (length(completed_at) = 24
        AND completed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
        AND julianday(completed_at) IS NOT NULL
        AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(completed_at)) = completed_at
        AND completed_at >= created_at)),
    FOREIGN KEY (legacy_workspace_id, root_generation)
        REFERENCES workspaces(id, root_generation),
    UNIQUE (cohort_id, legacy_workspace_id, root_generation)
);

CREATE TABLE legacy_recovery_holds (
    run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id),
    cohort_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    root_generation TEXT NOT NULL,
    original_status TEXT NOT NULL CHECK (original_status IN ('queued', 'running', 'awaiting_approval')),
    recovery_state TEXT NOT NULL CHECK (recovery_state IN ('awaiting_decision', 'ready', 'active', 'needs_attention', 'released')),
    approval_id TEXT,
    ready_at TEXT CHECK (ready_at IS NULL OR (length(ready_at) = 24
        AND ready_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
        AND julianday(ready_at) IS NOT NULL
        AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(ready_at)) = ready_at)),
    reason_code TEXT CHECK (reason_code IS NULL OR (length(reason_code) > 0
        AND reason_code NOT GLOB '*[^a-z0-9_-]*' AND instr(reason_code, char(0)) = 0)),
    released_at TEXT CHECK (released_at IS NULL OR (length(released_at) = 24
        AND released_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
        AND julianday(released_at) IS NOT NULL
        AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(released_at)) = released_at)),
    active_resume_approval_id TEXT,
    FOREIGN KEY (cohort_id, workspace_id, root_generation)
        REFERENCES legacy_recovery_cohorts(cohort_id, legacy_workspace_id, root_generation),
    FOREIGN KEY (approval_id, run_id) REFERENCES approvals(id, run_id),
    FOREIGN KEY (active_resume_approval_id, run_id) REFERENCES approvals(id, run_id),
    CHECK (recovery_state NOT IN ('awaiting_decision', 'ready') OR approval_id IS NOT NULL),
    CHECK (recovery_state != 'ready' OR ready_at IS NOT NULL),
    CHECK (recovery_state != 'needs_attention' OR reason_code IS NOT NULL),
    CHECK ((recovery_state = 'released') = (released_at IS NOT NULL)),
    CHECK ((active_resume_approval_id IS NULL AND recovery_state != 'active')
        OR (active_resume_approval_id IS NOT NULL AND recovery_state = 'active'
            AND approval_id IS NOT NULL AND active_resume_approval_id = approval_id))
);
CREATE INDEX idx_legacy_recovery_ready ON legacy_recovery_holds(ready_at, run_id)
    WHERE recovery_state = 'ready';
CREATE INDEX idx_legacy_recovery_unreleased ON legacy_recovery_holds(workspace_id, run_id)
    WHERE released_at IS NULL;
