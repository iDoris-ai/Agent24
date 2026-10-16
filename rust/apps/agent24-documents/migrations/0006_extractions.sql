-- Extractions (ADR-DOC-02 §3, §3.2): one per succeeded extract job, written
-- once with all its values in one transaction, then never changed or
-- deleted. Each names the text layer its anchors point into, which pins it.
-- Text is checked whole: length() stops at a NUL, so NUL is refused outright
-- and byte lengths are taken on the BLOB (as 0004 and 0005 do).
CREATE TABLE extractions (
    id                TEXT NOT NULL PRIMARY KEY CHECK (id GLOB 'ext_[0-7][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z][0-9A-HJKMNP-TV-Z]' AND instr(CAST(id AS BLOB), x'00') = 0),
    document_id       TEXT NOT NULL,
    revision          INTEGER NOT NULL CHECK (revision >= 1),
    schema_sha256     TEXT NOT NULL CHECK (instr(CAST(schema_sha256 AS BLOB), x'00') = 0 AND substr(schema_sha256, 1, 7) = 'sha256:' AND length(schema_sha256) = 71 AND substr(schema_sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    extractor_version TEXT NOT NULL CHECK (instr(CAST(extractor_version AS BLOB), x'00') = 0
                                           AND length(CAST(extractor_version AS BLOB)) BETWEEN 1 AND 64),
    -- The model actually used (§5.4), as the kernel named it: at most 256 bytes.
    model_id          TEXT NOT NULL CHECK (instr(CAST(model_id AS BLOB), x'00') = 0
                                           AND length(CAST(model_id AS BLOB)) BETWEEN 1 AND 256),
    text_layer_sha256 TEXT NOT NULL REFERENCES text_layers(text_layer_sha256),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    FOREIGN KEY (document_id, revision) REFERENCES revisions(document_id, revision)
) STRICT;

-- An extraction's values in the order of the requested fields, each one
-- DocumentsExtractedValue as JSON (at most 100: a request has at most 100
-- fields). Raw NUL is refused, as for job input (0005).
CREATE TABLE extraction_values (
    extraction_id TEXT NOT NULL REFERENCES extractions(id),
    ord           INTEGER NOT NULL CHECK (ord BETWEEN 0 AND 99),
    value         TEXT NOT NULL CHECK (instr(CAST(value AS BLOB), x'00') = 0
                                       AND json_valid(value) AND json_type(value) = 'object'),
    PRIMARY KEY (extraction_id, ord)
) STRICT;

-- The text layer is one of this revision's bytes: anchors into another
-- revision's layer would claim content they were not taken from (§3).
CREATE TRIGGER extractions_read_their_own_revision BEFORE INSERT ON extractions
WHEN (SELECT content_sha256 FROM text_layers WHERE text_layer_sha256 = NEW.text_layer_sha256)
  IS NOT (SELECT content_sha256 FROM revisions WHERE document_id = NEW.document_id AND revision = NEW.revision)
BEGIN SELECT RAISE(ABORT, 'an extraction reads a text layer of its own revision'); END;

-- Values are written with their extraction, before its job succeeds; after
-- that the set is sealed, so a replay or a page never changes.
CREATE TRIGGER extraction_values_are_sealed BEFORE INSERT ON extraction_values
WHEN EXISTS (SELECT 1 FROM jobs WHERE kind = 'extract' AND status = 'succeeded'
                                  AND result_ref = NEW.extraction_id)
BEGIN SELECT RAISE(ABORT, 'the values of a published extraction are sealed'); END;

CREATE TRIGGER extractions_are_immutable BEFORE UPDATE ON extractions
BEGIN SELECT RAISE(ABORT, 'extractions are immutable'); END;
CREATE TRIGGER extractions_are_never_deleted BEFORE DELETE ON extractions
BEGIN SELECT RAISE(ABORT, 'extractions are never deleted'); END;
CREATE TRIGGER extraction_values_are_immutable BEFORE UPDATE ON extraction_values
BEGIN SELECT RAISE(ABORT, 'extraction values are immutable'); END;
CREATE TRIGGER extraction_values_are_never_deleted BEFORE DELETE ON extraction_values
BEGIN SELECT RAISE(ABORT, 'extraction values are never deleted'); END;

-- REPLACE is refused before anything is deleted, with or without
-- recursive_triggers (as 0005 does for jobs): the old row is still there
-- when BEFORE INSERT runs, whether the conflict is on the key or the rowid.
CREATE TRIGGER extractions_are_never_replaced BEFORE INSERT ON extractions
WHEN EXISTS (SELECT 1 FROM extractions WHERE id = NEW.id OR rowid = NEW.rowid)
BEGIN SELECT RAISE(ABORT, 'an extraction is never replaced'); END;
CREATE TRIGGER extraction_values_are_never_replaced BEFORE INSERT ON extraction_values
WHEN EXISTS (SELECT 1 FROM extraction_values
             WHERE (extraction_id = NEW.extraction_id AND ord = NEW.ord) OR rowid = NEW.rowid)
BEGIN SELECT RAISE(ABORT, 'an extraction value is never replaced'); END;
CREATE TRIGGER extractions_rowid_is_positive AFTER INSERT ON extractions WHEN NEW.rowid <= 0
BEGIN SELECT RAISE(ABORT, 'rowids are positive'); END;
CREATE TRIGGER extraction_values_rowid_is_positive AFTER INSERT ON extraction_values WHEN NEW.rowid <= 0
BEGIN SELECT RAISE(ABORT, 'rowids are positive'); END;

-- A succeeded extract job names an extraction that exists, for its own
-- document and revision (§7): GET /jobs never points at nothing. Every
-- UPDATE is checked, not only of status and result_ref: moving a succeeded
-- job to another document or revision would leave it naming the wrong one.
CREATE TRIGGER extract_jobs_succeed_with_their_extraction BEFORE UPDATE ON jobs
WHEN NEW.kind = 'extract' AND NEW.status = 'succeeded' AND NOT EXISTS (
    SELECT 1 FROM extractions
    WHERE id = NEW.result_ref AND document_id = NEW.document_id AND revision = NEW.revision)
BEGIN SELECT RAISE(ABORT, 'a succeeded extract job names its extraction'); END;
CREATE TRIGGER extract_jobs_are_born_with_their_extraction BEFORE INSERT ON jobs
WHEN NEW.kind = 'extract' AND NEW.status = 'succeeded' AND NOT EXISTS (
    SELECT 1 FROM extractions
    WHERE id = NEW.result_ref AND document_id = NEW.document_id AND revision = NEW.revision)
BEGIN SELECT RAISE(ABORT, 'a succeeded extract job names its extraction'); END;

-- A succeeded extract job is final (§5.4: extracting again is a new job):
-- its status, result and target never change, so the seal on its
-- extraction's values can never be lifted by moving the job away and back.
CREATE TRIGGER extract_jobs_are_final_once_succeeded BEFORE UPDATE ON jobs
WHEN OLD.kind = 'extract' AND OLD.status = 'succeeded'
 AND (NEW.status IS NOT OLD.status OR NEW.result_ref IS NOT OLD.result_ref
      OR NEW.document_id IS NOT OLD.document_id OR NEW.revision IS NOT OLD.revision)
BEGIN SELECT RAISE(ABORT, 'a succeeded extract job is final'); END;
