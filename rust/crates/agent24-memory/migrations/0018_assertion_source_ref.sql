-- K1-6b: persist the source reference atomically with assertion content.
-- Existing assertions have no trustworthy provenance and remain NULL/Unknown.
ALTER TABLE mem_assertions ADD COLUMN source_ref TEXT;
