-- MD-2b: ArtifactStore — the CAS-versioned markdown/core authority.
--
-- An artifact is user- or agent-authored content (a markdown note, a persona
-- core) that is EDITABLE, unlike an immutable event. It is owner-scoped: two
-- owners may hold the same `path` independently, so the identity is
-- (scope_owner, path), NOT the SPEC's simplified "path PK" — a bare-path PK
-- would collide across tenants and leak one owner's core into another's read
-- (the zero-scope-leak acceptance).
--
-- Dual lineage (basic-memory): `db_checksum` is the hash of the body the DB
-- authoritatively holds; `file_checksum` is the hash the DB last observed on
-- disk. MD-2b keeps them equal at write time (a DB write is assumed flushed);
-- MD-2c's reconciliation is what makes them diverge when a file is edited
-- outside the store, and it must never silently delete on divergence.

CREATE TABLE mem_artifacts (
    scope_owner   TEXT    NOT NULL CHECK (scope_owner <> ''),
    path          TEXT    NOT NULL CHECK (path <> ''),
    version       INTEGER NOT NULL,
    body          TEXT    NOT NULL,
    db_checksum   TEXT    NOT NULL,
    file_checksum TEXT    NOT NULL,
    scope         TEXT    NOT NULL, -- full Scope JSON (owner + optional narrowing)
    updated_by    TEXT    NOT NULL,
    reason        TEXT    NOT NULL,
    at            TEXT    NOT NULL,
    PRIMARY KEY (scope_owner, path)
);

-- Every version ever committed (the CAS history). The UNIQUE (scope_owner,
-- path, version) is the real optimistic-concurrency guard: two racing writers
-- that both read version N and try to commit N+1 — one lands, the other hits
-- this constraint and is rejected as a conflict.
CREATE TABLE mem_artifact_versions (
    scope_owner   TEXT    NOT NULL,
    path          TEXT    NOT NULL,
    version       INTEGER NOT NULL,
    body          TEXT    NOT NULL,
    db_checksum   TEXT    NOT NULL,
    file_checksum TEXT    NOT NULL,
    scope         TEXT    NOT NULL,
    updated_by    TEXT    NOT NULL,
    reason        TEXT    NOT NULL,
    at            TEXT    NOT NULL,
    PRIMARY KEY (scope_owner, path, version)
);
