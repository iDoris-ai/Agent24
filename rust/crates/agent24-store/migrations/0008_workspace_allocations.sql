-- Durable allocation journal for G1. Workspace registration remains a later step.
CREATE TABLE workspace_allocations (
    allocation_id          TEXT PRIMARY KEY NOT NULL
        CHECK (length(allocation_id) = 29 AND substr(allocation_id, 1, 3) = 'wa_'
            AND substr(allocation_id, 4, 1) GLOB '[0-7]'
            AND substr(allocation_id, 4) NOT GLOB '*[^0-9ABCDEFGHJKMNPQRSTVWXYZ]*'
            AND instr(allocation_id, char(0)) = 0),
    workspace_id            TEXT NOT NULL UNIQUE
        CHECK (length(workspace_id) = 29 AND substr(workspace_id, 1, 3) = 'ws_'
            AND substr(workspace_id, 4, 1) GLOB '[0-7]'
            AND substr(workspace_id, 4) NOT GLOB '*[^0-9ABCDEFGHJKMNPQRSTVWXYZ]*'
            AND instr(workspace_id, char(0)) = 0),
    root_generation          TEXT NOT NULL
        CHECK (length(trim(root_generation)) > 0 AND instr(root_generation, char(0)) = 0),
    relative_name           TEXT NOT NULL UNIQUE
        CHECK (length(relative_name) BETWEEN 1 AND 255 AND relative_name NOT IN ('.', '..')
            AND instr(relative_name, char(0)) = 0 AND instr(relative_name, '/') = 0
            AND instr(relative_name, char(92)) = 0),
    parent_identity_kind    TEXT NOT NULL CHECK (parent_identity_kind IN ('unix', 'windows')),
    parent_unix_device      BLOB,
    parent_unix_inode       BLOB,
    parent_windows_volume   BLOB,
    parent_windows_file_id  BLOB,
    root_identity_kind      TEXT CHECK (root_identity_kind IS NULL OR root_identity_kind IN ('unix', 'windows')),
    root_unix_device        BLOB,
    root_unix_inode         BLOB,
    root_windows_volume     BLOB,
    root_windows_file_id    BLOB,
    phase                   TEXT NOT NULL CHECK (phase IN ('reserved', 'materialized', 'committed', 'retained')),
    created_at              TEXT NOT NULL
        CHECK (length(created_at) = 24
            AND created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
            AND julianday(created_at) IS NOT NULL
            AND strftime('%Y-%m-%dT%H:%M:%fZ', julianday(created_at)) = created_at),
    failure_reason          TEXT CHECK (failure_reason IS NULL OR
        (length(failure_reason) BETWEEN 1 AND 128 AND instr(failure_reason, char(0)) = 0
            AND failure_reason NOT GLOB '*[^a-z0-9_-]*')),

    CHECK (
        (parent_identity_kind = 'unix'
            AND typeof(parent_unix_device) = 'blob' AND length(parent_unix_device) = 8
            AND typeof(parent_unix_inode) = 'blob' AND length(parent_unix_inode) = 8
            AND parent_windows_volume IS NULL AND parent_windows_file_id IS NULL)
        OR (parent_identity_kind = 'windows'
            AND parent_unix_device IS NULL AND parent_unix_inode IS NULL
            AND typeof(parent_windows_volume) = 'blob' AND length(parent_windows_volume) = 8
            AND typeof(parent_windows_file_id) = 'blob' AND length(parent_windows_file_id) = 16)
    ),
    CHECK (
        (root_identity_kind IS NULL AND root_unix_device IS NULL AND root_unix_inode IS NULL
            AND root_windows_volume IS NULL AND root_windows_file_id IS NULL)
        OR (root_identity_kind = 'unix'
            AND typeof(root_unix_device) = 'blob' AND length(root_unix_device) = 8
            AND typeof(root_unix_inode) = 'blob' AND length(root_unix_inode) = 8
            AND root_windows_volume IS NULL AND root_windows_file_id IS NULL)
        OR (root_identity_kind = 'windows'
            AND root_unix_device IS NULL AND root_unix_inode IS NULL
            AND typeof(root_windows_volume) = 'blob' AND length(root_windows_volume) = 8
            AND typeof(root_windows_file_id) = 'blob' AND length(root_windows_file_id) = 16)
    ),
    CHECK (
        (phase = 'reserved' AND root_identity_kind IS NULL AND failure_reason IS NULL)
        OR (phase IN ('materialized', 'committed') AND root_identity_kind IS NOT NULL AND failure_reason IS NULL)
        OR (phase = 'retained' AND failure_reason IS NOT NULL)
    )
);
