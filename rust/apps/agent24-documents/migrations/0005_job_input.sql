-- What a job works on, so a re-queued or retried job can run again
-- (ADR-DOC-02 §7): for an import, {"upload_id": …, "title": …}. A JSON
-- object; NULL for rows from before this migration. Raw NUL is never valid
-- JSON text, and SQLite may stop reading at one, so it is refused outright.
ALTER TABLE jobs ADD COLUMN input TEXT
    CHECK (input IS NULL OR (instr(CAST(input AS BLOB), x'00') = 0
                             AND json_valid(input) AND json_type(input) = 'object'));

-- Once set, the input is fixed: a retry runs the same job on the same target.
CREATE TRIGGER jobs_input_is_fixed BEFORE UPDATE OF input ON jobs
WHEN OLD.input IS NOT NULL AND NEW.input IS NOT OLD.input
BEGIN SELECT RAISE(ABORT, 'a job input cannot change once set'); END;
-- REPLACE deletes the old row first; with jobs never deleted, it cannot swap
-- in a row with another input either.
CREATE TRIGGER jobs_are_never_deleted BEFORE DELETE ON jobs
BEGIN SELECT RAISE(ABORT, 'jobs are never deleted'); END;
-- Nor can a job be renamed or recast, and a new row take its old id.
CREATE TRIGGER jobs_identity_is_fixed BEFORE UPDATE OF id, kind, origin, created_at ON jobs
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.origin IS NOT OLD.origin OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'a job id, kind, origin and creation time cannot change'); END;

-- No Documenting code ever wrote a non-positive rowid; a database holding
-- one was edited by something else, and the guards below would misread it.
-- Refuse to migrate it rather than guess (a CHECK on a scratch table is how
-- plain SQL fails a migration; chunks cannot be renumbered, 0004 forbids it).
CREATE TEMP TABLE migration_0005_rowid_check (n INTEGER NOT NULL CHECK (n = 0)) STRICT;
INSERT INTO migration_0005_rowid_check
SELECT (SELECT count(*) FROM jobs WHERE rowid <= 0)
     + (SELECT count(*) FROM uploads WHERE rowid <= 0)
     + (SELECT count(*) FROM upload_chunks WHERE rowid <= 0);
DROP TABLE migration_0005_rowid_check;

-- Rowids are positive. SQLite only ever assigns positive ones; refusing an
-- explicit non-positive one keeps the rowid checks below unambiguous, since
-- in BEFORE INSERT an automatically assigned NEW.rowid reads as -1.
CREATE TRIGGER jobs_rowid_is_positive AFTER INSERT ON jobs WHEN NEW.rowid <= 0
BEGIN SELECT RAISE(ABORT, 'rowids are positive'); END;
CREATE TRIGGER uploads_rowid_is_positive AFTER INSERT ON uploads WHEN NEW.rowid <= 0
BEGIN SELECT RAISE(ABORT, 'rowids are positive'); END;
CREATE TRIGGER upload_chunks_rowid_is_positive AFTER INSERT ON upload_chunks WHEN NEW.rowid <= 0
BEGIN SELECT RAISE(ABORT, 'rowids are positive'); END;

-- The delete guards above (and 0004's for uploads and chunks) stop REPLACE
-- only while recursive_triggers is ON: without it SQLite deletes the old row
-- without firing them. These guards hold either way, as 0003 does for
-- documents (#820 review): when BEFORE INSERT runs the old row is still
-- there, whether the conflict is on the business key or on the hidden rowid,
-- and a row's rowid never changes.
CREATE TRIGGER jobs_id_is_never_reused BEFORE INSERT ON jobs
WHEN EXISTS (SELECT 1 FROM jobs WHERE id = NEW.id OR rowid = NEW.rowid)
BEGIN SELECT RAISE(ABORT, 'a job id is never reused'); END;
CREATE TRIGGER jobs_rowid_is_fixed BEFORE UPDATE ON jobs
WHEN NEW.rowid IS NOT OLD.rowid
BEGIN SELECT RAISE(ABORT, 'a job rowid cannot change'); END;
CREATE TRIGGER uploads_id_is_never_reused BEFORE INSERT ON uploads
WHEN EXISTS (SELECT 1 FROM uploads WHERE id = NEW.id OR rowid = NEW.rowid)
BEGIN SELECT RAISE(ABORT, 'an upload id is never reused'); END;
CREATE TRIGGER uploads_rowid_is_fixed BEFORE UPDATE ON uploads
WHEN NEW.rowid IS NOT OLD.rowid
BEGIN SELECT RAISE(ABORT, 'an upload rowid cannot change'); END;
-- Chunks already refuse every UPDATE (0004).
CREATE TRIGGER upload_chunks_are_never_replaced BEFORE INSERT ON upload_chunks
WHEN EXISTS (SELECT 1 FROM upload_chunks
             WHERE (upload_id = NEW.upload_id AND chunk_offset = NEW.chunk_offset)
                OR rowid = NEW.rowid)
BEGIN SELECT RAISE(ABORT, 'a recorded chunk is never replaced'); END;
