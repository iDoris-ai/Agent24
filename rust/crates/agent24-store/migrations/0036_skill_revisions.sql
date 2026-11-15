-- SL-2 skill tables (skill-loader plan §4.2, §4.5, §4.11).
-- New tables only. This migration does not read or write existing rows.
--
-- skill_revision is append-only. Identity is revision_id. semver is metadata
-- and does not identify the row. The same (skill_id, origin, kind,
-- content_digest) is one revision: a later semver with the same bytes returns
-- that row. The same (skill_id, origin, semver, kind) with a different digest
-- is a conflict and must not replace the row. Body and evals are two rows.
--
-- skill_pointer is the current pair plus a generation. No row means inactive.
-- The primary key is skill_id. The first successful move stores generation 1.
-- Callers pass 0 when no row exists.
--
-- skill_eval_result is one server-side binding. accepted_json lists the failed
-- case ids the caller accepted for this binding. It does not rewrite cases.
-- skill_activation is the history of activate and rollback. seq orders the
-- history. created_at is not the order. Rollback adds a history row that
-- points at an existing binding and does not change it.
--
-- run_skill_pin is the per-run pin from §4.11. providers_json is a JSON object
-- (capability id to component id). This file only creates the table.

CREATE TABLE skill_revision (
    revision_id    TEXT NOT NULL PRIMARY KEY CHECK (revision_id <> ''),
    skill_id       TEXT NOT NULL CHECK (skill_id <> ''),
    origin         TEXT NOT NULL CHECK (origin IN ('user', 'imported')),
    semver         TEXT NOT NULL CHECK (semver <> ''),
    kind           TEXT NOT NULL CHECK (kind IN ('body', 'evals')),
    content_digest TEXT NOT NULL CHECK (content_digest <> ''),
    content        BLOB NOT NULL,
    created_at     TEXT NOT NULL CHECK (created_at <> ''),
    UNIQUE (skill_id, origin, semver, kind)
);

CREATE UNIQUE INDEX idx_skill_revision_content
    ON skill_revision (skill_id, origin, kind, content_digest);

CREATE TABLE skill_pointer (
    skill_id          TEXT NOT NULL PRIMARY KEY CHECK (skill_id <> ''),
    body_revision_id  TEXT NOT NULL CHECK (body_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    evals_revision_id TEXT NOT NULL CHECK (evals_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    generation        INTEGER NOT NULL CHECK (generation >= 1),
    CHECK (body_revision_id <> evals_revision_id)
);

CREATE TABLE skill_eval_result (
    result_id         TEXT NOT NULL PRIMARY KEY CHECK (result_id <> ''),
    body_revision_id  TEXT NOT NULL CHECK (body_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    evals_revision_id TEXT NOT NULL CHECK (evals_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    segment_digest    TEXT NOT NULL CHECK (segment_digest <> ''),
    renderer_version  TEXT NOT NULL CHECK (renderer_version <> ''),
    cases_json        TEXT NOT NULL CHECK (
        cases_json <> ''
        AND json_valid(cases_json)
        AND json_type(cases_json) = 'array'
    ),
    accepted_json     TEXT NOT NULL DEFAULT '[]' CHECK (
        json_valid(accepted_json)
        AND json_type(accepted_json) = 'array'
    ),
    created_at        TEXT NOT NULL CHECK (created_at <> ''),
    CHECK (body_revision_id <> evals_revision_id)
);

CREATE TABLE skill_activation (
    seq               INTEGER PRIMARY KEY AUTOINCREMENT,
    activation_id     TEXT NOT NULL UNIQUE CHECK (activation_id <> ''),
    skill_id          TEXT NOT NULL CHECK (skill_id <> ''),
    body_revision_id  TEXT NOT NULL CHECK (body_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    evals_revision_id TEXT NOT NULL CHECK (evals_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    result_id         TEXT NOT NULL CHECK (result_id <> '')
        REFERENCES skill_eval_result (result_id),
    result_code       TEXT NOT NULL CHECK (result_code IN ('activated', 'rolled_back')),
    created_at        TEXT NOT NULL CHECK (created_at <> ''),
    CHECK (body_revision_id <> evals_revision_id)
);

CREATE INDEX idx_skill_activation_skill
    ON skill_activation (skill_id, seq);

CREATE TABLE run_skill_pin (
    run_id           TEXT NOT NULL PRIMARY KEY CHECK (run_id <> '')
        REFERENCES runs (id),
    skill_id         TEXT NOT NULL CHECK (skill_id <> ''),
    body_revision_id TEXT NOT NULL CHECK (body_revision_id <> '')
        REFERENCES skill_revision (revision_id),
    content_digest   TEXT NOT NULL CHECK (content_digest <> ''),
    providers_json   TEXT NOT NULL CHECK (
        providers_json <> ''
        AND json_valid(providers_json)
        AND json_type(providers_json) = 'object'
    )
);
