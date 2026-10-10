-- Integrity gaps found by PR-Daemon on #807 (each reproduced with sqlite3).
-- 0001 is checksummed by sqlx and never edited; SQLite cannot add a CHECK or
-- FOREIGN KEY to an existing table without rebuilding it, so these are triggers.

-- 1. head_revision must name an existing revision of the same document.
--    On INSERT the document row comes first and r1 follows in the same
--    transaction (import), so INSERT is the import handler's invariant and is
--    tested there; every later move of the head is checked here.
CREATE TRIGGER documents_head_revision_exists BEFORE UPDATE OF head_revision ON documents
WHEN NOT EXISTS (SELECT 1 FROM revisions WHERE document_id = NEW.id AND revision = NEW.head_revision)
BEGIN SELECT RAISE(ABORT, 'head_revision must name an existing revision'); END;

-- 2. A run origin carries run_id and tool_call_id as strings.
CREATE TRIGGER jobs_run_origin_ids_are_text_insert BEFORE INSERT ON jobs
WHEN json_extract(NEW.origin, '$.kind') = 'run'
 AND (json_type(NEW.origin, '$.run_id') IS NOT 'text' OR json_type(NEW.origin, '$.tool_call_id') IS NOT 'text')
BEGIN SELECT RAISE(ABORT, 'run origin ids must be strings'); END;
CREATE TRIGGER jobs_run_origin_ids_are_text_update BEFORE UPDATE OF origin ON jobs
WHEN json_extract(NEW.origin, '$.kind') = 'run'
 AND (json_type(NEW.origin, '$.run_id') IS NOT 'text' OR json_type(NEW.origin, '$.tool_call_id') IS NOT 'text')
BEGIN SELECT RAISE(ABORT, 'run origin ids must be strings'); END;

-- 3. A job's revision, when set, is an existing revision of its document
--    (which therefore must be set too).
CREATE TRIGGER jobs_revision_exists_insert BEFORE INSERT ON jobs
WHEN NEW.revision IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM revisions WHERE document_id = NEW.document_id AND revision = NEW.revision)
BEGIN SELECT RAISE(ABORT, 'job revision must name an existing revision of its document'); END;
CREATE TRIGGER jobs_revision_exists_update BEFORE UPDATE OF document_id, revision ON jobs
WHEN NEW.revision IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM revisions WHERE document_id = NEW.document_id AND revision = NEW.revision)
BEGIN SELECT RAISE(ABORT, 'job revision must name an existing revision of its document'); END;

-- 4. An upload is complete (or imported) only when every byte has arrived.
CREATE TRIGGER uploads_complete_has_all_bytes_insert BEFORE INSERT ON uploads
WHEN NEW.status IN ('complete', 'imported') AND NEW.received <> NEW.total_size
BEGIN SELECT RAISE(ABORT, 'an upload is complete only when received = total_size'); END;
CREATE TRIGGER uploads_complete_has_all_bytes_update BEFORE UPDATE OF status, received, total_size ON uploads
WHEN NEW.status IN ('complete', 'imported') AND NEW.received <> NEW.total_size
BEGIN SELECT RAISE(ABORT, 'an upload is complete only when received = total_size'); END;
