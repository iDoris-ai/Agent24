-- L6-F2 (David 2026-11-22, ADR-DOC-02 Q2 revised): soft delete. A deleted
-- document keeps its row, its revisions and their blobs; `deleted_at` (RFC 3339
-- UTC, as every timestamp here) hides it from lists, reads and tools. It can
-- be restored for 7 days. Rows are still never deleted (0003).
ALTER TABLE documents ADD COLUMN deleted_at TEXT;
