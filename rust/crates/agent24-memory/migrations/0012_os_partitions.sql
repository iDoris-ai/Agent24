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
    key_version   TEXT NOT NULL CHECK (trim(key_version) <> ''),
    -- The logical user, recorded rather than parsed back out of the key.
    logical_user  TEXT NOT NULL CHECK (trim(logical_user) <> ''),
    -- The module's manifest name AT FIRST SIGHT. A rename does not change the
    -- key, so this is the only record of what the key originally meant.
    module_name   TEXT NOT NULL CHECK (trim(module_name) <> ''),
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);

CREATE INDEX mem_os_partitions_user ON mem_os_partitions (logical_user);
