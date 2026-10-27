# P1-02b 容量口径只读对照

> 2026-10-24。对照基线 `origin/ab/memory-p1` = `a09561c2da10ece2cdb6ed6ed10511379ed74422`。本文只读源码，记录行数、payload、磁盘、附件和 per-owner override 五条口径。未打开用户私有库，未创建或读取 `*.sqlite` / `*.db`，未运行 `p1_capacity`，未启动 purge，未改配额失败行为。
>
> 计划原文：`docs/agent/PLAN-MEMORY-P1.md:53`。P1-02 要求核对 per-owner override 与 personal 路径；默认 `200000` / `256 MiB` 是 migration `0014` 的静态配置，不是磁盘占用或机器测量。

种子整数来自 SQL，不是本次测量：`rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql:82-83` 插入 `owner='*'`、`max_rows=200000`、`max_bytes=268435456`。`268435456 = 256 × 1024 × 1024`。同文件 `:77` 写明该档是 “Guessed, not measured”。下文不把这两个整数写成实测占用。

## 总表

| 口径 | 静态定义 | 本次是否给出占用数字 |
|---|---|---|
| 行数 | `mem_events` 每个 `scope_owner` 的行计数，上限种子 `200000` | 否。只有种子，没有库内行数 |
| payload | `LENGTH(CAST(mem_events.payload AS BLOB))`，上限种子 `268435456` | 否。只有种子，没有库内字节 |
| 磁盘 | 配额门不读页数、WAL 文件或 `metadata` | **未证**。无磁盘字节数字 |
| 附件是否计入行数 / payload | 配额 SQL 不读 `mem_trace_refs` / `mem_artifacts` | 源码排除这两张表；不是对库的测量 |
| 附件是否计入磁盘 | 没有磁盘计量 | **未证** |
| per-owner override | 触发器先查该 owner 行，再落到 `'*'` | 机制在 SQL 中。生产路径没有写入覆盖行 |

`agent24-store` 的 `QuotaExceeded`（`rust/crates/agent24-store/src/lib.rs:120-125`）和 `MODULE_SCHEDULE_QUOTA = 256`（`rust/apps/agent24d/src/scheduler_callback.rs:52-53`）计的是模块 `schedules` 行，不读 `mem_owner_quota`。下文口径不包括它。

personal 与 module 共用同一张 `mem_events` 和同一个 `mem_events_bi_quota`。`0016_personal_partition.sql:1-3` 只给 `mem_os_partitions` 加 `space_kind`。daemon 打开的是 `memory.db`（`rust/apps/agent24d/src/server.rs:487`），personal owner 由 `session_memory` 写入目录（`server.rs:506-519`）。本次没有打开该文件，没有 personal 行数或字节。

## 1. 行数口径

`mem_owner_usage.row_count` 是 `mem_events` 的派生计数，按 `scope_owner` 维护（`0014_owner_usage_quota.sql:7-8`）。

- 回填：`COUNT(*)`，`0014_owner_usage_quota.sql:91-92`。
- 新行：`AFTER INSERT` 把 `row_count` 加 1，`0014_owner_usage_quota.sql:97-101`。
- 门：插入前用「当前 `row_count` + 1」比较限额，`0014_owner_usage_quota.sql:189-204`。已存在的 `id` 被 `WHEN NOT EXISTS` 跳过，`0014_owner_usage_quota.sql:180`、`:189`。
- `seq` 可以有洞。`MAX(seq)` 不是行数，`rust/crates/agent24-memory/src/event.rs:7-9`。
- 默认上限是 `'*'` 行的 `max_rows = 200000`，`0014_owner_usage_quota.sql:82-83`。

拒绝文本 `mem_quota:rows` 由 `event.rs:29`、`event.rs:42` 映射为 `QuotaExceeded { dimension: "rows" }`。本文不实现、不改这条失败行为。

只读检查（仓库根目录；exit 0 表示下列源码片段仍在）：

```bash
python3 - <<'PY'
import pathlib, sys
sql = pathlib.Path("rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql").read_text().splitlines()
event = pathlib.Path("rust/crates/agent24-memory/src/event.rs").read_text().splitlines()
need = [
    (sql, 83, "VALUES ('*', 200000, 268435456);"),
    (sql, 92, "COUNT(*)"),
    (sql, 101, "row_count = row_count + 1,"),
    (sql, 201, "row_count FROM mem_owner_usage"),
    (event, 9, "MAX(seq)` is NOT a row count"),
]
for src, n, frag in need:
    if frag not in src[n - 1]:
        sys.exit(1)
sys.exit(0)
PY
```

2026-10-24 在本工作树执行，退出码 **0**。该命令只读两个源文件。

## 2. payload 口径

`byte_count` 只加 `LENGTH(CAST(mem_events.payload AS BLOB))`。

- 列定义：`payload` 是 JSON body，`rust/crates/agent24-memory/migrations/0002_events.sql:16`。
- 写入：`serde_json::to_string(&e.body)`，`event.rs:452`（`append_tx`）和 `event.rs:557`（`append`）。
- 计数点：插入 `0014_owner_usage_quota.sql:99`、累加 `:102`、限额比较 `:205-207`。回填求和在 `:92`。
- 迁移自己的边界：`0014_owner_usage_quota.sql:29-34` 写明这是事件正文内容，排除 `scope`、`causal`、`origin_*`、`id`、`at` 和索引。
- 默认上限是 `'*'` 行的 `max_bytes = 268435456`，`0014_owner_usage_quota.sql:83`。
- 模块单次 remember 另有 `MAX_BODY_BYTES = 64 * 1024`，`rust/apps/agent24d/src/os_memory.rs:156`，在 `os_memory.rs:786` 拒绝更大的序列化 body。`os_memory.rs:143-144` 写明这两个上限 “not make it a quota”。

会话轮次把整条 `Msg` 放进 `mem_events.payload`：`rust/crates/agent24-memory/src/session_log.rs:105-120`。因此用户句和助手句的 JSON 正文进入 payload 口径。这是事件 body，不是独立附件文件。

只读检查：

```bash
python3 - <<'PY'
import pathlib, sys
sql = pathlib.Path("rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql").read_text().splitlines()
ev = pathlib.Path("rust/crates/agent24-memory/migrations/0002_events.sql").read_text().splitlines()
event = pathlib.Path("rust/crates/agent24-memory/src/event.rs").read_text().splitlines()
osmem = pathlib.Path("rust/apps/agent24d/src/os_memory.rs").read_text().splitlines()
need = [
    (sql, 30, "NOT the row's total on-disk footprint"),
    (sql, 99, "LENGTH(CAST(new.payload AS BLOB))"),
    (sql, 102, "byte_count = byte_count + LENGTH(CAST(new.payload AS BLOB)),"),
    (sql, 207, "LENGTH(CAST(new.payload AS BLOB))"),
    (ev, 16, "payload       TEXT NOT NULL,            -- JSON body"),
    (event, 452, "serde_json::to_string(&e.body)"),
    (event, 557, "serde_json::to_string(&e.body)"),
    (osmem, 144, "not make it a quota"),
    (osmem, 156, "MAX_BODY_BYTES: usize = 64 * 1024"),
]
for src, n, frag in need:
    if frag not in src[n - 1]:
        sys.exit(1)
sys.exit(0)
PY
```

2026-10-24 执行，退出码 **0**。

## 3. 磁盘口径

配额门没有磁盘字节项。`mem_events_bi_quota` 比较的是 `row_count` 和 `byte_count`（`0014_owner_usage_quota.sql:200-210`）。`byte_count` 的注释要求调用方不要把它读成 “how much disk this owner occupies”（`0014_owner_usage_quota.sql:33-34`）。

`0014` 里的 `WAL` 出现在并发说明：单写者序列化依赖 WAL 与 `busy_timeout`（`0014_owner_usage_quota.sql:175-176`）。该句不把 WAL 文件长度加进 `byte_count`。

全 `rust/` 树里，`page_count` 只出现在合成探针 `rust/crates/agent24-memory/tests/p1_capacity.rs`。该测试声明自己不估计生产容量（`:1-2`），并写明 `byte_count` 不是 SQLite 文件或磁盘占用（`:91`）。它在临时库上读取 `pragma_page_count(), pragma_page_size()`（`:113-114`）后打印。打印值不写回 `mem_owner_quota`。本次没有运行这个测试，因此没有页数、页大小或二者的乘积。

磁盘占用数字：**未证**。

只读检查（确认配额 SQL 不含页数或文件长度，且 `page_count` 只留在上述测试）：

```bash
python3 - <<'PY'
import pathlib, sys
quota_path = pathlib.Path("rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql")
quota = quota_path.read_text()
lines = quota.splitlines()
if "NOT the row's total on-disk footprint" not in lines[29]:
    sys.exit(1)
if "WAL +" not in lines[174]:
    sys.exit(1)
for token in ("page_count", "page_size", "metadata(", "std::fs::"):
    if token in quota:
        sys.exit(1)
hits = sorted(
    str(path)
    for path in pathlib.Path("rust").rglob("*.rs")
    if "/target/" not in str(path) and "page_count" in path.read_text(errors="replace")
)
if hits != ["rust/crates/agent24-memory/tests/p1_capacity.rs"]:
    sys.exit(1)
cap = pathlib.Path(hits[0]).read_text().splitlines()
if "pragma_page_count(), pragma_page_size()" not in cap[113]:
    sys.exit(1)
sys.exit(0)
PY
```

2026-10-24 执行，退出码 **0**。exit 0 只说明源码里没有磁盘计量接入配额门。它不产生磁盘字节。

## 4. 附件是否计入

行数和 payload 的触发器只挂在 `mem_events` 上。`0014_owner_usage_quota.sql` 全文没有 `mem_trace_refs`、`mem_artifacts` 或 `attachment`。

当前源码里的两类 “正文在别的表”：

- 工具全文溢出：`0009_trace.sql:1-2` 把完整工具输出放进 ref blob。表 `mem_trace_refs.body` 是库内 `TEXT`（`0009_trace.sql:24-27`），字节检查在 `:31`。写入在 `trace.rs:142-149`，插入 `mem_trace_refs`，不插入 `mem_events`。`SymbolicTrace` 在 `trace.rs` 以外只出现在 `KvStore::trace` 构造（`rust/crates/agent24-memory/src/lib.rs:449-452`）。`TaskTrace` 与 `.trace()` 只出现在 `trace.rs`，其中 `.trace()` 在测试辅助 `trace.rs:256-257`。本次检索没有看到 daemon 或 agent 生产路径调用 `record`。
- 可编辑 artifact：`mem_artifacts.body` 同样是库内 `TEXT`（`0003_artifacts.sql:20-24`）。`artifact.rs:19-20` 写明这一片 “does not touch the filesystem”。`reconcile.rs:185-200` 的 `observe_dir` 能读调用方给出的目录，那是对账，不是配额门。本次没有调用它。

这两张表都在 `OTHER_OWNER_SCOPED_TABLES`（`lib.rs:223-231`），rekey 不把它们的字节并进 `mem_owner_usage`。

因此：以配额 SQL 为准，`mem_trace_refs` 和 `mem_artifacts` 的行和 body **不进入** `row_count` / `byte_count`。这是源码检索，不是对某个库的 `COUNT` 或 `SUM`。

附件字节是否进入 SQLite 主库、WAL、索引或库外文件的磁盘占用：**未证**。本文没有测量值，不填写字节、页数或文件长度。

只读检查：

```bash
python3 - <<'PY'
import pathlib, sys
quota = pathlib.Path("rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql").read_text()
for token in ("mem_trace_refs", "mem_artifacts", "attachment"):
    if token in quota:
        sys.exit(1)
trace_sql = pathlib.Path("rust/crates/agent24-memory/migrations/0009_trace.sql").read_text().splitlines()
trace = pathlib.Path("rust/crates/agent24-memory/src/trace.rs").read_text().splitlines()
art = pathlib.Path("rust/crates/agent24-memory/migrations/0003_artifacts.sql").read_text().splitlines()
art_rs = pathlib.Path("rust/crates/agent24-memory/src/artifact.rs").read_text().splitlines()
if "CREATE TABLE mem_trace_refs" not in trace_sql[23]:
    sys.exit(1)
if "body        TEXT NOT NULL" not in trace_sql[26]:
    sys.exit(1)
if "INSERT INTO mem_trace_refs" not in trace[142]:
    sys.exit(1)
if "body          TEXT    NOT NULL" not in art[23]:
    sys.exit(1)
if "not touch the filesystem" not in art_rs[19]:
    sys.exit(1)
outside = []
for path in pathlib.Path("rust").rglob("*.rs"):
    rel = str(path)
    if "/target/" in rel or rel.endswith("src/trace.rs"):
        continue
    text = path.read_text(errors="replace")
    if "SymbolicTrace" in text or "TaskTrace" in text or ".trace()" in text:
        outside.append(rel)
if sorted(outside) != ["rust/crates/agent24-memory/src/lib.rs"]:
    sys.exit(1)
lib = pathlib.Path("rust/crates/agent24-memory/src/lib.rs").read_text().splitlines()
if "pub fn trace(&self)" not in lib[450]:
    sys.exit(1)
sys.exit(0)
PY
```

2026-10-24 执行，退出码 **0**。exit 0 确认配额 SQL 不点名这两张表，并且溢出插入点仍在 `mem_trace_refs`。它不测量磁盘，所以附件是否计入磁盘仍是 **未证**。

## 5. per-owner override 是否存在

SQL 机制存在。`0014_owner_usage_quota.sql:39-42` 把 `'*'` 写成 fallback，并写明可以有 per-owner override，将来用 `INSERT ... ON CONFLICT(owner) DO UPDATE` 写入。限额比较先取 `owner = new.scope_owner`，没有该行再用 `owner = '*'`：行数 `:202-204`，字节 `:208-210`。

`'*'` 行不能删除（`:57`、`:61`），也不能改名离开 `'*'`（`:70`、`:74`）。种子只有这一行（`:82-83`）。

生产 Rust 没有插入其他 owner 的配额行。`INSERT INTO mem_owner_quota` / `UPDATE mem_owner_quota` 的命中是：

| 位置 | 所在范围 |
|---|---|
| `0014_owner_usage_quota.sql:82` | 迁移种子，只写 `'*'` |
| `event.rs:1225-1227` | `event.rs:660` 起的 `#[cfg(test)]` |
| `lib.rs:2345`、`lib.rs:2759-2760` | `lib.rs:1125` 与 `:2708` 的 `#[cfg(test)]` |
| `tests/p1_capacity.rs` | 集成测试，临时库里写入覆盖行 |
| `session_failure_tests.rs:61-64` | 只 `UPDATE` `owner='*'`；由 `agent24-agent/src/lib.rs:2393` 的 `#[cfg(test)]` 引入 |

所以：触发器能够识别 per-owner 行；当前生产代码没有按 owner 写入覆盖行的路径。本文没有在任何库里查询 `mem_owner_quota` 的实际行。

只读检查：

```bash
python3 - <<'PY'
import pathlib, sys
sql = pathlib.Path("rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql").read_text().splitlines()
need = {
    41: "per-owner overrides",
    83: "VALUES ('*', 200000, 268435456);",
    203: "WHERE owner = new.scope_owner",
    204: "WHERE owner = '*'",
    209: "max_bytes FROM mem_owner_quota WHERE owner = new.scope_owner",
    210: "max_bytes FROM mem_owner_quota WHERE owner = '*'",
}
for n, frag in need.items():
    if frag not in sql[n - 1]:
        sys.exit(1)
allowed = {
    "rust/crates/agent24-memory/migrations/0014_owner_usage_quota.sql",
    "rust/crates/agent24-memory/src/event.rs",
    "rust/crates/agent24-memory/src/lib.rs",
    "rust/crates/agent24-memory/tests/p1_capacity.rs",
    "rust/crates/agent24-agent/src/session_failure_tests.rs",
}
writes = []
for path in pathlib.Path("rust").rglob("*"):
    rel = str(path)
    if path.suffix not in {".rs", ".sql"} or "/target/" in rel:
        continue
    for i, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
        if "INSERT INTO mem_owner_quota" in line or "UPDATE mem_owner_quota" in line:
            writes.append((rel, i))
if not writes:
    sys.exit(1)
for rel, lineno in writes:
    if rel not in allowed:
        sys.exit(1)
    if rel.endswith("src/event.rs") and lineno < 660:
        sys.exit(1)
    if rel.endswith("src/lib.rs") and lineno < 1126:
        sys.exit(1)
agent = pathlib.Path("rust/crates/agent24-agent/src/lib.rs").read_text().splitlines()
if not agent[2392].startswith("#[cfg(test)]"):
    sys.exit(1)
sys.exit(0)
PY
```

2026-10-24 执行，退出码 **0**。

## 边界

- 未改 `migrations/`、Rust、`protocol/`。
- 未实现配额失败、保留、压实或 P1-02c。
- 未派 H03，未改 R05，未碰 #794，未启动 purge。
- 附件计入磁盘：**未证**。行数、payload、磁盘占用都没有来自用户库或合成库的新数字。
