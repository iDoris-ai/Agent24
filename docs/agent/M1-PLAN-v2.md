# M1 v2 —— 记忆成为产品（**已冻结 2026-10-01**）

> 状态：**已冻结 2026-10-01**。jason 拍板：开放问题 Q1–Q5 全部按推荐项（§3）；设计评审 1 轮（B 端 Codex gpt-6-astra，REQUEST_CHANGES 7H/5M/1L）后按 §6 处置表修订，**不再复审**。
> 执行：~~A（笔记本）派 task，B（Mac mini ab-codex）执行~~ —— **2026-10-03 更正（jason 裁决）：Agent24 的派活、构建、测试、验收全部在笔记本上做，B 机不再构建 Agent24**；下文的 `[B]` 标记一律作废（仅保留为历史记号）。集成分支 `ab/m1-memory`，task 分支 `ab/m1-memory-NN-<短名>`，门禁见仓库根 `AGENTS.md`。
> 范围补记（jason 2026-10-03）：SPEC-ORG-SPACE 的 **F11（`asserted_by` + 冲突断言并存）不属于 P0 / M1**，移入 P1（C3 P1，见 `docs/research/MEMORY-STRATEGY.md` §4.2 / §7）；这是对 SPEC-ORG-SPACE「F11 随 F2」的已记录偏离。
> 取代：`docs/agent/tasks.md`「M1 —— 记忆成为产品（2026-08-23 规划）」F1.1–F1.3 / T1.1.1–T1.3.3。台账见 tasks.md「M1 v2 台账」。
> 依据：`roadmap.md` M1、`architecture.md`、ADR-030、`SPEC-ME-FOLLOWUPS.md` F2、2026-10-01 代码核对（§1）。

## 0. 一句话（已按评审收窄）

**Agent24 跨会话记得你明确让它记住的事；你能查看、搜索、撤回它记得什么。对话原文完整进入本地事件日志，崩溃/重启后原样重放，从此不再因压缩而删除原文。**

明确**不**承诺：编辑记忆（只能撤回后重说）、自动从对话里抽取事实（T06b，笔记本，可选）、物理擦除（撤回 = 失效 + 撤回事件，原始对话仍保留在本地事件日志）、找回 M1 之前已被旧压缩删掉的原文（§2 F2 恢复边界）。

## 1. 2026-10-01 代码核对

| 事实 | 出处 | 对设计的影响 |
|---|---|---|
| 句柄发放只经 `MemoryLease::lend`：进程内挂载与 OOP 挂载两条生产路径都调它；`memory_callback.rs` 只消费已借出的句柄 | `domain.rs` ≈1348 / ≈1753 | T01 接线范围正确，验收覆盖两条路径 |
| `architecture.md` 契约里的 `ActiveScope<'a>` 代码中不存在 | `architecture.md:66` | T01 给最小占位定义（§2 T01） |
| 最新记忆迁移 0015；`mem_os_partitions.module_name` NOT NULL 且禁空白 | `migrations/0015…sql:16` | T02 用 0016；T07a 用 0017 |
| `pool_migrated_up_to(path, n)` 跑的是 `version < n` | `agent24-memory/src/lib.rs:1116` | 建 0015 态库要传 **16** |
| agent loop 会话记忆 = `CanonicalSession` KV blob，按 session id 存、无 owner；**摘要成功即删原文**、摘要持续失败超过 4×硬上限也删原文 | `session.rs:154`；`agent24-agent/src/lib.rs:440-457` | F2 改为事件权威，原文永不删除；旧数据恢复边界见 F2 |
| 会话写入在 run 终结前、受 `MEMORY_WRITE_BUDGET` 超时约束，失败只 warn | `agent24-agent/src/lib.rs:1068` | F2 定义显式失败语义 |
| `EventStore::append` 按 `id` 幂等；`EventLog::append_tx`（crate 内）可与其它写同事务；0014 配额触发器会真实拒绝写入 | `event.rs:253/447`；`0014_owner_usage_quota.sql:82` | F2/F4 的原子写都在 `agent24-memory` 内用 `append_tx` 实现 |
| replay 只认 `kind="message"`（body = `Msg`），其它 kind 跳过 | `replay.rs:46/100/143` | F2 对话事件沿用 `message`，元数据事件用别的 kind（replay 自然跳过） |
| `LlmSummaryCondenser` 每次 `summarize(None, 全部头部)`；`ContextFragment.source` 是数组下标不是 seq；`covers()` 只证明下标覆盖 | `condenser.rs:27/76/270` | F2 不用 `LlmSummaryCondenser`，直接用 `Summarizer` + 显式 `covered_through_seq` 契约 |
| FTS 用 `unicode61`，CJK 整串一个 token；索引写入靠 SQL 触发器 `mem_assertions_fts_ai`；`to_match_query` 隐式 AND | `0005_assertions_fts.sql:17-20`；`retriever.rs:88` | T07a 换写入接缝、重建虚表 + Rust 回填、新增 OR 检索入口 |
| `Assertion.evidence: Vec<EventId>`；`WriteGate`：`UserSaid+explicit_remember→Commit`，`Model/ToolOutput→Hold（持久化但不召回）`，`WebFetch→Reject`；`insert_tx` 不处理 supersede | `assertion.rs:78/192`；`writer.rs:156/245` | T06 用 EventId 做 evidence；同义更新推迟 |
| `retract` 是独立 UPDATE，不写事件 | `assertion.rs:284` | T09 新增同事务 `forget` |
| 无 `/api/v1/memory*` 路由、无桌面记忆页 | — | F4 |

## 2. Feature / Task（冻结）

记号：~~**[B]** = Mac mini 执行~~（2026-10-03 作废，全部改在笔记本执行）；**[笔记本]** = 需 oMLX/本地模型。每片 ≤300 行（不含生成文件/锁文件）；每条验收判据**先在修复前失败一次**（反面对照），PR body 写明怎么证的。验收命令都在 `rust/` 下，最后必须 `cargo test --workspace` 全绿（AGENTS.md 门禁）。

### F1 判定接缝 + personal space

**M1-T01 `Authorizer` 契约 + 接进 `lend`** [B] · 依赖：无 · ≈200 行 · **DONE（PR #636）**
- 目标：句柄发放经过判定点，行为零变化。
- 范围：新建 `apps/agent24d/src/authz.rs`：`Actor/Op/AccessRequest/Decision/Authorizer` 按 `architecture.md`「契约 / 接口」；`ActiveScope` 用最小占位（已编译验证）：
  ```rust
  /// 请求级作用域。M1 恒为 None；占位以保持 architecture.md 的签名，不承载数据。
  pub struct ActiveScope<'a> { _p: std::marker::PhantomData<&'a ()> }
  ```
  默认实现 `ModulePrivateOnly`（`allow ⟺ space == SpaceId::module_private(module)`）；`MemoryLease` 持有 `Arc<dyn Authorizer>`，`lend` 在 `ensure_recorded` 之前判定，deny → 不借、`tracing::warn!` 带 `reason`。
- 不做：不改判定逻辑；不进 `agent24-domain`；不新增存储。
- 验收：`cargo test -p agent24d authz`：自有空间 allow / 他模块空间 deny / `reason` 非空；**注入恒 deny 的 Authorizer 时，进程内与 OOP 两条挂载路径的模块都拿不到 memory 能力**（两条测试，变异落点）；既有跨模块隔离探针全过。
- 文件：`apps/agent24d/src/{authz.rs,main.rs,domain.rs}`

**M1-T02 `SpaceId::personal` + 目录登记（迁移 0016）** [B] · 依赖：无 · ≈250 行 · **DONE（PR #639）**
- 范围：`SpaceId::personal(user) -> "usr:<user>"`；`0016_personal_partition.sql`：`mem_os_partitions` 加 `space_kind TEXT NOT NULL DEFAULT 'module' CHECK (space_kind IN ('module','personal'))`，personal 行 `module_name='@agent'`（合法模块名 `[a-z0-9][a-z0-9_-]*` 不可能撞上）；`OsMemoryCatalog::ensure_personal_recorded(org, user) -> 分区 key`。
- 不做：不搬数据；不 bump `KEY_VERSION`；不在 SQL 里算 key。
- 验收：`cargo test -p agent24d space && cargo test -p agent24-memory migration_0016`
  - `usr:`/`os:` 不相交：扫小的交叉积（照 `the_partition_key_is_versioned_and_unambiguous`）；
  - **（评审补充）** 用 `pool_migrated_up_to(&path, 16)` 建 **0015 态**库，先断言迁移版本 = 15；插若干模块分区行（含 `last_seen_at=NULL`）→ 跑 0016 → 原有行 `space_kind='module'`、key/`last_seen_at`/外键/唯一约束都不变；
  - 同一 (org,user) 重复 `ensure_personal_recorded` 同一 key、目录一行；
  - 迁移失败回滚 + 重开测试（注入失败后库仍是 0015 态、可重开）。

### F2 对话以事件日志为权威（取代 v1 F1.3 / 草案 T3–T5）

**设计决定（处置 High 1–4）：**
1. **不设影子写阶段。** 一次切换：同一个版本里「旧会话导入 + 读 + 写」全部改到事件日志。这样不存在「旧 blob + 新事件」的混合态（High 1 的根因）。
2. **事件格式**：每条对话消息一条 `kind="message"` 事件（body = `Msg`，与 `replay` 兼容）；`Scope{owner=personal key, session=sid}`；`Origin` 用户消息 `Trust::UserSaid`、助手消息 `Trust::Model`。元数据用另外两个 kind：`session.summary`（body `{summary, covered_through_seq}`）、`session.imported`（body `{from:"kv", messages, had_summary}`），replay 自然跳过它们。
3. **幂等键**：消息事件 id = `sha256(owner ‖ session ‖ turn_no ‖ role)` 的十六进制；`turn_no` = 本会话已有 user 消息事件数（在每会话锁内计算，单写者）。重试同一轮同内容 → 幂等；同 id 不同内容 → 冲突错误（不吞）。
4. **原子性**：一轮的 user+assistant 两条事件在**一个事务**里 `append_tx`；导入的全部事件 + `session.imported` 标记在**一个事务**里；任一条失败（含配额触发器拒绝）整体回滚。
5. **失败语义（处置 High 2）**：写入失败（I/O、配额、超时）**不再静默**：run 仍以 completed 结束（答案已给出），但发 `memory.write_failed` 事件（带 session、原因），`tracing::error!`；该轮不在记忆里，下次 run 的上下文看不到它——这是显式、可观测的失败，不是数据损坏。**不做自动补写**（可从 run 的 durable thread 重建，记为后续 FU，M1 不做）。
6. **摘要契约（处置 High 4）**：视图 = 最新一条 `session.summary`（若有）+ 本会话 `seq > covered_through_seq` 的全部 `message` 事件（按 seq）。折叠：未覆盖消息数 > `max_recent` 时，`summarize(上一条 summary, 未覆盖的头部)`（头部 = 除最后 `keep_recent` 条外，且不拆开 tool_calls/tool 结果对，沿用 `CanonicalSession::append` 规则），成功则追加新 summary 事件，`covered_through_seq` = **被折叠的最后一条消息事件的 seq**（不是 summary 事件自己的 seq）。不用 `LlmSummaryCondenser`。
7. **no-loss（处置 High 3）**：原文事件**永不删除**。摘要失败 → 不写 summary，消息全部留在视图里，下次再试；超过旧硬上限（4×`max_recent`）时**只截断视图**（取最新的 4×`max_recent` 条进上下文，`tracing::error!`），不删任何事件。
8. **旧数据恢复边界（处置 High 1/3）**：首次读写某个 session 时（在每会话锁内），若该 session 在事件日志里没有任何事件、KV 里有 blob → 一个事务导入 blob 的 `summary`（作为 `session.summary`，`covered_through_seq` = 导入前的最大 seq 占位 0）+ `recent`（逐条 `message` 事件，origin `migration`）+ `session.imported` 标记。**M1 之前已被旧压缩删除的原文无法恢复**，导入的只是 blob 当时还保有的 summary + recent。导入后 KV blob 不再更新；**不支持降级回旧版本**（旧版本会读到停更的 blob），release note 写明。

**M1-T03 `SessionLog` 存储接口（agent24-memory）** [B] · 依赖：无 · ≈280 行 · **现在可入队**
- 范围：`agent24-memory/src/session_log.rs`，签名（已在 crate 内编译验证）：
  ```rust
  pub struct SessionLog { pool: sqlx::SqlitePool }            // KvStore::session_log() 取得
  pub struct TurnIds { pub user: EventId, pub assistant: EventId }
  pub enum ImportOutcome { Imported { events: usize }, AlreadyImported, NothingToImport }
  pub struct SessionView { pub summary: Option<String>, pub covered_through_seq: i64, pub tail: Vec<(i64, Msg)> }
  impl SessionLog {
      pub async fn append_turn(&self, owner: &str, session: &str, user: &Msg, user_origin: Origin, assistant: &Msg, assistant_origin: Origin) -> Result<TurnIds>;
      pub async fn import_legacy(&self, owner: &str, legacy: &CanonicalSession) -> Result<ImportOutcome>;
      pub async fn append_summary(&self, owner: &str, session: &str, summary: &str, covered_through_seq: i64) -> Result<i64>;
      pub async fn load_view(&self, owner: &str, session: &str) -> Result<SessionView>;
  }
  ```
  实现上面的第 2/3/4/6/8 条（事件格式、幂等键、事务、视图、导入）。
- 不做：不接 agent loop（T04）；不做折叠决策（T04 调 `append_summary`）。
- 验收：`cargo test -p agent24-memory session_log`
  - 一轮两条同事务：预先占用 assistant 事件的 id 制造第二条冲突 → `append_turn` 返回 Err，**user 事件也不存在**（反面对照：去掉事务必红）；
  - 配额耗尽（用 0014 的配额设成 0）→ Err 且无半写；
  - 同内容重试同一轮 → 幂等不增事件；同 id 不同内容 → Err；
  - `import_legacy`：summary+recent 全部导入、带标记；再调一次 → `AlreadyImported`、事件数不变；中途失败整体回滚；
  - `load_view`：连续两次 `append_summary` 后视图 = 最新 summary + 其 `covered_through_seq` 之后的消息；**另一个 session 的事件穿插在中间（seq 不连续）时**结果仍正确；`covered_through_seq` 之后、summary 事件之前写入的消息不被跳过；
  - `replay::replayed_from_events` 对本会话事件只还原出 `message` 消息，元数据 kind 被跳过。
- 文件：`agent24-memory/src/{session_log.rs,lib.rs}`

**M1-T04 agent loop 切换到 `SessionLog`** [B] · 依赖：T03 · ≈280 行
- 范围：`agent24-agent` 的 `SessionMemory` 改持有 `SessionLog` + **personal owner key（构造时注入的 `String`，本片不关心它怎么来）**；`session_context` = 锁内 `ensure_imported`（无事件且有 blob → `import_legacy`）→ `load_view` → 组装上下文（summary 作 system 消息 + tail；超硬上限只截视图）；`remember_exchange` = 锁内 `ensure_imported` → `append_turn` → 需要折叠时 `summarize` + `append_summary`（失败只记 error，原文已在）；写失败发 `memory.write_failed` 事件。`CanonicalSession::save` 不再被 agent loop 调用（类型保留，导入用）。
- 不做：agentd 接线（T05）；不改 `Condenser` 模块；不做自动补写。
- 验收：`cargo test -p agent24-agent`
  - **升级混合场景**：KV 里预置旧 blob → 跑一轮 → 重建 `SessionMemory`（模拟重启）→ 上下文 = 旧 summary + 旧 recent + 新一轮，逐条有序；blob 未再被写；
  - **强制折叠成功**：小 `max_recent`、跑 N 轮触发两次折叠 → 重启 → 分页 scan 本会话 `message` 事件**逐条等于全部 2N 条原文**；上下文含最新 summary；
  - **摘要持续失败**：跑到超过 4×`max_recent` → 重启 → 全部原文仍在事件日志；上下文长度 ≤ 4×`max_recent`；
  - 写失败（预占 id / 配额 0）→ run completed + 发出 `memory.write_failed`，事件日志无半写；
  - 同一 session 两个并发首读只导入一次。
  - 反面对照：把 `remember_exchange` 退回 `CanonicalSession::save`，前三条必红。
- 文件：`agent24-agent/src/lib.rs`（必要时拆 `agent24-agent/src/session_memory.rs`）

**M1-T05 agentd 接线 + 真实路径崩溃重放** [B] · 依赖：T02、T04 · ≈200 行
- 范围：agentd 启动时 `ensure_personal_recorded(org, user)` 取 personal key，构造 `SessionMemory`；端到端测试经真实 run 入口（mock provider）。
- 验收：`cargo test -p agent24d session_memory`：会话写 3 轮 → 丢弃进程内状态、重开库 → 第 4 轮发给 mock provider 的 messages 与重启前逐条相等（含 summary）；事件的 `scope.owner` 等于 personal key（不是裸 user id）；反面对照：注入裸 user id 作 owner，断言必红。
- 文件：`apps/agent24d/src/{server.rs 或 runs.rs 的构造点}`、`apps/agent24d/tests/`

### F3 跨会话召回

**M1-T06 Retain：显式「记住」写入断言账本** [B] · 依赖：T05 · ≈220 行
- 范围：run 终结时（`append_turn` 成功之后），**只看用户 prompt**（不看模型输出）匹配显式句式（`记住…`/`请记住…`/`remember …`/`remember that …`，规则表写成常量 + 表驱动测试）→ `Candidate{scope.owner=personal key, subject="user", predicate="said_to_remember", object=去掉触发词的原句, evidence=vec![TurnIds.user], origin=UserSaid}.remember()` → `WriteGate::propose`。assertion id = `sha256(owner ‖ object)`（同句重复记住幂等）。
- 不做：同义更新/supersede（推迟，M1 每次记住都是独立断言，撤回靠 T09）；LLM 抽取（T06b）；consolidator。
- 验收：`cargo test -p agent24d retain`：「记住我对花生过敏」→ 账本 1 条 qualified、`evidence == [该轮 user 事件 id]`；同句再说一次仍 1 条；**模型回答里出现「记住…」不产生候选**；直接构造 `Trust::Model` 的同句 Candidate 经 WriteGate → `Held`（持久化但 `qualified=0`，不进召回）；`append_turn` 失败时不写断言（没有 evidence 就不记）。反面对照：去掉「只看用户 prompt」的限制，模型回答用例必红。

**M1-T06b LLM 抽取偏好/事实** [笔记本] · 依赖：T06、T08 · 规模另估 · M1 可选，不阻塞发布。

**M1-T07a 中文可检索：FTS 写入接缝下移 + CJK 二元组 + OR 检索入口（迁移 0017）** [B] · 依赖：无 · ≈280 行 · **现在可入队**
- 范围：
  - `0017_assertions_fts_cjk.sql`：`DROP TRIGGER mem_assertions_fts_ai`；`DROP TABLE mem_assertions_fts`；重建虚表，多一列 `cjk`（应用层预处理文本），`tokenize='unicode61'` 不变；建一张 `mem_fts_state(k TEXT PRIMARY KEY, v TEXT)` 并插入 `('needs_rebuild','1')`。（SQL 算不了二元组，所以迁移只建空表 + 打标记。）
  - 写入接缝：`AssertionLedger::insert_tx`（`assert` 与 `WriteGate` 的共同路径）在**同一事务**里写 FTS 行（含 `cjk`）。
  - 回填：`KvStore::open` 跑完迁移后，若 `needs_rebuild='1'` → `FtsRetriever::rebuild`（Rust 计算 `cjk` 列，事务内清空 + 全量写 + 清标记）。
  - 纯函数 `cjk_bigrams(text) -> String`：CJK 连续段切成重叠二元组（单字段保留单字），非 CJK 不进 `cjk` 列；索引与查询共用。
  - **检索**：`search`（AND，现有语义，不动）+ 新增 `search_any(query, owner, limit)`：英文词与 CJK 二元组 **OR**，按 bm25 排序。召回（T07）用 `search_any`。
- 不做：改 tokenizer；向量召回；改 `search` 的语义。
- 验收：`cargo test -p agent24-memory retriever && cargo test -p agent24-memory migration_0017`
  - 「我对花生过敏」可被「过敏」「花生」各自命中（`search` 与 `search_any` 都能）；**完整问句「我对什么过敏？」经 `search_any` top-1 命中它**，且不命中无关断言「我喜欢北京烤鸭」；英文既有用例全过；
  - **旧库升级**：0016 态库里有中文断言 → 开库 → `search_any` 命中；
  - **新写入**：经 `WriteGate::propose` 写入的中文断言立即可检索（反面对照：去掉 `insert_tx` 里的 FTS 写入必红）；
  - 清空 FTS 后 `rebuild` 结果一致；rebuild 中途注入失败 → 回滚、索引不变、标记仍为 1。
- 文件：`agent24-memory/{migrations/0017_*.sql,src/retriever.rs,src/assertion.rs,src/lib.rs}`

**M1-T07 Recall：run 前召回并注入上下文** [B] · 依赖：T05、T06、T07a · ≈220 行
- 范围：每次 run 组装上下文时，用 run 的用户 prompt 调 `search_any(prompt, personal_key, k)`，取 top-k（默认 5）按 token 预算渲染成**一条 system 消息**（`"你记得关于用户的这些事：\n- …"`），放在会话上下文之前；召回到的 assertion id 记入 `tracing::info!` 与 run 事件 `memory.recalled{ids}`（审计；`ContextFragment` 不改）。只查 personal key。
- 不做：向量召回（T07b）；对话事件全文召回。
- 验收：`cargo test -p agent24d recall`（经真实 run 入口 + mock provider）：会话 A「记住我对花生过敏」→ **新会话 B 问完整问句「我对什么过敏？」**，发给 mock provider 的 messages 里有该断言；往某模块分区写入同关键词断言 → B 的召回**不含**它；无关问句不注入；预算 0 不注入；`memory.recalled` 事件 ids 与注入内容一致。反面对照：owner 改成模块分区 key，跨空间用例必红。

**M1-T07b 向量召回** [笔记本] · 依赖：T07 · 规模另估 · 可选。

**M1-T08 召回评测基线** [B] · 依赖：T07 · ≈200 行
- 范围：用 `eval.rs`（LongMemEval 形状）做仓内小样本集（≤20 例，中文为主，手写，含 5 条干扰断言），`cargo test` 跑 `search_any` 基线，打印命中率；**只记录不设门**。
- 验收：`cargo test -p agent24-memory eval_m1 -- --nocapture` 输出命中率；评测集文件存在且 ≥15 例。

### F4 查看、搜索、撤回

**M1-T09 存储层同事务撤回 `forget`** [B] · 依赖：无 · ≈150 行 · **现在可入队**
- 范围：`agent24-memory`：`pub async fn forget(pool, owner, id, at) -> Result<Forget>`（`enum Forget { Forgotten, AlreadyForgotten, NotFound }`，签名已编译验证），一个事务里：`UPDATE mem_assertions SET recorded_to=? WHERE id=? AND scope_owner=? AND recorded_to IS NULL` + `append_tx` 一条 `assertion.retracted` 事件（id = 对 `serde_json::to_string(&["retract", owner, assertion_id])` 得到的紧凑 JSON 数组字符串取 UTF-8 字节后求 sha256）。挂在 `KvStore` 上暴露。
- 验收：`cargo test -p agent24-memory forget`：成功 → 断言失效 + 1 条撤回事件；再调 → `AlreadyForgotten`、无新事件；他人 owner → `NotFound`；**配额 0 时**（事件写不进）→ Err 且断言仍有效（同成同败；反面对照：拆成两次独立写必红）；`(owner="alice", assertion_id="xa1")` 与 `(owner="alicex", assertion_id="a1")` 各自独立撤回，且事件 ID 不同。`search` 撤回前命中、撤回后及 `rebuild` 后均不再返回它；**待 T09 与 T07a 集成时补齐**：验证 `search_any` 在撤回前命中、撤回后不命中，且 `rebuild` 后仍不命中（当前 `ab/m1-memory` 尚无 `search_any`，因此此项未验收）。
- 文件：`agent24-memory/src/{assertion.rs,lib.rs}`

**M1-T10 记忆 REST：列出 / 搜索 / 撤回** [B] · 依赖：T07、T09 · ≈300 行
- 范围：`GET /api/v1/memory/assertions?q=&limit=`（无 q 列出最新 qualified 断言；有 q 走 `search_any`）、`DELETE /api/v1/memory/assertions/{id}`（调 `forget`；`Forgotten`/`AlreadyForgotten` → 204，`NotFound` → 404）；只暴露 personal 空间；`protocol/openapi.yaml` + `pnpm gen:api`。
- 验收：`pnpm lint:openapi`、codegen 无 diff；`cargo test -p agent24d memory_routes`：撤回后 T07 的召回不再注入它（端到端）；模块分区的 id → 404（不泄露存在性）；重复 DELETE → 204。
- 文件：`apps/agent24d/src/{memory_routes.rs,server.rs}`、`protocol/openapi.yaml`、`packages/api-client`

**M1-T11 桌面端「记忆」页** [B] · 依赖：T10 · ≈300 行
- 范围：设置页旁「记忆」：列表 + 搜索 + 撤回（二次确认）；空状态「说『记住……』让我记住」；文案用「撤回」不用「删除」。
- 验收：`pnpm typecheck && pnpm lint && pnpm test`；组件测试：列表渲染、撤回后从列表消失、请求失败显示错误且不清空列表。

### 依赖图与 B 上入队顺序

```
T01 ─────────────────────────────── (进行中)
T02 ───────────────┐                 (进行中)
T03 ── T04 ────────┴─ T05 ─┬─ T06 ─┐
T07a ──────────────────────┼───────┴─ T07 ─┬─ T08 ──(笔记本) T06b / T07b
T09 ───────────────────────┘               └─ T10 ─ T11
```

| 批次 | 入队条件 | task |
|---|---|---|
| 0（已在跑） | — | T01、T02 |
| **1（现在就入队）** | 无依赖 | **T03、T07a、T09** |
| 2 | T03 合并 | T04 |
| 3 | T02 + T04 合并 | T05 |
| 4 | T05 合并 | T06 |
| 5 | T05 + T06 + T07a 合并 | T07 |
| 6 | T07 合并 | T08；T10（还需 T09） |
| 7 | T10 合并 | T11 |

迁移编号：T02 = 0016、T07a = 0017，按编号顺序合并（T07a 若先于 T02 合并，执行者把 0017 的「0016 态」测试基线改为当时的最新态并在 PR 说明）。

**M1 完成判据**：T01–T11（除 T06b/T07b）全部合进 `ab/m1-memory` → 笔记本上真实 agent24d + 桌面端走一遍「会话 A 记住 → 重启 daemon → 会话 B 用完整问句召回 → 记忆页撤回 → 会话 C 不再召回」→ 人工开 release PR `ab/m1-memory → main`（PR-Daemon 评审），release note 写明「不支持降级」与旧会话恢复边界。

## 3. 已拍板的开放问题（jason 2026-10-01：全部按推荐）

1. 旧会话：**首读/首写时在每会话锁内一次性导入为事件**（一个事务、幂等），恢复边界见 F2 第 8 条。
2. Retain：**M1 只做用户明说「记住……」的规则写入**；LLM 自动抽取 = T06b（笔记本，可选）。
3. 召回范围：**只召回 personal 空间**，模块记忆永远不进 agent loop 上下文。
4. 摘要：**落成 `session.summary` 事件**（带 `covered_through_seq`），不每轮重算。
5. AgentEar 语音摄入：**不进 M1**；冻结后把事件形状（`message` / `session.summary` / 断言写入）同步给 agentear-73，摄入接口放 M2。

## 4. 明确不在 M1

shared space / grant / group（ADR-030 F9）· `asserted_by` + 冲突断言（SPEC-ORG-SPACE F11，2026-10-03 移入 P1）· 编辑记忆 · 同义更新/supersede · 物理擦除与导出 · consolidator/insight · knowledge/instruction store · 多用户 · 跨设备同步 · 配额 UI · 写失败自动补写（FU）· 降级回旧版本。

## 5. 后续 FU（M1 内不做）

- FU-M1-1：`memory.write_failed` 的自动补写（从 run 的 durable thread 按幂等 id 重放）。
- FU-M1-2：同义更新 / supersede 的匹配规则。

## 6. 评审处置表（B 端 Codex gpt-6-astra，基准 `af394b9`，2026-10-01）

| # | 发现 | 处置 | 落点 |
|---|---|---|---|
| H1 | 「没有事件就导入」在影子写后漏历史 | **接受**：取消影子写阶段，一次切换；导入在每会话锁内、首读首写前；导入+标记同事务；写明不支持降级 | F2 决定 1/8、T03、T04 升级混合测试 |
| H2 | 权威写允许静默失败，no-loss 不成立 | **部分接受**：两条事件同事务 + 内容稳定幂等 id + 显式失败（`memory.write_failed`）；**自动持久重试不做**（原型阶段，记 FU-M1-1） | F2 决定 3/4/5、T03、T04 |
| H3 | 原文恢复边界未写、`covers()` 不能证明持久化、T5 没强制压缩 | **接受**：写明旧数据只能导入 blob 剩余部分；原文永不删除；新增「强制折叠成功→重启→逐条读全部原文」「摘要持续失败超硬上限→重启」两组测试 | F2 决定 7/8、T04 验收 |
| H4 | `chat.*` kind 与 replay 不兼容；摘要不是现成能力；source 是下标 | **接受**：对话沿用 `kind="message"`；元数据另用 kind；定义 `covered_through_seq` 契约；不用 `LlmSummaryCondenser`；测连续两次摘要与非连续 seq | F2 决定 2/6、T03 验收 |
| H5 | 二元组 + 隐式 AND，完整问句召回为 0 | **接受**：新增 `search_any`（OR + bm25），`search` 语义不动；验收用完整问句 + 无关反例 | T07a、T07 |
| H6 | 写入接缝找错（实际是 SQL 触发器）；FTS 不能加列；回填缺失 | **接受**：迁移删触发器 + 重建虚表 + 打标记；写入下移到 `insert_tx` 同事务；开库时 Rust 回填；覆盖旧库升级/新写入/rebuild/中途失败 | T07a |
| H7 | 撤回事件与账本失效不原子 | **接受**：存储层 `forget` 同事务，`Forgotten/AlreadyForgotten/NotFound` 语义，配额失败同成同败 | T09（新片）、T10 |
| M1 | 迁移测试基线错一位 | **接受**：用 16，先断言版本；补约束保留与失败回滚测试 | T02（已在跑，补充已追加进其任务 prompt） |
| M2 | provenance/trust/supersede 不能直接复用 | **接受**：evidence 用 EventId；只看用户 prompt + Model→Held 不召回；推迟同义更新 | T06、§4、FU-M1-2 |
| M3 | 依赖不全、拆片不足 | **接受**：先拆存储接口 T03/T09/T07a 再接线；T08→依赖 T07；T10 依赖 T07+T09；每片独立反面对照 | §2 依赖图 |
| M4 | `ActiveScope`、`ContextFragment` 无定义 | **接受**：T01 给最小占位（已编译验证）；召回审计改走 `memory.recalled` 事件，不改 `ContextFragment` | T01、T07 |
| M5 | 承诺超出交付（「改得了」、现有擦除路线） | **接受**：§0 收窄为查看/搜索/撤回；删除「现有擦除路线」表述 | §0、§4 |
| L1 | §1 两处措辞 | **接受**：`session_context/remember_exchange` 属 `RunManager`；外部引用表述改为按实际列举 | §1（重写后不再含该概括） |

拒绝 0 条；部分接受 1 条（H2：自动补写推迟为 FU）。
