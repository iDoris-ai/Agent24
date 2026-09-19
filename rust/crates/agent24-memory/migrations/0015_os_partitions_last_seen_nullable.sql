-- T8.5c-W-mount 决策 5: `last_seen_at` 从这里起可以是 NULL，含义是"这个身份
-- 被 ensure_recorded 记录过，但从未被 touch_os_partition_last_seen/
-- mark_mounted 确认过一次真正的挂载成功"。历史行的既有值原样保留——它们是
-- 靠 0012/0013 时代"每次 record 都 touch"的旧语义写入的，这次迁移不重新
-- 判断它们是否曾经真的挂载成功过，只改变今后 INSERT 分支的行为。表结构
-- 照抄 0013 之后的真实八列 schema（含 org_id/space_id 及其外键/唯一约束），
-- 不是 0012 时代的六列旧结构。
ALTER TABLE mem_os_partitions RENAME TO mem_os_partitions_v0014;

CREATE TABLE mem_os_partitions (
    owner_key     TEXT PRIMARY KEY,
    key_version   TEXT NOT NULL CHECK (trim(key_version, char(32)||char(9)||char(10)||char(13)) <> ''),
    org_id        TEXT NOT NULL REFERENCES mem_orgs(org_id),
    space_id      TEXT NOT NULL CHECK (trim(space_id, char(32)||char(9)||char(10)||char(13)) <> ''),
    logical_user  TEXT NOT NULL CHECK (trim(logical_user, char(32)||char(9)||char(10)||char(13)) <> ''),
    module_name   TEXT NOT NULL CHECK (trim(module_name, char(32)||char(9)||char(10)||char(13)) <> ''),
    first_seen_at TEXT NOT NULL,
    -- NULL: this identity's row has been recorded, but no mount has ever been
    -- confirmed successful for it (T8.5c-W-mount decision 5).
    last_seen_at  TEXT
);

INSERT INTO mem_os_partitions
    (owner_key, key_version, org_id, space_id, logical_user, module_name,
     first_seen_at, last_seen_at)
SELECT owner_key, key_version, org_id, space_id, logical_user, module_name,
       first_seen_at, last_seen_at
FROM mem_os_partitions_v0014;

DROP TABLE mem_os_partitions_v0014;

-- Both indexes are lost when the table is rebuilt, so both are rebuilt.
CREATE INDEX mem_os_partitions_user ON mem_os_partitions (logical_user);
CREATE UNIQUE INDEX mem_os_partitions_space ON mem_os_partitions (org_id, space_id);
