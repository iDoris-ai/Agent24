-- Follow-up from PR-Daemon on #810 (found by R2, strengthened by Codex R3).
-- `INSERT OR REPLACE INTO documents` runs as delete + insert, never an UPDATE,
-- so 0002's `documents_head_revision_exists` (BEFORE UPDATE) does not see it.

-- 1. A document cannot be deleted in DOC-1 (ADR-DOC-02 §2.4, Q2). With
--    recursive_triggers=ON (Db::open) this also stops INSERT OR REPLACE from
--    swapping a document row out from under its revisions.
CREATE TRIGGER documents_are_never_deleted BEFORE DELETE ON documents
BEGIN SELECT RAISE(ABORT, 'documents are never deleted in DOC-1'); END;

-- 2. Any insert of a document that already exists, or that already has
--    revisions, must name an existing revision as its head. A brand-new
--    document (no row, no revisions yet) passes; import writes r1 in the same
--    transaction. Checking "row already exists" as well as "revisions exist"
--    closes the window where a REPLACE runs before r1 is written.
CREATE TRIGGER documents_head_revision_exists_insert BEFORE INSERT ON documents
WHEN (EXISTS (SELECT 1 FROM documents WHERE id = NEW.id)
      OR EXISTS (SELECT 1 FROM revisions WHERE document_id = NEW.id))
 AND NOT EXISTS (SELECT 1 FROM revisions WHERE document_id = NEW.id AND revision = NEW.head_revision)
BEGIN SELECT RAISE(ABORT, 'head_revision must name an existing revision'); END;
