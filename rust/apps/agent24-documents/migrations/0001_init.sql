-- documents.db, slice 1 (ADR-DOC-02 §2–§7). Timestamps are RFC 3339 UTC strings.
-- STRICT tables: an INTEGER column only ever holds integers. Ids are checked
-- with GLOB, which is case-sensitive: `doc_` + ULID (first char 0–7, §3).
-- Content addresses are `sha256:` + 64 lowercase hex. Presence checks use
-- coalesce(length(x), 0) > 0: a CHECK that evaluates to NULL would pass.
-- Db::open sets recursive_triggers=ON, so INSERT OR REPLACE / UPSERT also
-- fire the delete triggers that keep revisions, text layers and the oplog fixed.

-- One row per document. head_revision is the single editable authority (§2.1).
CREATE TABLE documents (
    id              TEXT NOT NULL PRIMARY KEY CHECK (id GLOB 'doc_[0-7][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z]'),
    title           TEXT NOT NULL,
    media_type      TEXT NOT NULL,
    head_revision   INTEGER NOT NULL CHECK (head_revision >= 1),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Immutable revisions; r1 is the imported original (§3).
CREATE TABLE revisions (
    document_id     TEXT NOT NULL REFERENCES documents(id),
    revision        INTEGER NOT NULL CHECK (revision >= 1),
    content_sha256  TEXT NOT NULL CHECK (substr(content_sha256, 1, 7) = 'sha256:' AND length(content_sha256) = 71 AND substr(content_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    size            INTEGER NOT NULL CHECK (size >= 0),
    media_type      TEXT NOT NULL,
    origin          TEXT NOT NULL CHECK (origin IN ('import', 'commit')),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (document_id, revision)
) STRICT;
CREATE TRIGGER revisions_are_immutable BEFORE UPDATE ON revisions
BEGIN SELECT RAISE(ABORT, 'revisions are immutable'); END;
CREATE TRIGGER revisions_are_never_deleted BEFORE DELETE ON revisions
BEGIN SELECT RAISE(ABORT, 'revisions are never deleted in DOC-1'); END;

-- Text layer parsed from one revision's bytes by one engine build (§3).
-- Pinned: never rewritten on engine upgrade, never changed or deleted.
CREATE TABLE text_layers (
    text_layer_sha256 TEXT NOT NULL PRIMARY KEY CHECK (substr(text_layer_sha256, 1, 7) = 'sha256:' AND length(text_layer_sha256) = 71 AND substr(text_layer_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    content_sha256    TEXT NOT NULL CHECK (substr(content_sha256, 1, 7) = 'sha256:' AND length(content_sha256) = 71 AND substr(content_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    engine_id         TEXT NOT NULL CHECK (length(engine_id) > 0),
    engine_version    TEXT NOT NULL CHECK (length(engine_version) > 0),
    config_sha256     TEXT NOT NULL CHECK (substr(config_sha256, 1, 7) = 'sha256:' AND length(config_sha256) = 71 AND substr(config_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (content_sha256, engine_id, engine_version, config_sha256)
) STRICT;
CREATE TRIGGER text_layers_are_immutable BEFORE UPDATE ON text_layers
BEGIN SELECT RAISE(ABORT, 'text layers are immutable'); END;
CREATE TRIGGER text_layers_are_never_deleted BEFORE DELETE ON text_layers
BEGIN SELECT RAISE(ABORT, 'text layers are pinned'); END;

-- Chunked uploads (§5.6). Bytes live in uploads/<id>/, outside tmp/ and blob GC.
CREATE TABLE uploads (
    id              TEXT NOT NULL PRIMARY KEY CHECK (id GLOB 'upl_[0-7][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z]'),
    total_size      INTEGER NOT NULL CHECK (total_size >= 0),
    sha256          TEXT NOT NULL CHECK (substr(sha256, 1, 7) = 'sha256:' AND length(sha256) = 71 AND substr(sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    received        INTEGER NOT NULL DEFAULT 0 CHECK (received >= 0 AND received <= total_size),
    status          TEXT NOT NULL DEFAULT 'receiving'
                    CHECK (status IN ('receiving', 'complete', 'imported', 'expired')),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_chunk_at   TEXT
) STRICT;

-- Jobs (§7). The row, not any event, is the authority.
-- origin: {"kind":"page"} or {"kind":"run","run_id":…,"tool_call_id":…}.
CREATE TABLE jobs (
    id              TEXT NOT NULL PRIMARY KEY CHECK (id GLOB 'job_[0-7][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z]'),
    kind            TEXT NOT NULL CHECK (length(kind) > 0),
    document_id     TEXT REFERENCES documents(id),
    revision        INTEGER CHECK (revision IS NULL OR revision >= 1),
    status          TEXT NOT NULL CHECK (status IN
                    ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    attempt         INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
    progress        TEXT CHECK (progress IS NULL OR json_valid(progress)),
    error           TEXT CHECK (error IS NULL OR json_valid(error)),
    result_ref      TEXT,
    origin          TEXT NOT NULL CHECK (json_valid(origin) AND (
                        (json_extract(origin, '$.kind') = 'page'
                         AND json_extract(origin, '$.run_id') IS NULL
                         AND json_extract(origin, '$.tool_call_id') IS NULL)
                     OR (json_extract(origin, '$.kind') = 'run'
                         AND coalesce(length(json_extract(origin, '$.run_id')), 0) > 0
                         AND coalesce(length(json_extract(origin, '$.tool_call_id')), 0) > 0))),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX jobs_by_status ON jobs(status);

-- Business-key idempotency (§5.4). request_sha256 covers business parameters only.
CREATE TABLE idempotency (
    kind            TEXT NOT NULL CHECK (kind IN ('upload', 'import', 'extract', 'propose', 'export', 'commit')),
    key             TEXT NOT NULL CHECK (length(key) > 0),
    request_sha256  TEXT NOT NULL CHECK (substr(request_sha256, 1, 7) = 'sha256:' AND length(request_sha256) = 71 AND substr(request_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    target_ref      TEXT NOT NULL CHECK (length(target_ref) > 0),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (kind, key)
) STRICT;

-- Append-only operation log (ADR-DOC-01 D9): who did what, from the page or a run.
CREATE TABLE oplog (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    op              TEXT NOT NULL CHECK (length(op) > 0),
    document_id     TEXT,
    revision        INTEGER CHECK (revision IS NULL OR revision >= 1),
    origin          TEXT NOT NULL CHECK (origin IN ('page', 'run')),
    run_id          TEXT,
    tool_call_id    TEXT,
    details         TEXT CHECK (details IS NULL OR json_valid(details)),
    CHECK ((origin = 'page' AND run_id IS NULL AND tool_call_id IS NULL)
        OR (origin = 'run' AND coalesce(length(run_id), 0) > 0 AND coalesce(length(tool_call_id), 0) > 0))
) STRICT;
CREATE TRIGGER oplog_is_append_only_update BEFORE UPDATE ON oplog
BEGIN SELECT RAISE(ABORT, 'oplog is append-only'); END;
CREATE TRIGGER oplog_is_append_only_delete BEFORE DELETE ON oplog
BEGIN SELECT RAISE(ABORT, 'oplog is append-only'); END;
