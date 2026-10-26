# P1-02c：配额失败、保留空间与压实边界（设计说明）

> 2026-10-26。基线：`origin/ab/memory-p1` `812204be`。
>
> **这是设计说明，不写代码。** 评审对象是本分支的精确 head；APPROVE 只接受本稿的语义与分片，不批准任何迁移或实现。各实现片各自开 PR，各自评审。
>
> 上游约束：
> - 生命周期 ADR（`docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md`，§9 已于 #1247 接受）：正常生命周期 no-loss；压缩、归档、配额都不能偷删原文；配额满不能报「已记住」；purge 控制记录需要独立的保留空间。
> - ADR-037 的两级权限（#1249）。
>
> 代码事实见 [`docs/research/P1-02C-FACTS-20261026.md`](../research/P1-02C-FACTS-20261026.md)，每条附 文件:行。口径见 `P1-02B-CAPACITY-CALIBER-20261024.md`。

## 1. 现状中违反 ADR 的地方

| # | 现状 | 违反 | 证据 |
|---|---|---|---|
| V1 | 配额满时，撤回（`assertion.retracted`）也被同一个触发器拒绝，整笔回滚，断言仍然有效。`DELETE /memory/assertions/{id}` 返回 500 `internal`。用户满额后无法忘记任何东西 | 「撤回始终可用」；配额不得阻断用户控制 | facts §2；`memory_routes.rs:212`；`agent24-memory/src/lib.rs:2544` 测试 |
| V2 | `mem_events` 没有任何不受配额约束的空间。审计、撤回、将来的 purge 控制记录都与内容争同一份额度 | ADR「purge 控制记录须有独立保留空间」 | facts §2 |
| V3 | run 路径写入失败时，只发 `memory.write_failed` WS 事件，没有任何消费方；run 照常 `completed`；普通聊天轮次写入失败时，`memory_receipt` 为空，界面零提示 | 「配额满不能报已记住；原文未保存必须显式显示」 | facts §1；`agent24-agent/src/lib.rs:2023` |
| V4 | 写入失败不留任何持久记录，事后无从查证 | 审计可查 | facts §2 |
| V5 | `memory.write_failed.reason` 直接用 `MemoryError::Display`，含带 NUL 的分区键；进程内模块路径把错误转成字符串，丢失了类型 | 错误应可分类、不泄露内部键 | facts §1、§5 |
| V6 | 没有磁盘满映射，没有写前剩余空间检查；SQLITE_FULL 会变成通用 internal | 失败应可解释、可恢复 | facts §4 |
| V7 | 满额后没有任何回收路径（不 VACUUM、不 checkpoint、`mem_events` 无删除），会话压实只追加摘要 | ADR 允许的回收只有 purge（P1-08）和归档切换（P1-04b），这里不算违反，但必须写明满额的出路 | facts §5 |

## 2. 决定

### D1 配额只约束「新增内容」，用户控制与审计走保留空间

`mem_events` 的 kind 分成两类，由迁移中的封闭清单决定（新增 kind 默认属于 A，进入 B 必须改迁移并评审）：

- **A 内容类**（受基础配额约束）：消息、断言、重申、会话摘要、trace，以及其他新增内容。
- **B 控制与审计类**（只受保留空间约束）：
  - `assertion.retracted`、`mem.write_decision`；
  - 本稿新增的 `mem.write_rejected`、`session.gap`；
  - 将来的 purge 控制与审计 kind；
  - 设置变更类。

触发器改为：
- A 类：`row_count + 1 ≤ max_rows`，且 `byte_count + len ≤ max_bytes`（现状不变）。
- B 类：`row_count + 1 ≤ max_rows + reserve_rows`，且 `byte_count + len ≤ max_bytes + reserve_bytes`。

默认 `reserve_rows = 10000`、`reserve_bytes = 16 MiB`。这是推测值，P1-02c-0 实测后才复核，写进迁移注释。

用量统计不分类，A、B 合计进 `row_count`/`byte_count`，告警（D6）按合计计算。

**防滥用**：
- B 类单行 payload 有上限（例如 4 KiB），只允许元数据，不得携带正文。
- `mem.write_rejected` 按 owner 限频（见 D3）。
- 撤回数量的上界是已有断言数，不会无限增长。

**reassert 归 A 类**：它会让失效内容重新生效，等同于新增内容。

**重申「满额后能撤回」**：只要 B 类保留空间还有余量，撤回一定成功。保留空间本身耗尽，只可能发生在异常滥用时；这时返回类型化错误 `memory_reserve_exhausted`，不再回落为 internal。

### D2 统一的类型化失败与各路径回执

新增错误码闭集：

| 码 | 含义 |
|---|---|
| `memory_quota_rows` | 行数配额已满 |
| `memory_quota_bytes` | 字节配额已满 |
| `memory_reserve_exhausted` | 保留空间耗尽 |
| `memory_disk_low` | 写前检查发现磁盘剩余不足 |
| `memory_disk_full` | 写入时遇到 SQLITE_FULL |
| `memory_internal` | 其他内部错误 |

错误对外只带码、维度和 owner 类别（`personal` / `module`），不带分区键或 Display 原文（修 V5）。

各路径的回执：

| 路径 | 失败时用户必须看到 |
|---|---|
| chat 显式「记住」 | 维持 `memory_receipt: "failed"`，新增 `memory_error_code` 字段 |
| chat 普通轮次 | `memory_receipt: "not_saved"` 加错误码；桌面在该轮显示「本轮未保存到记忆」 |
| run 路径 | `memory.write_failed{code, dimension, owner_kind}`（类型化，不含原文）；run 结束事件携带 `memory_persisted: false`；桌面和 TUI 必须消费并显示，属 P1-03 的验收项 |
| `DELETE /memory/assertions/{id}` | 按 D1 不应再因配额失败。保留空间耗尽时返回 507 `memory_reserve_exhausted`，其他错误保持 500 |
| 模块 `os_memory` | 进程外路径保持现有的 `quota_exceeded`，改为带维度；进程内路径改为类型化，不再转字符串 |

**模型口头说「记住了」不算回执**（与 P1-10 一致）。界面以结构化回执为准，并把两者冲突的情况标出来。

### D3 失败留痕：`mem.write_rejected`

每次 A 类写入被拒，都在保留空间写一行 `mem.write_rejected`。payload 只含元数据：错误码、维度、被拒 kind、拒绝时的计数、时间。不含正文。

**限频**：同一 owner、同一错误码，每 60 秒最多写一行；被合并的次数累加进下一行的 `suppressed_count`。

这是审计，不是重试队列：系统不会自动重写被拒的内容。

### D4 会话历史的缺口要显式化

某一轮写入失败后，后续上下文不含这一轮（facts §1，`SessionLog`），这本身符合「不伪造」。但必须让用户和后续轮次知道这里有缺口：

- 在保留空间写一行 `session.gap`，记录 session、轮次号、错误码，不含正文。
- 构造后续上下文时，遇到 `gap` 就插入一条宿主系统说明：「此处有 N 轮未能保存」。这条说明的来源 kind 记为 HostSystem，与 R05-5 一致。
- 会话压实摘要写入失败时，同样记 `session.gap`，并发出事件，不能只打 `tracing::error!`。
- `append_turn` 与 `persist` 是两个事务，可能出现「消息已存、断言未存」。这时回执写成「对话已保存、记忆未写入」，不能笼统说「已保存」。

### D5 磁盘

- **写前检查**：在 `memory.db` 所在卷（不是 cwd）检查剩余空间，低于 `max(512 MiB, 2 × 当前 WAL 大小)` 时，A 类写入返回 `memory_disk_low`；B 类写入在剩余空间高于 64 MiB 时仍然允许，保证撤回可用。检查做缓存，每 5 秒最多读一次，避免逐写 statfs。
- **SQLITE_FULL（code 13）**：映射为 `memory_disk_full`，可借鉴 `agent24-documents/src/state.rs` 的现有映射。
- 平台差异：Windows 的卷剩余空间读法留给 PLT-WIN。

### D6 告警阈值（供 P1-03 实现）

- **按 owner 告警**：行数或字节用量超过基础配额的 80% 发 `warning`，超过 95% 发 `critical`；分别回落到 75% 和 90% 以下时解除（滞回）。
- **磁盘告警**：剩余低于 2 GiB 发 `warning`，低于 D5 的门槛发 `critical`。
- **告警通道**：用量 API（P1-03a）和事件 `memory.usage_level_changed`，后者只在跨档时发出。
- **阈值要可测**：阈值本身写在配置里，测试用小配额 owner 覆盖跨档与解除。

### D7 不自动回收；满额的出路

- **不做自动删除**：到限不删、不压缩原文、不自动扩大限额。
- **用户可用的出路**，按交付顺序：
  1. 撤回（D1 保证可用）；撤回不释放空间，但让内容不再被召回。
  2. 管理员调整 owner 限额（D8）。
  3. 冷归档（P1-04b）：归档包经校验、回放等价后，才切换引用并回收热副本。
  4. purge（P1-08）。
- **WAL**：允许定期执行 `wal_checkpoint(TRUNCATE)` 限制 WAL 长度。它不删除任何逻辑数据，只在空闲时执行，单独成片（c5）。
- **VACUUM / incremental_vacuum**：只在 purge 或归档回收之后由对应流程触发，本稿不新增。

### D8 owner 限额调整

- 新增 owner 级配额覆盖的写入口（API / CLI），属于 ADR-037 的**提升级**操作，需要用户在场证明。
- 只允许设定绝对值，不允许设成「无限」；把限额调到低于当前用量时，不删数据，只让后续 A 类写入被拒。
- 每次调整写 B 类审计行（`mem.quota_changed`，记录旧值、新值和操作者的不透明 id）。

### D9 测量先于调参

`P1-02c-0`：在隔离的合成库上运行现有的 `p1_capacity` 探针，扩展到真实的 personal 写入路径（chat 和 retain），记录 `page_count × page_size`、WAL 峰值、`mem_events` 平均 payload、每千轮对话的行数与字节。结果写入 `docs/research/`，只作为 D1 保留空间和 D6 阈值的复核依据。**默认限额不因测量自动改变**；要改，另开一份决定。

## 3. 测试门（每片 red → green，并做变异验证）

| 门 | 断言 |
|---|---|
| T1 满额撤回 | 把 owner 配额设为当前用量，撤回成功，断言失效；去掉 B 类分支后测试变红 |
| T2 保留空间上界 | B 类超出 `max + reserve` 时返回 `memory_reserve_exhausted`（507），不是 500 |
| T3 A 类仍然拒绝 | 满额后新增消息或断言被拒，`mem_events` 无新增内容行，`mem.write_rejected` 有且只有一行（限频窗口内） |
| T4 回执 | chat 显式记住、普通轮次、run 三条路径在满额时都给出类型化失败回执；run 结束事件 `memory_persisted:false`；任何回执都不出现「已记住」 |
| T5 不泄露 | 失败事件与 HTTP 错误体中不含分区键、NUL 或 Display 原文 |
| T6 会话缺口 | 某轮写入失败后，下一轮上下文含 HostSystem 缺口说明，且来源行可证明 |
| T7 磁盘 | 模拟卷剩余不足：A 类返回 `memory_disk_low`，B 类撤回仍成功；注入 SQLITE_FULL 映射为 `memory_disk_full` |
| T8 限额调整 | 没有在场证明时拒绝；有证明时写入审计行；下调到低于用量后不删数据 |
| T9 分区隔离 | 模块 owner 写满，不影响 personal owner 的写入与撤回 |

## 4. 实现分片（批准后再派，各自独立 PR）

| 片 | 内容 | 依赖 |
|---|---|---|
| c0 | D9 测量报告（只读、合成库） | — |
| c1 | 迁移 0024：kind 分类封闭清单、B 类保留空间、新 kind；T1、T2、T3、T9 | — |
| c2 | 类型化错误码与各路径回执、`memory_persisted`；T4、T5 | c1 |
| c3 | `mem.write_rejected` 限频与 `session.gap` 及上下文缺口说明；T3、T6 | c1 |
| c4 | 磁盘写前检查与 SQLITE_FULL 映射；T7 | c2 |
| c5 | 空闲时 WAL checkpoint(TRUNCATE) | — |
| c6 | owner 限额调整入口（提升级）；T8 | c1，ADR-037 提升级机制（CORE-2） |

c6 依赖 CORE 线的在场证明。在那之前，限额只能离线改，并在文档中注明。告警与用量 API 属于 P1-03，不在本稿。

## 5. 不在本稿

- 用量 API 与记忆页 UI（P1-03）；附件外置（P1-04a）；冷归档（P1-04b）；purge（P1-08）。
- 改动默认的 200000 行 / 256 MiB。
- 跨 owner 的全局上限：模块数增长时总量会叠加，这条记为风险，留给 P1-03 告警之后再决定。
