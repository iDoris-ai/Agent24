-- F1: a DURABLE record of which physical owner key belongs to which (user,
-- module).
--
-- Design C keys a domain OS's memory as a kernel-derived owner string. That
-- buys zero migration, and it costs semantic debt: after a module is renamed,
-- the database alone cannot say whether a partition belongs to the old module,
-- the new one, both merged, or something uninstalled. The agreed mitigation was
-- a kernel-owned catalog — so that a future export, erase or key-version
-- migration has an explicit list instead of prefix-matching strings that
-- contain NUL.
--
-- The first version of that catalog was an in-memory Vec rebuilt from whichever
-- modules happened to mount, then dropped. Adversarial review pointed out the
-- obvious: it knew nothing about previous runs, disabled modules, renamed
-- modules or older key versions — the exact cases it existed for. A catalog
-- that is not durable is a diagnostic, not a catalog.
--
-- `first_seen_at` is never updated: the point is when this partition came into
-- existence. `last_seen_at` moves, so an operator can tell a live partition
-- from one belonging to a module that is no longer installed — which is
-- precisely what a rename leaves behind, and precisely what must NOT be
-- silently deleted.
CREATE TABLE mem_os_partitions (
    -- The physical `scope_owner` value. One row per partition, ever.
    owner_key     TEXT PRIMARY KEY,
    -- The derived-key format, so a later migration knows what it is reading
    -- rather than inferring it from the string's shape.
    key_version   TEXT NOT NULL CHECK (trim(key_version, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- The logical user, recorded rather than parsed back out of the key.
    logical_user  TEXT NOT NULL CHECK (trim(logical_user, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- The module's manifest name AT FIRST SIGHT, and write-once.
    --
    -- A rename does NOT rewrite this row: the key is derived from the name, so a
    -- renamed module produces a NEW partition and leaves the old one behind, data
    -- and all, under the old name. That is exactly the debt design C accepted, and
    -- this column is the only thing that can later say what the abandoned key
    -- meant — the key itself cannot be asked, and the running kernel no longer
    -- knows the old name.
    module_name   TEXT NOT NULL CHECK (trim(module_name, char(32)||char(9)||char(10)||char(13)) <> ''),
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);

-- The two-argument trim, matching migration 0011's event-owner rule. The
-- one-argument form strips ASCII SPACE only, so a tab-only module name would
-- pass a CHECK that reads as "not blank" — the exact gap 0011 was written to
-- close, and one this table would have reintroduced.
CREATE INDEX mem_os_partitions_user ON mem_os_partitions (logical_user);
