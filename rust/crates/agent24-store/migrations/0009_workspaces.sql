-- A24-OD-01: workspace registry storage. Run/session/approval bindings are
-- deliberately deferred to later additive migrations.

CREATE TABLE workspaces (
    id                       TEXT PRIMARY KEY NOT NULL,
    kind                     TEXT NOT NULL CHECK (kind IN ('orchestrator_scratch', 'legacy_compat')),
    state                    TEXT NOT NULL CHECK (state IN ('active', 'expired', 'releasing', 'released', 'cleanup_failed')),
    provenance_source        TEXT NOT NULL CHECK (length(trim(provenance_source)) > 0 AND instr(provenance_source, char(0)) = 0),
    provenance_project_ref   TEXT CHECK (provenance_project_ref IS NULL OR instr(provenance_project_ref, char(0)) = 0),
    provenance_base_revision TEXT CHECK (provenance_base_revision IS NULL OR instr(provenance_base_revision, char(0)) = 0),
    writeback_policy         TEXT NOT NULL CHECK (writeback_policy = 'external'),
    lifecycle_owner_kind     TEXT NOT NULL CHECK (lifecycle_owner_kind = 'orchestrator'),
    lifecycle_owner_ref      TEXT NOT NULL CHECK (length(trim(lifecycle_owner_ref)) > 0 AND instr(lifecycle_owner_ref, char(0)) = 0),
    concurrency_policy       TEXT NOT NULL CHECK (concurrency_policy = 'serial'),
    created_at               TEXT NOT NULL,
    expires_at               TEXT NOT NULL,
    renewed_at               TEXT,
    released_at              TEXT,
    revision                 INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision >= 1 AND revision <= 9223372036854775807),

    canonical_root           TEXT NOT NULL CHECK (length(trim(canonical_root)) > 0 AND instr(canonical_root, char(0)) = 0),
    root_generation          TEXT NOT NULL CHECK (length(trim(root_generation)) > 0 AND instr(root_generation, char(0)) = 0),
    root_identity_kind       TEXT NOT NULL CHECK (root_identity_kind IN ('unix', 'windows')),
    unix_device              BLOB,
    unix_inode               BLOB,
    windows_volume_serial    BLOB,
    windows_file_id          BLOB,

    quarantine_root          TEXT CHECK (quarantine_root IS NULL OR (length(trim(quarantine_root)) > 0 AND instr(quarantine_root, char(0)) = 0)),
    quarantined_at           TEXT,
    cleanup_attempts         INTEGER NOT NULL DEFAULT 0 CHECK (typeof(cleanup_attempts) = 'integer' AND cleanup_attempts >= 0 AND cleanup_attempts <= 9223372036854775807),
    cleanup_last_attempt_at  TEXT,
    cleanup_error            TEXT CHECK (cleanup_error IS NULL OR instr(cleanup_error, char(0)) = 0),
    cleanup_retry_at         TEXT,

    UNIQUE (id, root_generation),
    CHECK (length(id) = 29 AND instr(id, char(0)) = 0 AND substr(id, 1, 3) = 'ws_'
        AND substr(id, 4, 1) GLOB '[0-7]'
        AND substr(id, 4) NOT GLOB '*[^0-9ABCDEFGHJKMNPQRSTVWXYZ]*'),

    -- Timestamps are canonical UTC RFC3339 with fixed millisecond precision.
    CHECK (length(created_at) = 24 AND created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(created_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(created_at)) = created_at),
    CHECK (length(expires_at) = 24 AND expires_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(expires_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(expires_at)) = expires_at),
    CHECK (renewed_at IS NULL OR (length(renewed_at) = 24 AND renewed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(renewed_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(renewed_at)) = renewed_at)),
    CHECK (released_at IS NULL OR (length(released_at) = 24 AND released_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(released_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(released_at)) = released_at)),
    CHECK (quarantined_at IS NULL OR (length(quarantined_at) = 24 AND quarantined_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(quarantined_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(quarantined_at)) = quarantined_at)),
    CHECK (cleanup_last_attempt_at IS NULL OR (length(cleanup_last_attempt_at) = 24 AND cleanup_last_attempt_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(cleanup_last_attempt_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(cleanup_last_attempt_at)) = cleanup_last_attempt_at)),
    CHECK (cleanup_retry_at IS NULL OR (length(cleanup_retry_at) = 24 AND cleanup_retry_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(cleanup_retry_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(cleanup_retry_at)) = cleanup_retry_at)),
    CHECK (julianday(expires_at) > julianday(created_at)),
    CHECK (renewed_at IS NULL OR (julianday(renewed_at) >= julianday(created_at) AND julianday(renewed_at) < julianday(expires_at))),
    CHECK (
        (CAST(strftime('%s', expires_at) AS INTEGER) * 1000 + CAST(substr(expires_at, 21, 3) AS INTEGER))
        - (CAST(strftime('%s', COALESCE(renewed_at, created_at)) AS INTEGER) * 1000
            + CAST(substr(COALESCE(renewed_at, created_at), 21, 3) AS INTEGER)) <= 604800000
    ),
    CHECK (released_at IS NULL OR julianday(released_at) >= julianday(created_at)),
    CHECK (quarantined_at IS NULL OR julianday(quarantined_at) >= julianday(created_at)),
    CHECK (released_at IS NULL OR renewed_at IS NULL OR julianday(released_at) >= julianday(renewed_at)),
    CHECK (released_at IS NULL OR quarantined_at IS NULL OR julianday(released_at) >= julianday(quarantined_at)),
    CHECK (quarantined_at IS NULL OR renewed_at IS NULL OR
        (CAST(strftime('%s', quarantined_at) AS INTEGER) * 1000 + CAST(substr(quarantined_at, 21, 3) AS INTEGER)) >=
        (CAST(strftime('%s', renewed_at) AS INTEGER) * 1000 + CAST(substr(renewed_at, 21, 3) AS INTEGER))),

    CHECK (
        (root_identity_kind = 'unix'
            AND typeof(unix_device) = 'blob' AND length(unix_device) = 8
            AND typeof(unix_inode) = 'blob' AND length(unix_inode) = 8
            AND windows_volume_serial IS NULL AND windows_file_id IS NULL)
        OR (root_identity_kind = 'windows'
            AND unix_device IS NULL AND unix_inode IS NULL
            AND typeof(windows_volume_serial) = 'blob' AND length(windows_volume_serial) = 8
            AND typeof(windows_file_id) = 'blob' AND length(windows_file_id) = 16)
    ),

    CHECK ((cleanup_attempts = 0 AND cleanup_last_attempt_at IS NULL)
        OR (cleanup_attempts > 0 AND cleanup_last_attempt_at IS NOT NULL)),
    CHECK (quarantined_at IS NULL OR quarantine_root IS NOT NULL),
    CHECK (state != 'active' OR (quarantine_root IS NULL AND quarantined_at IS NULL
        AND cleanup_attempts = 0 AND cleanup_last_attempt_at IS NULL
        AND released_at IS NULL AND cleanup_error IS NULL AND cleanup_retry_at IS NULL)),
    CHECK (state != 'expired' OR (quarantine_root IS NULL AND quarantined_at IS NULL
        AND cleanup_attempts = 0 AND cleanup_last_attempt_at IS NULL
        AND released_at IS NULL AND cleanup_error IS NULL AND cleanup_retry_at IS NULL)),
    CHECK (state != 'released' OR (released_at IS NOT NULL AND quarantine_root IS NOT NULL
        AND quarantined_at IS NOT NULL AND cleanup_error IS NULL AND cleanup_retry_at IS NULL)),
    CHECK (state = 'released' OR released_at IS NULL),
    CHECK (state != 'cleanup_failed' OR (cleanup_error IS NOT NULL AND cleanup_retry_at IS NOT NULL
        AND cleanup_attempts > 0 AND cleanup_last_attempt_at IS NOT NULL)),
    CHECK (state = 'cleanup_failed' OR (cleanup_error IS NULL AND cleanup_retry_at IS NULL)),
    CHECK (quarantine_root IS NULL OR quarantined_at IS NOT NULL OR state = 'releasing')
);

CREATE UNIQUE INDEX idx_workspaces_unix_identity
    ON workspaces(unix_device, unix_inode)
    WHERE root_identity_kind = 'unix';
CREATE UNIQUE INDEX idx_workspaces_windows_identity
    ON workspaces(windows_volume_serial, windows_file_id)
    WHERE root_identity_kind = 'windows';
CREATE UNIQUE INDEX idx_workspaces_quarantine_root
    ON workspaces(quarantine_root)
    WHERE quarantine_root IS NOT NULL;
CREATE UNIQUE INDEX idx_workspaces_canonical_root
    ON workspaces(canonical_root);
CREATE INDEX idx_workspaces_active_expiry
    ON workspaces(expires_at, id)
    WHERE state = 'active';

CREATE TABLE workspace_leases (
    lease_id           TEXT PRIMARY KEY NOT NULL,
    workspace_id       TEXT NOT NULL,
    root_generation    TEXT NOT NULL CHECK (length(trim(root_generation)) > 0 AND instr(root_generation, char(0)) = 0),
    owner_id           TEXT NOT NULL CHECK (length(trim(owner_id)) > 0 AND instr(owner_id, char(0)) = 0),
    kind               TEXT NOT NULL CHECK (kind IN ('run', 'host')),
    daemon_generation  TEXT,
    host_instance_id   TEXT,
    acquired_at        TEXT NOT NULL,
    expires_at         TEXT,
    renewed_at         TEXT,
    released_at        TEXT,

    FOREIGN KEY (workspace_id, root_generation)
        REFERENCES workspaces (id, root_generation),

    CHECK (length(lease_id) = 29 AND substr(lease_id, 1, 3) = 'wl_' AND instr(lease_id, char(0)) = 0
        AND substr(lease_id, 4, 1) GLOB '[0-7]'
        AND substr(lease_id, 4) NOT GLOB '*[^0-9ABCDEFGHJKMNPQRSTVWXYZ]*'),
    CHECK (length(acquired_at) = 24 AND acquired_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(acquired_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(acquired_at)) = acquired_at),
    CHECK (expires_at IS NULL OR (length(expires_at) = 24 AND expires_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(expires_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(expires_at)) = expires_at)),
    CHECK (renewed_at IS NULL OR (length(renewed_at) = 24 AND renewed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(renewed_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(renewed_at)) = renewed_at)),
    CHECK (released_at IS NULL OR (length(released_at) = 24 AND released_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AND julianday(released_at) IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(released_at)) = released_at)),
    CHECK (
        (kind = 'run' AND daemon_generation IS NULL AND host_instance_id IS NULL
            AND expires_at IS NULL AND renewed_at IS NULL)
        OR (kind = 'host' AND daemon_generation IS NOT NULL
            AND length(trim(daemon_generation)) > 0 AND instr(daemon_generation, char(0)) = 0
            AND host_instance_id IS NOT NULL AND length(trim(host_instance_id)) > 0
            AND owner_id = host_instance_id AND expires_at IS NOT NULL
            AND julianday(expires_at) > julianday(acquired_at)
            AND (CAST(strftime('%s', expires_at) AS INTEGER) * 1000 + CAST(substr(expires_at, 21, 3) AS INTEGER))
                - (CAST(strftime('%s', COALESCE(renewed_at, acquired_at)) AS INTEGER) * 1000
                    + CAST(substr(COALESCE(renewed_at, acquired_at), 21, 3) AS INTEGER)) <= 90000
            AND (renewed_at IS NULL OR (julianday(renewed_at) >= julianday(acquired_at)
                AND julianday(renewed_at) < julianday(expires_at))))
    ),
    CHECK (released_at IS NULL OR julianday(released_at) >= julianday(acquired_at)),
    CHECK (released_at IS NULL OR renewed_at IS NULL OR julianday(released_at) >= julianday(renewed_at))
);

CREATE UNIQUE INDEX idx_workspace_leases_active_run
    ON workspace_leases(workspace_id)
    WHERE kind = 'run' AND released_at IS NULL;
CREATE UNIQUE INDEX idx_workspace_leases_active_run_owner
    ON workspace_leases(owner_id)
    WHERE kind = 'run' AND released_at IS NULL;
CREATE UNIQUE INDEX idx_workspace_leases_active_host
    ON workspace_leases(workspace_id, host_instance_id)
    WHERE kind = 'host' AND released_at IS NULL;
CREATE INDEX idx_workspace_leases_open
    ON workspace_leases(workspace_id, kind)
    WHERE released_at IS NULL;
CREATE INDEX idx_workspace_leases_host_expiry
    ON workspace_leases(expires_at, workspace_id)
    WHERE kind = 'host' AND released_at IS NULL;
