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

-- `trim(...) <> ''` not just `<> ''`: an all-whitespace owner ("   ") is unowned
-- memory too, and #114's governance claim is "no unowned memory" (review #115
-- Low). The same tighter check should back-fill 0002_events.sql's scope_owner in
-- a later migration — a shipped migration is immutable, so it is not edited here.
CREATE TABLE mem_artifacts (
    scope_owner   TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    path          TEXT    NOT NULL CHECK (trim(path) <> ''),
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
-- path, version) is DEFENSE IN DEPTH, not the concurrency guard: `cas_write`
-- takes the write lock up front (BEGIN IMMEDIATE), so the read-check-write is
-- serialized and the normal path loses at the version check, never reaching this
-- constraint (0 hits under ~1400 racing writes, review #115 B3). It stays as a
-- backstop against a future path that writes history directly. Its CHECKs mirror
-- the pointer table's so that backstop cannot admit an unowned/empty-path row.
CREATE TABLE mem_artifact_versions (
    scope_owner   TEXT    NOT NULL CHECK (trim(scope_owner) <> ''),
    path          TEXT    NOT NULL CHECK (trim(path) <> ''),
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
