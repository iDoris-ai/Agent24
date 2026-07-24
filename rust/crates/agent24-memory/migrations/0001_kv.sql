-- Agent24 memory L0: a namespaced key-value store (D1). Values are TEXT
-- holding arbitrary JSON. (namespace, key) is the primary key so a namespace
-- partitions an independent keyspace — e.g. "module_state", "session", "kv".
CREATE TABLE kv (
    namespace  TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (namespace, key)
);
