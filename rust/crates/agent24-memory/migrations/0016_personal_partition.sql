ALTER TABLE mem_os_partitions
    ADD COLUMN space_kind TEXT NOT NULL DEFAULT 'module'
    CHECK (space_kind IN ('module', 'personal'));
