-- Chunked uploads (ADR-DOC-02 §5.6): the display name, every chunk
-- received (so a resent chunk can be told apart from a different one), and
-- the rules that keep `received` honest.
--
-- SQLite's GLOB, length() and comparisons stop at an embedded NUL, so the
-- checks below that must see a whole value measure it as a BLOB.

-- A base name only, never a path (ADR-001); the default title at import.
ALTER TABLE uploads ADD COLUMN filename TEXT
    CHECK (filename IS NULL OR (length(filename) BETWEEN 1 AND 255
                                AND filename NOT GLOB '*[/\]*'
                                AND instr(CAST(filename AS BLOB), x'00') = 0));

-- No upload had a writer before this migration. Should a row exist anyway,
-- it has no chunk records, so it can never complete: close it.
UPDATE uploads SET status = 'expired' WHERE status IN ('receiving', 'complete');

CREATE TABLE upload_chunks (
    upload_id       TEXT NOT NULL REFERENCES uploads(id),
    chunk_offset    INTEGER NOT NULL CHECK (chunk_offset >= 0),
    -- 768 KiB: under the kernel proxy's 1 MiB body limit.
    chunk_size      INTEGER NOT NULL CHECK (chunk_size BETWEEN 1 AND 786432),
    -- As text and as bytes: 71 of each leaves no room for a NUL.
    sha256          TEXT NOT NULL CHECK (length(sha256) = 71 AND length(CAST(sha256 AS BLOB)) = 71
                                         AND substr(sha256, 1, 7) = 'sha256:' AND substr(sha256, 8) NOT GLOB '*[^0-9a-f]*'),
    PRIMARY KEY (upload_id, chunk_offset)
) STRICT;

-- 1. An upload starts empty, with a whole id and hash, and is never deleted
--    (so never replaced either: REPLACE deletes first). The 0001 CHECKs
--    already measure id and sha256 as text; equal byte lengths rule out a NUL.
CREATE TRIGGER uploads_start_empty BEFORE INSERT ON uploads
WHEN NEW.received IS NOT 0 OR NEW.status IS NOT 'receiving'
  OR length(CAST(NEW.id AS BLOB)) <> 30 OR length(CAST(NEW.sha256 AS BLOB)) <> 71
BEGIN SELECT RAISE(ABORT, 'an upload starts empty and receiving'); END;
CREATE TRIGGER uploads_are_never_deleted BEFORE DELETE ON uploads
BEGIN SELECT RAISE(ABORT, 'uploads are never deleted'); END;

-- 2. What the client declared never changes, and status only moves forward:
--    receiving → complete | expired, complete → imported | expired.
CREATE TRIGGER uploads_declaration_is_fixed BEFORE UPDATE OF id, total_size, sha256, filename, created_at ON uploads
WHEN NEW.id IS NOT OLD.id OR NEW.total_size IS NOT OLD.total_size OR NEW.sha256 IS NOT OLD.sha256
  OR NEW.filename IS NOT OLD.filename OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'an upload declaration cannot change'); END;
-- NULL is refused too: `UPDATE OR REPLACE` would turn it into the
-- 'receiving' default after this trigger had let it through.
CREATE TRIGGER uploads_status_moves_forward BEFORE UPDATE OF status ON uploads
WHEN NEW.status IS NOT OLD.status
 AND (NEW.status IS NULL
      OR NOT ((OLD.status = 'receiving' AND NEW.status IN ('complete', 'expired'))
           OR (OLD.status = 'complete' AND NEW.status IN ('imported', 'expired'))))
BEGIN SELECT RAISE(ABORT, 'upload status only moves forward'); END;

-- 3. A chunk is appended at the bytes received so far, fits the declared
--    size, and only while the upload is still receiving.
CREATE TRIGGER upload_chunks_append_at_received BEFORE INSERT ON upload_chunks
WHEN NOT EXISTS (
    SELECT 1 FROM uploads
    WHERE id = NEW.upload_id AND status = 'receiving'
      AND received = NEW.chunk_offset AND NEW.chunk_offset + NEW.chunk_size <= total_size)
BEGIN SELECT RAISE(ABORT, 'a chunk must start at the bytes received so far'); END;

-- 4. A recorded chunk never changes; it goes only with an upload that is over.
CREATE TRIGGER upload_chunks_are_immutable BEFORE UPDATE ON upload_chunks
BEGIN SELECT RAISE(ABORT, 'upload chunks are immutable'); END;
CREATE TRIGGER upload_chunks_outlive_a_live_upload BEFORE DELETE ON upload_chunks
WHEN EXISTS (SELECT 1 FROM uploads WHERE id = OLD.upload_id AND status IN ('receiving', 'complete'))
BEGIN SELECT RAISE(ABORT, 'chunks of a live upload cannot be deleted'); END;

-- 5. `received` moves only while receiving, and only to the end of the
--    recorded chunks, so it never claims bytes no chunk row accounts for.
CREATE TRIGGER uploads_received_matches_chunks BEFORE UPDATE OF received ON uploads
WHEN NEW.received IS NOT OLD.received
 AND (OLD.status <> 'receiving'
      OR NEW.received IS NOT (SELECT coalesce(max(chunk_offset + chunk_size), 0)
                              FROM upload_chunks WHERE upload_id = NEW.id))
BEGIN SELECT RAISE(ABORT, 'received must equal the end of the recorded chunks'); END;
