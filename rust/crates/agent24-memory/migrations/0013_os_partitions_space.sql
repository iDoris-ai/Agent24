-- F8: an organisation is a first-class entity, and a partition is owned by an
-- (org, space) PAIR rather than by a user.
--
-- F1 shipped the ownership dimension as (user, module): the physical owner key
-- was derived from the authenticated user and the manifest name, and the catalog
-- recorded exactly those two things. That is the shape of a single-user product,
-- and it is wrong in a way that gets more expensive every day it holds data. In
-- any multi-party deployment the owner of a memory is a CONTAINER — Team Shared,
-- Finance Private, Customer A — and the user is an ACCESSOR of it. F1 collapsed
-- container and accessor into one column.
--
-- This migration separates them while the catalog is new (0012 landed one day
-- earlier and has never been released), which is the only cheap moment there
-- will be.
--
-- WHY THE ORG IS A ROW AND NOT A DERIVED STRING
--
-- The first draft of this migration derived it: `org_id = 'u:' || logical_user`,
-- no table. That reintroduced the exact defect it was written to fix, one level
-- up — an organisation whose identity is a FUNCTION OF A USER is not an org with
-- one member, it is a user wearing an org's name, and the day a second member
-- arrives its id has to change. Since the id is baked into every owner key, that
-- is another re-key of every partition.
--
-- A generated, stable `org_id` costs one table and makes `v2` terminal for this
-- dimension: adding members, renaming the org, or attaching a real registry
-- changes rows, not keys.
--
-- `mem_orgs` is not decoration, which is the bar this repo holds tables to:
-- `mem_org_members` is READ on every startup to answer "which org is this
-- authenticated user acting in", and that answer is an input to the key. A table
-- nothing reads would not be here — which is why there is deliberately no
-- `mem_spaces` alongside it. No path today can create a space that is not a
-- module's own, because nothing can grant access to one; a space that cannot be
-- granted does not exist yet. The space dimension is therefore real and RECORDED
-- (below), but it does not get a registry until something reads it.
CREATE TABLE mem_orgs (
    -- Opaque and STABLE. Nothing may parse it — that is the whole point of it
    -- not being derived from a member's name.
    org_id       TEXT PRIMARY KEY CHECK (trim(org_id, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- For humans. Renaming an org must never touch a key, which is exactly what
    -- keeping the name OUT of `org_id` buys.
    display_name TEXT NOT NULL CHECK (trim(display_name, char(32)||char(9)||char(10)||char(13)) <> ''),
    created_at   TEXT NOT NULL
);

-- Which users act in which org. One row today; the schema is not the reason it
-- is one, which is the point.
--
-- NO ROLE COLUMN. Roles are a policy concept and nothing evaluates policy yet;
-- a `role` column here would be a field every reader had to ignore and every
-- writer had to invent, which is how a schema starts asserting more than the
-- code delivers. Membership is the fact this table can state honestly today.
CREATE TABLE mem_org_members (
    org_id    TEXT NOT NULL REFERENCES mem_orgs(org_id),
    user_id   TEXT NOT NULL CHECK (trim(user_id, char(32)||char(9)||char(10)||char(13)) <> ''),
    joined_at TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);

CREATE INDEX mem_org_members_user ON mem_org_members (user_id);

-- Every user that F1 ever recorded a partition for gets the org they were
-- implicitly already in. The id is legacy-shaped ON PURPOSE: org ids are opaque
-- and resolved by MEMBERSHIP, never parsed, so a row minted by SQL and a row
-- minted by the kernel are interchangeable to every reader. Deriving it here is
-- safe for the same reason deriving it into the KEY was not — nothing downstream
-- depends on its shape, and it never changes again.
INSERT INTO mem_orgs (org_id, display_name, created_at)
SELECT 'org_legacy_' || logical_user, logical_user, MIN(first_seen_at)
FROM mem_os_partitions
GROUP BY logical_user;

INSERT INTO mem_org_members (org_id, user_id, joined_at)
SELECT 'org_legacy_' || logical_user, logical_user, MIN(first_seen_at)
FROM mem_os_partitions
GROUP BY logical_user;

-- WHAT THE PARTITION CATALOG NOW IS
--
-- The mapping from a LOGICAL identity (org_id, space_id) to the PHYSICAL
-- `scope_owner` string that identity is currently stored under, plus the
-- encoding version of that string. Those are three different facts, and 0012
-- kept only enough to reconstruct one of them by parsing — which the F1 review
-- had already ruled out: nothing may discover partitions by prefix-matching keys
-- that contain NUL.
--
-- That separation is what makes a re-key possible at all. `owner_key` MAY change
-- (a key-version migration is precisely that); `org_id` and `space_id` may not,
-- because they are the identity the key merely encodes.
--
-- WHY A REBUILD RATHER THAN `ALTER TABLE ADD COLUMN`
--
-- The new columns are NOT NULL and there is no honest default for them. SQLite
-- requires a DEFAULT to add a NOT NULL column, so `ADD COLUMN` would mean
-- inventing a value — a column that reads as "always known" while holding a
-- placeholder is the failure mode this repo has spent six review rounds on. A
-- rebuild also lets the CHECKs be written properly rather than bolted on.
CREATE TABLE mem_os_partitions_v2 (
    -- The physical `scope_owner` value TODAY. Changes when the partition is
    -- re-keyed; `key_version` says which encoding it is in.
    owner_key     TEXT PRIMARY KEY,
    -- `v1` = F1's (user, module) key; `v2` = F8's (org, space) key. Read, never
    -- inferred from the string's shape.
    key_version   TEXT NOT NULL CHECK (trim(key_version, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- The organisation this partition belongs to.
    org_id        TEXT NOT NULL REFERENCES mem_orgs(org_id),
    -- The space: the container the memories belong to. Today always the module's
    -- own private space `os:<module>`, which reproduces F1's isolation exactly —
    -- one partition per module — while naming the dimension for what it is.
    space_id      TEXT NOT NULL CHECK (trim(space_id, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- The user who CREATED this partition. Write-once, like `module_name`.
    --
    -- NOT the export/erase lookup, which is what 0012 used it for and what an
    -- earlier draft of this comment still said. A partition is owned by
    -- (org_id, space_id), so every member of the org derives the same key and
    -- writes into the same rows; this column answers provenance ("who brought
    -- this into being"), not ownership. An export or erase path reads `org_id`.
    logical_user  TEXT NOT NULL CHECK (trim(logical_user, char(32)||char(9)||char(10)||char(13)) <> ''),
    -- The module's manifest name AT FIRST SIGHT, write-once, kept from 0012 for
    -- the reason recorded there: a rename produces a NEW partition and leaves the
    -- old one behind, and this column is the only thing that can later say what
    -- the abandoned key meant.
    module_name   TEXT NOT NULL CHECK (trim(module_name, char(32)||char(9)||char(10)||char(13)) <> ''),
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);

-- The logical columns are derivable exactly from what a v1 row already states: a
-- v1 partition belonged to user U and module M, so its org is U's org and its
-- space is `os:M`.
--
-- `owner_key` is deliberately NOT rewritten here, and `key_version` stays `v1`.
-- Re-keying has to rewrite `mem_events.scope_owner` for every row of the
-- partition in the same transaction as the catalog row, refuse rather than merge
-- if the target key already exists, and compute a LENGTH-PREFIXED key whose
-- lengths are BYTE counts. SQLite's `length()` counts CHARACTERS, so any
-- non-ASCII byte in a user id would produce a key that silently disagrees with
-- the one the kernel derives, for those users only. `octet_length()` exists but
-- only from SQLite 3.43, a runtime version this crate does not pin. So the
-- re-key lives in Rust (`KvStore::rekey_os_partition`), driven by these rows;
-- this migration's job is only to make them able to say what they are.
INSERT INTO mem_os_partitions_v2
    (owner_key, key_version, org_id, space_id,
     logical_user, module_name, first_seen_at, last_seen_at)
SELECT owner_key,
       key_version,
       'org_legacy_' || logical_user,
       'os:' || module_name,
       logical_user,
       module_name,
       first_seen_at,
       last_seen_at
FROM mem_os_partitions;

DROP TABLE mem_os_partitions;
ALTER TABLE mem_os_partitions_v2 RENAME TO mem_os_partitions;

-- The provenance lookup, unchanged from 0012 in shape but no longer in meaning
-- — see `logical_user` above.
CREATE INDEX mem_os_partitions_user ON mem_os_partitions (logical_user);
-- No separate index for the export/erase lookup (`WHERE org_id = ?`): the
-- UNIQUE index below is on (org_id, space_id), and org_id is its leftmost
-- prefix, so SQLite already serves that query from it. A second index would be
-- write cost for a read that is already covered.
-- The lookup this table now exists for: "which physical key is (org, space)
-- stored under". UNIQUE because one logical space has exactly one partition —
-- without it, a re-key that failed halfway and left both rows behind would look
-- like a space with two partitions, and every reader would have to pick one.
CREATE UNIQUE INDEX mem_os_partitions_space ON mem_os_partitions (org_id, space_id);
