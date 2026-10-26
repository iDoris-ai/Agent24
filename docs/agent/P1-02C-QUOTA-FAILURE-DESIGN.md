# P1-02c：配额失败、撤回豁免与压实边界（设计说明）

> 2026-10-26。基线：`origin/ab/memory-p1` `812204be`。第 2 版，回应 Mac mini Codex 第一轮挑战（3 High、3 Medium、1 Low），修订记录见文末。
>
> **这是设计说明，不写代码。** 评审对象是本分支的精确 head；APPROVE 只接受本稿的语义和分片，不批准迁移或实现。各实现片单独开 PR、单独评审。
>
> 上游约束：
> - 生命周期 ADR（`docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md`，§9 已于 #1247 接受）：正常生命周期不丢数据；配额不得偷删原文；配额满时不能报「已记住」；purge 控制记录需要独立保留空间（ADR:46、ADR:62）。
> - ADR-037 的两级权限（#1249）。
> - R05-5 的来源契约（#1248）。
>
> 代码事实见 [`docs/research/P1-02C-FACTS-20261026.md`](../research/P1-02C-FACTS-20261026.md)，每条附 文件:行。

## 1. 现状中违反 ADR 的地方

| # | 现状 | 违反 | 证据 |
|---|---|---|---|
| V1 | 配额满时，撤回（`assertion.retracted`）也被同一个触发器拒绝，整笔回滚，断言仍然有效。`DELETE /memory/assertions/{id}` 返回 500 `internal`。用户在满额后无法让任何记忆失效 | 「撤回始终可用」；配额不得阻断用户控制 | facts W5；`memory_routes.rs:212`；`agent24-memory/src/lib.rs:2544` 的测试 |
| V2 | `mem_events` 没有任何不受配额约束的空间；将来的 purge 控制记录也没有落点 | ADR「purge 控制记录需要独立保留空间」 | facts §2 |
| V3 | run 路径写入失败只发一个 `memory.write_failed` WS 事件，没有任何消费方，run 照常 `completed`；普通 chat 轮次写入失败时 `memory_receipt` 为空，界面没有任何提示；压实摘要（W2）写入失败只打 `tracing::error!` | 「配额满不能报已记住；原文没保存必须显式显示」 | facts W1–W3；`agent24-agent/src/lib.rs:2023` |
| V4 | 写入失败不留任何持久记录 | 审计可查 | facts §2 |
| V5 | `memory.write_failed.reason` 用的是 `MemoryError::Display`，其中以 `{owner:?}` 形式带出分区键（NUL 以转义文本出现）；进程内模块路径把错误转成字符串，丢掉了类型 | 错误要能分类，不泄露内部键 | facts §1；`agent24-memory/src/lib.rs:81`；`os_memory.rs:841` |
| V6 | 没有磁盘满的映射，写入前也不检查剩余空间；SQLITE_FULL 最终变成通用 internal | 失败要能解释、能恢复 | facts §4 |
| V7 | 满额后没有任何回收路径 | 不算违反（ADR 只允许 purge 和归档回收空间），但必须写明满额后的出路 | facts §5 |

## 2. 决定

### D1 撤回豁免配额：靠结构上界，不靠共享保留池

第 1 版提出「B 类 kind 共享一个保留池」。挑战指出这个方案有三个问题：模块可以自选 kind 冒充控制事件；`mem.write_decision` 审计含正文，新增断言可以借它绕过配额；保留池在正常使用下也会耗尽，还会挤占内容额度。第 2 版改为：

1. **内容和成功审计照旧受配额约束（A 类）。** 包括消息、断言、重申、会话摘要、trace、导入，以及 `mem.write_decision`（含正文，与内容在同一事务，R05/H02 现状不变）。新增 kind 默认都是 A 类。
2. **只有撤回豁免配额。** 触发器对 `kind = 'assertion.retracted'` 的行跳过配额比较，但必须同时满足以下三条，否则仍按 A 类处理：
   - **写入者**：只有宿主撤回路径会写这个 kind（`assertion.rs` 的 `forget`）。
   - **owner 类别**：`scope_owner` 是 personal owner。判断方式是在 SQL 里精确关联分区目录（`mem_os_partitions` 的 `space_kind` 为 personal，或者 personal owner 目录表），**不能用字符串前缀判断**。模块 owner 是含 NUL 的版本化键（`os_memory.rs:252`）。模块 owner 不享受豁免，因为它们没有 ADR 要求的「用户撤回」语义。
   - **payload**：触发器逐项检查以下四点：
     - `json_valid(payload)`；
     - 顶层只有一个键 `assertion_id`（`(SELECT COUNT(*) FROM json_each(payload)) = 1`）；
     - `json_type(payload,'$.assertion_id') = 'text'`；
     - `LENGTH(CAST(payload AS BLOB)) <= 256`。

     现有写法本来就只写这一个键。
3. **豁免有结构上界，按「生效次数」证明。**
   - `forget` 只在断言当前有效时才写撤回事件；对已经失效的断言，它返回 `AlreadyForgotten`，不写事件（`assertion.rs:42–66`）。
   - 一条断言要从失效重新变为有效，唯一的生产路径是重申。重申一定会追加一行 A 类事件（`writer.rs:671–709`）。除此之外，没有其他生产路径能写 `assertion.retracted`，也没有其他路径能复活断言。
   - 因此每一行豁免撤回，都唯一对应它之前的一次生效（首次断言或重申），而每次生效都写过至少一行 A 类事件。
   - 结论：**豁免撤回的行数，不超过历史上写入的 A 类生效事件数**，后者每一行在写入时都通过了 A 类门。字节数不超过 256 × 该行数。
   - 这个上界跟「当前」的 `max_rows` 无关。D8 把限额调低之后，豁免行累计超过新的 `max_rows` 是合法的，因为对应的生效行都是在旧限额下合法写入的。这一点要写进 T2。

   这是证明，不是估计值，由 T2 固定。
4. **用量计数分开。** `mem_owner_usage` 新增 `exempt_row_count` 和 `exempt_byte_count` 两列；A 类门只比较 A 类计数，豁免行不会挤占内容额度。告警（D6）同时报两组计数。
5. **模块不能写保留 kind。** 进程内和进程外两个模块写入入口都拒绝以下 kind：`assertion.*`、`mem.*`、`session.*`，以及将来的 purge 控制类 kind。返回类型化错误 `reserved_kind`。需要补两条伪造测试（T9）。
6. **purge 控制记录不进 `mem_events`。** ADR §5 已经要求「独立恢复控制存储」，所以 purge 的意图、fence 和世代记录存在独立的控制表或控制库，不受 `mem_owner_quota` 约束，空间由 P1-08 设计。本稿只确认一点：purge 不会因为记忆满额而写不下控制记录。

### D2 统一的类型化失败与各路径回执

新增错误码闭集：

| 码 | 含义 |
|---|---|
| `memory_quota_rows` | 行配额已满 |
| `memory_quota_bytes` | 字节配额已满 |
| `memory_disk_low` | 写前检查发现剩余空间不足 |
| `memory_disk_full` | SQLITE_FULL |
| `memory_reserved_kind` | 模块试图写保留 kind |
| `memory_internal` | 其他内部错误 |

对外只给出错误码、维度和 owner 类别（personal / module），不带分区键，也不带 Display 原文（修 V5）。

| 路径（facts 编号） | 失败时用户必须看到 |
|---|---|
| W1 chat 普通轮次 | `memory_receipt: "not_saved"` 加错误码；桌面在该轮显示「本轮未保存到记忆」 |
| W1 + W4 chat 显式「记住」 | 保持 `memory_receipt: "failed"`，新增 `memory_error_code`；如果 `append_turn` 成功、断言失败，回执为 `partial`，并写明「对话已保存，记忆未写入」 |
| W1 run 路径 | `memory.write_failed{code, dimension, owner_kind}`（类型化，不含原文）；run 结束事件带 `memory_persisted: false`；桌面和 TUI 必须消费并显示（P1-03 验收项） |
| W2 会话压实摘要 | 写入失败时发 `memory.write_failed{kind:"session.summary"}`，不只打日志；之后的上下文仍用完整原文，不依赖摘要 |
| W3 旧会话导入 | 导入失败时 `ensure_imported` 返回类型化错误；该会话标记「导入未完成」并可重试，不能当成空会话继续 |
| W5 撤回 `DELETE /memory/assertions/{id}` | 按 D1 不再因配额失败。磁盘满时返回 507 `memory_disk_full`，磁盘偏低时返回 507 `memory_disk_low`，其他错误保持 500 |
| W6 / W7 模块 | OOP 保持 `quota_exceeded`，加上维度；进程内改为类型化；保留 kind 返回 `memory_reserved_kind` |

模型口头说「记住了」不算回执（与 P1-10 一致）。界面以结构化回执为准，两者冲突时标出来。

### D3 失败留痕

第 1 版设计的 `mem.write_rejected` 是 `mem_events` 里的保留行，会和 D1 的计数问题纠缠在一起。第 2 版改成**独立表** `mem_write_rejections`：

- 字段：owner 类别、不透明 owner id、错误码、维度、被拒 kind、拒绝时的计数、时间、`suppressed_count`。不含正文。
- 不受 `mem_owner_quota` 约束。
- 自身容量有上限：每个 owner 最多 1000 行，满了就环形覆盖最旧的行。审计的完整性以可见窗口为准，这一点要写明。
- 限频：同一 owner、同一错误码，每 60 秒最多新增一行，被合并的次数累加到 `suppressed_count`。
- 它是审计，不是重试队列；系统不会自动重写被拒的内容。

### D4 会话缺口：只记录、对用户显式，暂不注入模型上下文

第 1 版提出用 HostSystem 来源把缺口说明注入后续上下文。这与 R05-5:72 冲突（HostSystem 当前唯一的写点是暂停通知），会动到来源契约，所以第 2 版不注入：

- 一轮写入失败后，在**独立的** `mem_session_gaps` 表里记录区间：`session_id`、`turn_from`、`turn_to`、错误码、`first_at`、`last_at`。相邻的失败轮次合并成同一个区间。这张表**不做环形覆盖**，每个连续缺口占一行，行数受会话轮次约束，随会话的 purge 一起清除，不受 `mem_owner_quota` 约束。
- `mem_write_rejections`（D3）按 owner 和错误码聚合，只供审计使用，它的环形覆盖不影响缺口展示。
- 用户可见：该轮回执（D2）是 `not_saved`；会话页在缺口处显示「此处 N 轮未保存」，数据来自 `mem_session_gaps`。
- 模型可见：**不处理**。后续上下文照现状不含失败轮次，也不插入说明。如果以后要让模型知道缺口，另开一份 R05 契约修订（R05-6）：定义唯一写点，来源与 digest 同一事务，并经过发送门和冷恢复测试。本稿不批准这件事。

### D5 磁盘

- **写前检查**：针对 `memory.db` 所在的卷（不是 cwd）。剩余空间低于 `max(512 MiB, 2 × 当前 WAL 大小)` 时，内容写入（A 类）返回 `memory_disk_low`。
- **撤回**：只是**尽力保证**，不做硬保证。撤回只写一行（≤256 B）加 WAL 增量，所以剩余空间不低于 16 MiB 时放行；低于 16 MiB 时返回 `memory_disk_low`，并提示用户先释放磁盘。同一卷上可能有并发写入（其他进程或其他库）在检查之后耗尽空间，这种情况会遇到 SQLITE_FULL，映射为 `memory_disk_full`。本稿不承诺在磁盘完全写满时还能撤回。
- **缓存**：检查结果最多缓存 5 秒，A 类写入在缓存过期时重新读取。缓存只用于减少 statfs 调用，不作为安全依据；安全依据是 SQLITE_FULL 的映射。
- **SQLITE_FULL（code 13）**：映射为 `memory_disk_full`，可以参照 `agent24-documents/src/state.rs` 的现有映射。
- Windows 卷剩余空间的读取方法交给 PLT-WIN。

### D6 告警阈值（供 P1-03 实现）

- **按 owner 告警**：A 类用量（行或字节）达到 80% 为 `warning`，达到 95% 为 `critical`；回落到 75% / 90% 以下时解除（滞回）。豁免撤回计数单独展示，不触发配额告警。
- **磁盘告警**：剩余空间低于 2 GiB 为 `warning`，低于 D5 门槛为 `critical`。
- **通道**：用量 API（P1-03a），以及 `memory.usage_level_changed` 事件（只在跨档时发出）。
- **阈值放在配置里**：测试用小配额 owner 覆盖跨档和解除。

### D7 不自动回收；满额后的出路

- 到限后不删数据、不压缩原文、不自动扩大限额。
- 用户可用的出路（按交付顺序）：
  1. **撤回**（D1 保证可用）：撤回不释放空间，但让内容不再被召回。
  2. **管理员调整限额**（D8）。
  3. **冷归档**（P1-04b）：校验并回放等价之后，才切换引用、回收热副本。
  4. **purge**（P1-08）。
- **WAL**：允许在空闲时执行 `wal_checkpoint(TRUNCATE)`，限制 WAL 长度，不删除逻辑数据（c5）。
- **VACUUM / incremental_vacuum**：只在 purge 或归档回收之后由对应流程触发，本稿不新增。

### D8 owner 限额调整

- 新增 owner 级配额覆盖的写入口（API / CLI），属于 ADR-037 的提升级操作，需要用户在场证明。
- 只能设为绝对值，不能设为「无限」。调低到当前用量以下时不删数据，只让后续 A 类写入被拒。
- 每次调整都写入 `mem_write_rejections` 的同类审计表 `mem_quota_changes`（旧值、新值、不透明操作者 id、时间），不受配额约束。

### D9 先测量再调参

`P1-02c-0`：在隔离的合成库上运行现有 `p1_capacity` 探针，并扩展到真实的 personal 写入路径（chat、retain）。记录 `page_count × page_size`、WAL 峰值、`mem_events` 平均 payload、每千轮对话的行数和字节。结果写入 `docs/research/`，用来复核 D6 阈值和 D5 门槛。**默认限额不因测量结果自动改变。**

## 3. 测试门（每片 red → green，并做变异验证）

| 门 | 断言 | 片 |
|---|---|---|
| T1 满额撤回 | 把 personal owner 的配额设为当前用量，撤回成功、断言失效；去掉豁免分支后测试变红 | c1 |
| T2 豁免上界 | 对同一条断言反复「重申 → 撤回」，直到 A 类配额满：重申先被拒，此后没有可撤回的有效断言；豁免行数不超过 A 类生效事件数；D8 调低限额之后豁免累计超过新 `max_rows` 仍合法；以下 payload 都按 A 类门处理：多键、非字符串、超过 256 B、owner 只是前缀相似但不在 personal 目录里 | c1 |
| T3 内容仍拒 | 满额后新增消息、断言或重申被拒，`mem_events` 没有新增内容行；`mem.write_decision` 与断言一起回滚 | c1 |
| T4 计数分离 | 豁免撤回不增加 A 类计数，A 类门不受影响 | c1 |
| T5 回执 | W1–W7 各路径在满额时都给出类型化失败回执；run 结束事件 `memory_persisted:false`；`partial` 组合（轮次已存、断言未存）有专门用例；任何回执都不出现「已记住」 | c2 |
| T6 不泄露 | 失败事件和 HTTP 错误体里不含分区键、`{owner:?}` 转义文本或 Display 原文 | c2 |
| T7 失败留痕 | 拒绝时 `mem_write_rejections` 有一行；60 秒内重复拒绝只累加 `suppressed_count`；超过 1000 行时环形覆盖；环形覆盖之后，`mem_session_gaps` 里的缺口区间仍然完整，相邻失败会被合并 | c3 |
| T8 磁盘 | 模拟卷剩余不足：A 类返回 `memory_disk_low`，剩余 ≥16 MiB 时撤回成功、<16 MiB 时返回 507；注入 SQLITE_FULL 映射为 `memory_disk_full`（A 类和撤回都测）；缓存过期后重新读取 | c4 |
| T9 模块伪造 | 进程内、进程外模块入口都无法写 `assertion.retracted` / `mem.*` / `session.*`（返回 `memory_reserved_kind`）；模块 owner 的撤回不享受豁免 | c1 |
| T10 分区隔离 | 模块 owner 写满不影响 personal 的写入和撤回 | c1 |
| T11 限额调整 | 没有在场证明时拒绝；有证明时写入审计；调低到用量以下时不删数据 | c6 |

## 4. 实现分片（批准后再派，各自独立 PR）

| 片 | 内容 | 依赖 |
|---|---|---|
| c0 | D9 测量报告（只读、合成库） | — |
| c1 | 迁移 0024：撤回豁免（personal、宿主写点、payload 上限）、豁免计数列、模块保留 kind 拒绝；T1–T4、T9、T10 | — |
| c2 | 类型化错误码与 W1–W7 回执、`memory_persisted`、`partial`；T5、T6 | c1 |
| c3 | `mem_write_rejections` 表（限频、环形上限）与 `mem_session_gaps` 区间表；T7 | c1 |
| c4 | 磁盘写前检查与 SQLITE_FULL 映射；T8 | c2 |
| c5 | 空闲时 `wal_checkpoint(TRUNCATE)` | — |
| c6 | owner 限额调整入口（提升级）与 `mem_quota_changes`；T11 | c1；ADR-037 在场证明（CORE-2） |

告警与用量 API 属于 P1-03。会话缺口注入模型上下文属于另行开的 R05-6。

## 5. 不在本稿

- 用量 API 与记忆页 UI（P1-03）；附件外置（P1-04a）；冷归档（P1-04b）；purge 控制存储的具体设计（P1-08）。
- 修改默认的 200000 行 / 256 MiB。
- 跨 owner 的全局上限：模块数量增长时，总量会叠加。这一条记为风险，等 P1-03 告警上线后再决定。

## 6. 修订记录

**第 3 版**，回应第二轮挑战（无 High，3 条 Medium）：
- D1.3 的上界改为按「生效次数」证明，并说明限额调低后的情况。
- D1.2 判断 personal owner 改为精确关联分区目录，payload 四项校验。
- 会话缺口改用独立的 `mem_session_gaps` 区间表，不做环形覆盖。

**第 2 版**，回应 Mac mini Codex 第一轮挑战：

- **H1**（模块可冒充控制 kind）：只豁免宿主撤回、只对 personal owner 豁免，模块入口拒绝保留 kind（D1.2、D1.5，T9）。
- **H2**（`mem.write_decision` 含正文，可绕过配额门）：成功审计保持 A 类，与内容同一事务（D1.1，T3）。
- **H3**（共享保留池在正常使用下会耗尽，挤占内容额度，也证明不了 purge 有空间）：取消保留池，改为撤回豁免加结构上界的证明（D1.3），计数分离（D1.4），purge 控制记录放到 `mem_events` 之外（D1.6），失败留痕改用独立表并设环形上限（D3）。
- **M1**（HostSystem 唯一写点冲突）：会话缺口不注入模型上下文，留给 R05-6（D4）。
- **M2**（回执遗漏 W2/W3、撤回磁盘满仍返回 500、门禁顺序）：补齐回执表；撤回的磁盘错误改为 507；T3 不再依赖 c3。
- **M3**（64 MiB 加 5 秒缓存保证不了撤回）：撤回改为尽力保证，写明低于门槛时的行为，并补并发和 SQLITE_FULL 测试（D5，T8）。
- **L1**：V5 的措辞改为「`{owner:?}` 以转义文本出现」。
