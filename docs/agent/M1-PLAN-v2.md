# M1 v2 —— 记忆成为产品（设计草案，待 jason 拍板）

> 状态：**草案，未冻结**（2026-10-01）。冻结后由 A（笔记本）切成 task 交给 B（Mac mini ab-codex），集成分支 `ab/m1-memory`。
> 取代对象：`docs/agent/tasks.md` 的「M1 —— 记忆成为产品（2026-08-23 规划）」F1.1–F1.3 / T1.1.1–T1.3.3。冻结前 tasks.md 不改。
> 依据：`roadmap.md` M1、`architecture.md`、ADR-030（`docs/decision.md`）、`docs/specs/SPEC-ME-FOLLOWUPS.md` F2，以及 2026-10-01 `origin/main` 的代码核对（下节）。

## 0. 一句话

**Agent24 跨会话记得你，你看得见、改得了、删得掉它记得什么；对话本身进入 EventLog，崩溃后能原样重放。**

v1 只做到「底座接上线」（判定接缝 + personal space + 会话进 EventLog），交付后用户**感知不到任何变化**。v2 保留这条地基，然后在上面加两层用户能摸到的东西：**跨会话召回** 和 **记忆可见可控**。

## 1. 2026-10-01 代码核对：v1 哪些还成立

| v1 的前提 | 现状 | 对设计的影响 |
|---|---|---|
| M-D 12 个模块 crate 外引用为 0（F2） | 仍成立：`condenser/assertion/retriever/writer/consolidator/vector/knowledge/replay/trace/eval/artifact` 外部引用全为 0；只有 `event`（ME-3 的 `os_memory.rs`/`os_memory_page.rs`）被用上 | F2 的判断不变 |
| agent loop 用 `CanonicalSession` 存 KV blob | 仍成立：`agent24-agent/src/lib.rs` `SessionMemory::{session_context, remember_exchange}` → `CanonicalSession::load/save(kv, sid)`，按 **session id** 存、**没有 owner**，跨会话零召回 | ADR-030 硬门槛 3 仍未解决 |
| 判定点只需接 `MemoryLease::lend` | **仍成立**：ME-3 的 OOP 挂载（`domain.rs` ≈1753）和进程内挂载（≈1348）**都走 `lend`**；`memory_callback.rs` 只消费已借出的 `OsScopedMemory`，不另发句柄 | v1 T1.1.2 的接线范围正确，不用扩；但验收要覆盖**两条**挂载路径 |
| 迁移 `0014_personal_space.sql` | **编号已被占**（`0014_owner_usage_quota`，最新 `0015`） | 新迁移从 **0016** 起 |
| `mem_os_partitions` 可直接登记 personal 分区 | `module_name TEXT NOT NULL`（0012），personal 分区没有模块 | 需要 0016 让目录能表达「非模块分区」（见 T2） |
| `authz.rs` | 不存在 | T1 新建，签名照 `architecture.md`「契约 / 接口」 |
| 用户可见的记忆入口 | 无：无 `/api/v1/memory*` 路由，桌面端无记忆页 | v2 新增 F4 |

## 2. Feature / Task

记号：**[B]** = 可在 Mac mini（无本地模型推理）上做；**[笔记本]** = 需要 oMLX/本地模型推理，留笔记本。所有 task ≤300 行（不含生成文件），验收判据必须先失败一次（反面对照）。

### F1 判定接缝 + personal space（地基，对应 v1 F1.1/F1.2）

**T1 `Authorizer` 契约 + 接进 `lend`** [B] · 依赖：无 · 约 200 行
- 目标：句柄发放经过判定点，行为零变化。
- 范围：新建 `rust/apps/agent24d/src/authz.rs`，类型逐字照 `architecture.md`「契约 / 接口」（`Actor/Op/AccessRequest/Decision/Authorizer`）；默认实现 `ModulePrivateOnly`（`allow ⟺ space == SpaceId::module_private(module)`）；`MemoryLease` 持有 `Arc<dyn Authorizer>`，`lend` 在 `ensure_recorded` **之前**判定，deny → 不借、`tracing::warn!` 带 `reason`。
- 不做：不改判定逻辑；不进 `agent24-domain`；不新增存储。（v1 把这件事拆成 T1.1.1+T1.1.2，合并成一片：纯新增 + 一处接线，拆开的那片没有可验证的行为。）
- 验收：`cd rust && cargo test -p agent24d authz && cargo test --workspace`
  - 自有空间 allow / 他模块空间 deny / `reason` 非空；
  - **变异落点**：注入恒 deny 的 `Authorizer` 时，**进程内与 OOP 两条挂载路径**的模块都拿不到 memory 能力（两条测试）；
  - F1/F8 既有跨模块隔离探针全部继续通过。
- 文件：`apps/agent24d/src/{authz.rs,main.rs,domain.rs}`

**T2 `SpaceId::personal` + 目录登记（迁移 0016）** [B] · 依赖：无（与 T1 并行）· 约 250 行
- 目标：agent loop 的记忆在空间模型里有名字、有目录行，且不可能与模块空间相撞。
- 范围：`SpaceId::personal(user) -> "usr:<user>"`；`0016_personal_partition.sql` 让 `mem_os_partitions` 能登记**非模块分区**（推荐：加 `space_kind TEXT NOT NULL DEFAULT 'module' CHECK (space_kind IN ('module','personal'))`，personal 行 `module_name` 填保留值 `@agent`——模块名只允许 `[a-z0-9][a-z0-9_-]*`（`agent24-domain/src/lib.rs` ≈533），`@agent` 不可能撞上真实模块）；`OsMemoryCatalog::ensure_personal_recorded(org, user)` 返回 personal 分区的 key。
- 不做：不搬任何已有数据（T4 做）；不 bump `KEY_VERSION`；**不在 SQL 里算 key**（0013 的教训：SQLite `length()` 数字符不数字节）。
- 验收：`cargo test -p agent24d space && cargo test -p agent24-memory migration_0016`
  - `usr:` 与 `os:` 不相交：**扫小的交叉积**（照 F8 `the_partition_key_is_versioned_and_unambiguous`），不是两个手挑例子；
  - 用 `pool_migrated_up_to(&path, 15)` 建 0015 态库 + 若干模块分区行 → 跑 0016 → 原有行 `space_kind='module'` 且 key 不变；
  - 同一 (org,user) 重复 `ensure_personal_recorded` 得到同一 key，目录只有一行。

### F2 对话进入 EventLog（对应 v1 F1.3 / SPEC F2）

**T3 会话轮次写进 EventLog（影子写）** [B] · 依赖：T2 · 约 250 行
- 目标：情节权威里第一次有对话。
- 范围：`remember_exchange` 在现有 `CanonicalSession` 保存之外，追加两条 `MemEvent`（`kind=chat.user`/`chat.assistant`，`Trust::UserSaid`/`Trust::Model`，`Scope{owner=personal key, session=sid, run=run_id}`），用 `replay::message_event` 构造；写失败只 warn，不影响 run。
- 不做：不改读路径（T4）；不动压缩。
- 验收：`cargo test --workspace`；一次模拟 3 轮对话后，`EventLog` 按 seq 读回 6 条，`replay::messages_from_events` 与写入的消息**逐条相等**；反面对照：把写入注释掉该测试必红。

**T4 读路径切到 EventLog + `Condenser`，`CanonicalSession` 降级为投影** [B] · 依赖：T3 · 约 300 行（超了就拆 T4a 读切换 / T4b 旧 blob 导入）
- 目标：一份会话权威（EventLog）、一份压缩实现（`Condenser`），不两套并存（`architecture.md` 核心判断 3）。
- 范围：`session_context` 改为「读本会话事件 → `replay` → `Condenser`（`LlmSummaryCondenser`，summarizer 仍走现有 `RouterSummarizer`）投影」；**摘要结果也落成事件**（`kind=chat.summary`，payload 带 `covers` 到的 seq），下次投影从最近一条 summary 起算，避免每次 run 重新摘要（开放问题 Q4）；`CanonicalSession::save` 不再被 agent loop 调用；**旧会话**：首次读到一个只有 KV blob、没有事件的 session 时，在一个事务里把 blob 的 `summary+recent` 导入为事件（`origin=migration`），之后只读事件（开放问题 Q1）。
- 不做：不新增压缩策略；不删 `CanonicalSession` 类型（降级为投影，供测试/导入用）。
- 验收：`cargo test --workspace`
  - **no-loss**：摘要器必然失败时消息不丢，下次重试（照搬 `session.rs` 的 `FailingSummarizer` 测试形状到新路径）；
  - 长对话压缩后 `covers(n)` 与实际覆盖轮次一致；
  - 旧 blob 会话首读后事件条数 = blob 中消息条数 + (summary? 1:0)，再读一次**导入 0 条**（幂等）；
  - 若 no-loss 在 `Condenser` 下无法等价成立 → 标 `BLOCKED` 写进 progress.md，**不得放宽保证**（v1 原话保留）。

**T5 崩溃重放对真实会话生效** [B] · 依赖：T4 · 约 120 行
- 范围：端到端测试：经 agentd 的 run 路径写入对话 → 丢弃内存态、重开 `KvStore` → 下一 run 的上下文与崩溃前**逐条相等**。
- 验收：`cargo test --workspace replay`；反面对照：让 T4 的读路径退回 KV blob，测试必红。

### F3 跨会话召回（v2 新增，用户第一次能感知）

**T6 Retain：显式记忆写入断言账本** [B] · 依赖：T2, T3（provenance 指向 chat.user 事件）· 约 250 行
- 目标：用户说「记住……」/「remember …」时，形成一条跨会话的持久断言。
- 范围：规则触发（不调模型）：`Trust::UserSaid` 的用户消息命中显式记忆句式 → `WriteGate` 判定 → `AssertionLedger` 写入 personal 空间（`Modality` 用现有枚举，provenance 指向源事件 seq）；同义更新走账本已有的 supersede 语义。
- 不做：**不做 LLM 自动抽取**（T6b，[笔记本]，见 Q2）；不做 consolidator。
- 验收：「记住我对花生过敏」→ 账本 1 条、provenance 指向 chat.user 事件；`WebFetch`/`Model` 来源的同句式**不写入**（写闸门承重：去掉 trust 判断测试必红）。

**T6b LLM 抽取偏好/事实** [笔记本] · 依赖：T6, T10 · 规模另估
- 用本地模型（oMLX）从对话抽取候选，进同一个 `WriteGate`；用 T10 的评测集量化收益。M1 内可选，不阻塞发布。

**T7a 中文可检索：断言 FTS 加 CJK bigram 影子索引** [B] · 依赖：无（与 T1/T2 并行）· 约 200 行
- 问题（2026-10-01 核对）：`mem_assertions_fts` 用 `tokenize='unicode61'`（0005），它不切分 CJK——「我对花生过敏」整串是**一个** token，查「过敏」**召不回**。`FtsRetriever` 现有测试全是英文，没暴露这点。trigram tokenizer 也不行（2 字中文词查不到）。
- 范围：迁移 0017 给 FTS 加一列应用层预处理的影子文本（CJK 连续段切成重叠二元组，非 CJK 原样），`FtsRetriever::rebuild/写入/查询` 用同一个纯函数处理；`rebuild` 仍可从账本确定性重建。
- 验收：`cargo test -p agent24-memory retriever`；「我对花生过敏」可被「过敏」「花生」各自命中；英文既有用例全过；反面对照：去掉影子列查询，中文用例必红。

**T7 Recall：run 前召回并注入上下文** [B] · 依赖：T3, T6, T7a · 约 250 行
- 目标：新会话里问「我对什么过敏？」能答出来。
- 范围：每次 run 前，用 `FtsRetriever` 在 **personal 空间的断言账本**里检索（`FtsRetriever` 只索引断言，不索引对话事件；对话事件的全文召回不在 M1），取 top-k，按 token 预算插成一段 `ContextFragment`（标明来源 assertion id，供审计）；召回**只读 personal 空间**，绝不读模块空间（开放问题 Q3）。
- 不做：向量召回（T7b，[笔记本]：真实 `Embedder` 走 oMLX；`HashEmbedder` 只能做管线测试，不代表质量）；历史对话事件召回。
- 验收：会话 A 写入「记住我对花生过敏」→ 新会话 B 的模型请求里出现该断言（断言发给 mock provider 的 messages）；会话 B 召回结果里**没有**任何 `os:*` 分区的断言（往模块分区写同关键词作反面对照）；预算为 0 时不注入。

**T7b 向量召回** [笔记本] · 依赖：T7 · 规模另估

**T10 召回评测基线** [B] · 依赖：T7 · 约 200 行
- 范围：用现有 `eval.rs`（LongMemEval 形状）做一个仓内小样本集（≤20 例，中文为主，手写），`cargo test` 里跑 FTS 基线，输出命中率；阈值先**记录不设门**（原型阶段），作为 T6b/T7b 的对照。

### F4 记忆可见、可控（v2 新增）

**T8 记忆 REST：列出 / 搜索 / 遗忘** [B] · 依赖：T6 · 约 300 行
- 范围：`GET /api/v1/memory/assertions?q=`、`DELETE /api/v1/memory/assertions/{id}`（**遗忘 = 追加撤回事件 + 账本标记失效**，不物理删；物理擦除走现有 export/erase 路线，不在 M1）；`protocol/openapi.yaml` + `pnpm gen:api`；只暴露 personal 空间。
- 验收：openapi lint + codegen 无 diff；删除后 T7 召回不再返回它（跨 T7 的端到端）；对模块分区的 id 返回 404 而不是 403（不泄露存在性）。

**T9 桌面端「记忆」页** [B] · 依赖：T8（可先按 openapi 契约 mock 并行开工）· 约 300 行
- 范围：设置页旁新增「记忆」：列表 + 搜索 + 遗忘（二次确认）；空状态说明「说『记住……』来让我记住」。
- 验收：`pnpm typecheck && pnpm lint && pnpm test`；组件测试覆盖：列表渲染、遗忘调用 DELETE 后从列表消失、请求失败显示错误不清空列表。

### 依赖图与并行

```
T1  ───────────────────────────── (独立，可随时合)
T7a ───────────────┐                (独立)
T2 ── T3 ─┬─ T4 ─ T5
           └─ T6 ─┬─ T7 (+T7a) ─ T10 ──(笔记本) T6b / T7b
                  └─ T8 ─ T9（T9 可在 T8 契约定后并行）
```

B 上最多同时 2–3 条线：`T1 ∥ T2 ∥ T7a` → `T3` → `T4 ∥ T6` → `T5 ∥ T7 ∥ T8` → `T10 ∥ T9`。

**M1 v2 完成判据**：T1–T5、T6、T7a、T7、T8、T9、T10 全部合进 `ab/m1-memory` → 笔记本上真实 agent24d + 桌面端走一遍「会话 A 记住 → 重启 daemon → 会话 B 召回 → 记忆页遗忘 → 会话 C 不再召回」→ 开 release PR `ab/m1-memory → main`（人工）。T6b/T7b 不阻塞。

## 3. 需要 jason 拍板的开放问题

1. **旧会话怎么处理**（T4）——推荐 **首读时惰性导入为事件**（一个事务、幂等）。备选：不迁移，旧会话从空上下文重新开始（更简单，但用户会觉得「它忘了」）。
2. **Retain 的触发方式**——推荐 **M1 只做显式「记住……」+ 规则**（B 上能做、可预测、可测），LLM 自动抽取作为 T6b 在笔记本上用本地模型做、不阻塞 M1。备选：M1 就上 LLM 抽取（质量更高，但依赖本地模型、难评测、易误记）。
3. **召回范围**——推荐 **只召回 personal 空间**，模块（Sin90/Cos72）的记忆永远不进 agent loop 的上下文，要用就走模块自己的 API。备选：用户授权后召回模块空间（需要 ADR-030 的 grant，M1 不具备）。
4. **摘要是否落成事件**（T4）——推荐 **落成 `chat.summary` 事件**（重放可复现、不重复花模型钱）。备选：每次投影现算（实现简单但每轮都调摘要模型）。
5. **AgentEar 语音转写的摄入接口**——推荐 **M1 不做**，M1 冻结后把 `chat.*` 事件 + `AssertionLedger` 的写入形状同步给 AgentEar（agentear-73），摄入接口放 M2。备选：M1 加一个 `POST /api/v1/memory/ingest`（text, origin=voice）。

## 4. 明确不在 M1

shared space / grant / group（ADR-030 F9）· 物理擦除与导出 UI · consolidator/insight 合成 · knowledge/instruction store · 多用户 · 跨设备同步 · 配额 UI（配额本身 0014 已有）。
