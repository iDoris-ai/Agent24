# 记忆隔离与分区（Memory Isolation & Partitions）

> 什么可以越过记忆边界、什么绝不能。规则与形状见 [`README.md`](README.md)。
>
> 立于 2026-09-20。迁移自两处：[`../agent/architecture.md`](../agent/architecture.md)「不可动摇的边界」六条（2026-08-23 记录），
> 与 [`../specs/SPEC-ME-FOLLOWUPS.md`](../specs/SPEC-ME-FOLLOWUPS.md) F1「不得声称的话」六条（2026-08-22，F1 经三轮对抗复审）。
> 按 README 的要求逐条重写，**「今天靠什么保证」按 2026-09-20 的代码实况填写**；没有机制的地方如实写「没有机制」。

---

## 0. 核实记录（2026-09-20）

写法律之前先复核现状 —— 既不把不存在的缺陷写成已知问题，也不把不存在的保证写成机制：

| 查了什么 | 结果 | 位置 |
|---|---|---|
| 面向模块的记忆契约是否含 scope / id 参数 | **没有。** 只有 `remember` / `recall` / `recent`；`Remember` 只有 `kind` + `body` | `agent24-domain/src/memory.rs:167-192`；测试 `a_module_cannot_name_what_it_remembers`（`:200`） |
| 模块句柄是否握有可外达的原始 store | **没有。** 实现只持有私有 `key`、`module`、一个 `EventLog`，无 accessor | `agent24d/src/os_memory.rs:636-649` |
| `kv` 表是否 owner-scoped | **不是。** 主键 `(namespace, key)`，没有 owner 列 | `agent24-memory/migrations/0001_kv.sql` |
| 记忆事件 id 是否分区内唯一 | **不是。** `id TEXT NOT NULL UNIQUE` 是**数据库全局**唯一 | `agent24-memory/migrations/0002_events.sql:9` |
| 分区目录是否存在 | **存在。** `mem_os_partitions` 记 `owner_key / key_version / 逻辑身份` | migration 0012 建，0013 / 0015 修 |
| re-key 是否单事务、是否会因其它表有行而拒绝 | **是。** 事务内检查，且有专门测试 | `agent24-memory/src/lib.rs:805-834`（`pool.begin()`）；测试 `:1616` / `:2027` |
| 全局重建操作是否在模块句柄上 | **不在。** `FtsRetriever::rebuild()` 只在 crate 层 | `agent24-memory/src/retriever.rs:55` |

---

## 法律

### L-MEM-1 · 模块永远拿不到原始 store

**模块 MUST NOT 取得 `KvStore`、数据库连接池、`EventLog`，或任何由它们派生的原始存储句柄
（events / artifacts / assertions / retriever / consolidation / knowledge / trace / vector 的访问器）。**

**挡的是什么**：拿到 `KvStore` 就等于拿到全部 —— 上面那些访问器都挂在它身上，而它们大多不带
owner 谓词。持有原始句柄的模块不需要「绕过」隔离，它只是不再经过隔离。

**今天靠什么保证**：**机制 + 类型**。`ScopedMemory` 只暴露三个业务方法，没有任何 accessor
返回底层 store（`agent24-domain/src/memory.rs:167`；模块文档 `:12-24` 同样写明「没有底层句柄外逃」）；daemon 的实现字段私有、无 getter
（`agent24d/src/os_memory.rs:636`）。进程外模块根本不链接本仓库的 crate，连类型都拿不到。

---

### L-MEM-2 · 面向模块的记忆 API 不得接受作用域参数

**任何交给模块的记忆 API MUST NOT 接受 owner / space / module / scope 参数；作用域 MUST 由内核注入。**

**挡的是什么**：一个接受 owner 参数的 API，会把隔离退回到「调用方自觉」。
模块只要传另一个 owner 的键就能跨读 —— 而这不是攻击，是误用就会发生。
「注入而不是接受」正是 `EventSink` 盖模块名、而不是接受模块名的同一条规则。

**今天靠什么保证**：**机制 + 编译期**。trait 三个方法签名都不含作用域；`Remember` 只有
`kind` + `body`，**没有 id、也没有任何能夹带 owner 的字段**，测试
`a_module_cannot_name_what_it_remembers`（`agent24-domain/src/memory.rs:200`）用集合断言钉住。
owner 由 `OsScopedMemory` 自己派生并自填。

---

### L-MEM-3 · 模块不得获得跨 owner 的全局维护操作

**模块 MUST NOT 取得任何跨 owner 的维护或重建操作（`rebuild`、全量重扫、跨 owner 聚合）；
那些 MUST 只在内核运维路径上可用。**

**挡的是什么**：`FtsRetriever::rebuild()` 会 `DELETE FROM mem_assertions_fts` 再从全表重建 ——
这是**跨租户副作用**：一个模块能清掉并重建别的模块和用户自己的检索投影。
它读不到别人的数据，但能让别人读到暂时的空。

**今天靠什么保证**：**机制**。`ScopedMemory` 上没有任何全局操作（`agent24-domain/src/memory.rs:167`）；
`rebuild` 只存在于 crate 层（`agent24-memory/src/retriever.rs:55`）；模块拿到的 `KernelCtx` 只暴露 `events()` 与 `memory()`（`agent24-domain/src/lib.rs:1126-1151`）。进程外模块不链接本仓 crate；编译进内核的模块若直接依赖 crate 绕过，那是复审/信任边界，不是沙箱。
F1 复审逐条扫过「接受 owner 作为参数的 API」，确认没有暴露给模块。

---

### L-MEM-4 · 进 key 的只有不可变 ID

**只有不可变的稳定标识（org / space / user 等）MUST 进入存储 key；
任何会变更的分类（部门、授权、作用域、显示名）MUST NOT 进 key。**

**挡的是什么**：把可变属性编进 key，一次改名或重组就要全量重写数据，而旧数据无从归属 ——
迁移成本随数据量单调上升。

**今天靠什么保证**：**机制 + 目录**。ADR-030 决策 2/3 定死；`mem_os_partitions`
（migration 0012 / 0013）把逻辑身份放在**独立的目录行**里，而不是编进 key；
`key_version` 表示当前编码，供将来的迁移代码识别 —— 且**不得**用对含 NUL 的 owner 字符串做
SQL 前缀匹配来发现分区（F1 复审逐条否掉过的做法）。

---

### L-MEM-5 · 分区迁移必须单事务

**任何分区迁移（含 re-key）MUST 在单个事务内完成：事件行、checkpoint、目录行同生共死。**

**挡的是什么**：**半个搬移比不搬更糟** —— 数据看起来还在，但归属不明，既读不到也删不掉。

**今天靠什么保证**：**机制 + 测试**。`rekey_os_partition` 在事务内先检查再搬
（`agent24-memory/src/lib.rs:805-834`，`self.pool.begin()`，注释写明「check 与 move 看到同一个状态」）。

---

### L-MEM-6 · re-key 只搬 events 与 checkpoints，其余有行即拒

**`rekey_os_partition` MUST 只迁移 events 与 checkpoints；
其余 owner-scoped 表若已有行 MUST 拒绝这次迁移，MUST NOT 静默孤儿化。**

**挡的是什么**：默默跳过就是制造孤儿数据 —— 行还在，但没有任何路径再认领它。

**今天靠什么保证**：**机制 + 测试**。`a_rekey_refuses_when_another_owner_scoped_table_holds_rows`
（`agent24-memory/src/lib.rs:1616`）与 `t85b_rekey_refusal_leaves_owner_usage_unchanged`（`:2027`）
钉住拒绝路径与拒绝后的状态。

---

### L-MEM-7 · 隔离主张不得超过机制

**任何契约、API 文档或对外表述 MUST NOT 声称记忆隔离具备机制未提供的性质；
主张的范围 MUST 与本文件各条的「今天靠什么保证」一致。**

**挡的是什么**：F1 / F8 二十余轮对抗复审抓到的几乎全是**同一类**问题 —— 措辞比机制强。
它危险，因为下一个读文档的人会据此设计功能，直到撞上不存在的保证。
下面六句都已被逐条否定，**写在任何地方之前先读这张表**：

| 不得声称 | 真相 | 相关法律 |
|---|---|---|
| ❌「schema 层面强制的**模块**隔离」 | schema 强制的是**不透明 owner** 的隔离；模块含义由内核包装层给出 | L-MEM-1 / L-MEM-2 |
| ❌「每张记忆表都是 owner-scoped」 | **`kv` 不是** —— 主键是 `(namespace, key)`，无 owner 列 | L-MEM-2 |
| ❌「模块有独立键空间」 | 除非先处理全局唯一的事件 / 断言 id（`mem_events.id` 是全局 `UNIQUE`） | L-MEM-4 |
| ❌「agent / session / run 是隔离边界」 | 它们只是序列化元数据，**存储层没有强制** | L-MEM-2 |
| ❌「GDPR 就绪的按用户删除」 | 除非有一条被测过的内核枚举 / 目录操作 | L-MEM-4（目录） |
| ❌「将来迁到方案 B 是无损、轻松的」 | 会产生**语义迁移债**；旧数据与已卸载模块的归属无从判断 | L-MEM-4 |

**今天靠什么保证**：**部分靠机制，部分是纯纪律**。
① 已被 L-MEM-1/2 的类型挡住；② 是事实陈述（`0001_kv.sql`），但**没有 CI 门阻止你写错**；
③ 的全局 ID 已知，且 `Remember` 无 id 字段（`a_module_cannot_name_what_it_remembers`）；
④⑤⑥ **今天只有这条法律，没有任何机械门** —— 这正是 README 说的「诚实地写『今天没有机制』」。

---

## 与既有文档的关系

- [`../specs/SPEC-ME-FOLLOWUPS.md`](../specs/SPEC-ME-FOLLOWUPS.md) **F1** 是本文件的直接来源，含三条方案对比、三轮对抗复审与四条发版级洞的修复记录。
- [`../agent/architecture.md`](../agent/architecture.md)「不可动摇的边界」是 L-MEM-1..6 的来源；本文件是它的**可逐条引用**版本。
- [`CONTEXT.md`](CONTEXT.md) 的 L-CTX-2 与 F6（模块目录 symlink 需 `openat` 句柄）是**同一族**问题：都要求边界由机制而不是字符串匹配来保证。
