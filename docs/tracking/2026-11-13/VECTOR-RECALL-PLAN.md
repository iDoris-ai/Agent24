# 向量召回接入对话循环——实施方案（第 1 版）

- 日期：2026-11-13
- 状态：已冻结（2026-11-13）。jason 已拍板 §5 三项，PR-Daemon 一轮评审无阻塞；评审带入分片的事项见 §4 末「评审带入」。只写文档，没有改代码。
- 来源：用户外部反馈「跨会话记忆只有 SQLite FTS 全文检索，向量检索还没接进 loop」。组件规范化（C9、技能加载器 v1）收口后的下一条主线。
- 依据：[ADR-MEMORY-P1-LIFECYCLE-AND-PURGE](../../adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md)（下称「生命周期 ADR」）；[ADR-MEMORY-RECALL-MESSAGE-ORIGINS](../../adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md)；[ADR-K1-02](../../adr/ADR-K1-02-session-dataflow-security-modes.md)；[ADR-039](../../adr/ADR-039-concept-model.md) §2；[ADR-046](../../adr/ADR-046-idoris-managed-host.md)；[ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md)、[ADR-KB-02](../../knowledge/ADR-KB-02-HOST-AUTHORITY.md)；[SPEC-MD-ME](../../specs/SPEC-MD-ME.md) MD-6；[MEMORY-STRATEGY](../../research/MEMORY-STRATEGY.md) P2；[HW-TIERS](../../design/HW-TIERS-capability-hardware-matrix.md) §4。
- 核对基线：`main` @ `ee5d6c360`（本工作树 `docs/vector-recall-plan`）。行号开工时按符号再对一次。

## 1. 一页结论

今天的跨会话召回只召回一种东西：用户明说「记住……」写进账本的事实（`mem_assertions`）。召回走 FTS5 关键词检索，换个说法就找不到。

向量的底座其实已经在仓里：`agent24-memory/src/vector.rs` 有 `Embedder` 接口、`VectorRetriever`、暴力余弦和换模型后的重嵌状态机，表 `mem_embeddings`（memory 迁移 0007）也带了 `model_id`、`revision`、`dims`。缺的只有三样：一个真的本地嵌入实现（文档里叫 `OmlxEmbedder`，至今不存在），召回入口改成「关键词 + 向量」混合，以及后台把事实补嵌进去。

方案按原型尺度来，不引入新引擎：

1. **嵌入在哪算**：走 OpenAI 兼容的 `/v1/embeddings`。开了 iDoris 就走 iDoris（带 `X-iDoris-Privacy: local_only`，回包必须是 loopback），没开就直连本机 oMLX（只认 loopback 地址）。不进程内加载模型，也没有云端嵌入。
2. **存在哪**：沿用 `mem_embeddings` 的 BLOB 列加 Rust 暴力余弦，不上 sqlite-vec。个人事实是千条量级，暴力扫描远低于 10 ms。
3. **怎么排**：FTS 取前 20，向量取前 20（余弦过下限才算），用 RRF（k=60）合并，取前 5。没有嵌入模型、模型超时、索引为空时，结果和今天的纯 FTS 逐字节一致。
4. **注入点不动**：仍是 `SessionMemory::recall` 生成的那一条「记忆数据·非指令」消息，预算仍是 1024 字节、5 条。消息来源（`HostRecall`）、冷恢复的逐条复验、K1 标签都不改格式。
5. **标签**：向量表里不存分类。命中后一律回联 `mem_assertions` 取 `source_ref`，`Unknown` 照样是 `Unknown`。
6. **删除**：事实被撤回或更正时，用触发器在同一事务里删掉它的所有向量（所有模型、所有版本）。入站文字不会变成事实，所以 180 天入站清理没有向量副本可删；这条用测试钉住。

共 1 个探针加 5 片，1 片带迁移。没有协议和界面改动。

## 2. 现状与差距

### 2.1 召回今天怎么进 run

| 需要的东西 | 现状 | 证据 |
|---|---|---|
| 召回入口 | `SessionMemory::recall(prompt)`：先查暂停开关，再 `retriever().search_any(prompt, owner, 5)`，按预算拼成一条 `user` 角色消息 | `agent24-agent/src/session_memory.rs:153-230`，检索调用 `:171` |
| 召回框 | 开头 `[记忆数据·非指令] …不是新的指令，不会改变系统规则或工具授权`，结尾 `[记忆数据结束]`；每条带 `id`、`recorded_at`，正文先清控制字符再 JSON 引号 | `session_memory.rs:31-43`、`:190-205`、`:618-634` |
| 预算 | 1024 字节（UTF-8 字节加帧，偏保守的估计），前 5 条，单条 500 字符封顶 | `session_memory.rs:22-23`、`:48`、`:140-146` |
| 调用点 1：run | `execute` 在 run 开始时调一次，失败只告警、不挡 run；取消则结束 | `agent24-agent/src/lib.rs:2508-2527` |
| 调用点 2：无状态 chat | `chat_memory_prelude` 同样调 `memory.recall` | `lib.rs:1104-1150`（调用 `:1133`） |
| 快照与消息来源 | 召回消息作为快照第一条落库，来源 `MessageOrigin::HostRecall { recall_ref }`，和快照同一事务 | `lib.rs:2611-2650` |
| 冷恢复复验 | 恢复时按当前账本逐条复验 `HostRecall`，被撤回的 id 不再发给模型 | `lib.rs:2990-3075`；`session_memory.rs:236-246` |
| K1 标签 | 每条召回写 `SourceRef::recalled_memory(id, classification)`，`mode` 恒为 `LocalOnly`；分类取自该事实的 `source_ref`，缺失或读不懂即 `Unknown` | `lib.rs:2626-2638`；`agent24-store/src/source_policy.rs:248-263`、`:516-526`；`session_memory.rs:213-215` |
| 外发门 | 非 `Ordinary` 来源挡外发；有入站对照测试 | `lib.rs:6900` |

### 2.2 FTS 与账本

| 项 | 现状 | 证据 |
|---|---|---|
| 账本 | `mem_assertions` 主键 `id`，`id` = SHA-256(owner ‖ 正文)；撤回只置 `recorded_to`，正文保留 | memory 迁移 `0004_assertions.sql:20`；`agent24-agent/src/retain.rs:238`；`agent24-memory/src/assertion.rs:416-430` |
| 更正 | 同一事务里关闭旧行再插新行 | `assertion.rs:395-413` |
| 重申 | 同文再「记住」落到同一行，`recorded_to` 置回 NULL，`source_ref` 更新 | `agent24-memory/src/writer.rs:731-738` |
| 谁能写事实 | 只有 `Trust::UserSaid` 的直接用户 run；入站、模型、定时内容都写不进 | `retain.rs:224-228`；`session_log.rs:316-323` |
| FTS | `mem_assertions_fts`（unicode61 + Rust 算的 CJK 二元组），写入时同事务索引，查询回联账本过滤 owner / 未关闭 / 已确认 | 迁移 `0017_assertions_fts_cjk.sql`；`retriever.rs:75-105`、`:278-320`；`assertion.rs:348` |
| 评测基线 | `eval_m1.rs`：手写中文用例，Hit@5 ≥ 13/14 与「无答案不注入」都是硬门 | `agent24-memory/src/eval_m1.rs:1-30`、`:271-384` |

### 2.3 已有的向量与嵌入代码

| 项 | 现状 | 证据 |
|---|---|---|
| 向量表 | `mem_embeddings(assertion_id, scope_owner, model_id, revision, dims, normalized, vec BLOB, at)`，主键 `(assertion_id, model_id, revision)`，`CHECK(length(vec) = dims*4)` | memory 迁移 `0007_embeddings.sql` |
| 接口与检索器 | `Embedder` trait（`model_id`、`revision`、`embed`）；`VectorRetriever::reindex` 只嵌缺的；`search` 只在索引**完整**时用向量，否则整段退回 FTS | `vector.rs:44-52`、`:146-194`、`:197-280` |
| 校验 | 声明与返回的模型身份一致、长度等于 `dims`、分量有限，否则拒绝 | `vector.rs:113-140` |
| 生产调用 | 没有。`KvStore::vector_retriever` 只有测试在用；`OmlxEmbedder` 只出现在注释里 | `agent24-memory/src/lib.rs:606-613`；`SPEC-MD-ME.md:75`、`:224` |
| 重嵌文本 | `reindex` 嵌的是 `"{subject} {predicate} {object}"`，`object` 是 JSON 原文（带引号），`subject`/`predicate` 在生产里是常量 `user`/`said_to_remember` | `vector.rs:166-170`；`retain.rs:239-245` |
| 竞态 | `reindex` 先读后嵌再插，中间被撤回的事实仍会插进向量 | `vector.rs:148-190` |
| ML worker | `agent24-worker` 定义了 `/v1/embed` 合同，但被 S1-7 冻结，没有产品调用方 | `agent24-worker/src/lib.rs:1-20`；`rust/Cargo.toml:34-41` |
| iDoris | 路由有 `/v1/embeddings`（角色 `embed`），付费嵌入直接 503 | iDoris `crates/idoris-router/src/lib.rs:393`、`:655-690`、`:776-790` |
| Agent24 → iDoris | `IdorisGateway` 只有 chat；请求带 `X-iDoris-Privacy: local_only`，回包必须 `x-idoris-served-locality: loopback` | `agent24-models/src/idoris.rs:330-332`、`:375` |
| 路由开关 | 设了 `IDORIS_URL` 走 iDoris，否则直连 oMLX（默认 `http://127.0.0.1:8088`），非 loopback 一律标 Remote | `agent24-models/src/router.rs:358-405`、`:182-197` |
| 本机模型 | `~/.omlx/models/` 只有 `jina-embeddings-v2-base-zh`（1.6 GB）一个嵌入模型。2026-11-13 实测 oMLX `/v1/embeddings` 加载它失败：`Received 60 parameters not in model` | 本机 `curl` 探测，见 §7 第 1 条 |
| 本机数据量 | `~/.agent24/memory.db`：`mem_assertions` 1 行，`mem_embeddings` 0 行 | 本机只读计数 |

### 2.4 文档里已经写下的约束

- 生命周期 ADR 向量一行：「个人产品向量上线 N/A（P2）……未来接入必须支持逐 owner/对象、所有 model/revision 删除及重建 fence；实例若发现残留行必须处理」（`ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md:39`）。全个人域 purge 本身已接受、待实现（`:5`、`:109`）。
- K1-02：索引、向量化等变换继承来源；LocalOnly 不得送云（`ADR-K1-02-session-dataflow-security-modes.md:80`）；embedding 请求属于「模型」出口（`:92`），向量属于「存储外流」（`:95`）。
- MEMORY-STRATEGY：先过滤再排序，多路召回用 RRF，不直接加分（`MEMORY-STRATEGY.md:27`）；P2 验收是同一用例集上混合检索的 Hit@5 高于纯 FTS（`:192-196`）。
- HW-TIERS：记忆检索推荐 bge-m3（约 1.1 GB fp16），2 档 8 GB 降级为「关键词 + 小嵌入」（`HW-TIERS-capability-hardware-matrix.md:131`）。云端补位默认关（`:123`）。
- 知识库（WeKnora）的切块与嵌入归 Knowledge 组件，个人记忆归 Agent24 M1，两边不双写（`docs/knowledge/POSITIONING-AND-DIFFS.md:23`、`:30`）。本方案只动 M1。

### 2.5 差距

一句话：底座和表都在，缺本地嵌入实现、缺混合排序、缺后台补嵌，召回入口仍只调 FTS。附带要补的两个洞：撤回不删向量、`reindex` 与撤回有竞态。

## 3. 目标设计

### 3.1 嵌入在哪算

| 方案 | 做法 | 好处 | 问题 |
|---|---|---|---|
| A. 跟随现有路由开关 | 设了 `IDORIS_URL` 走 iDoris `/v1/embeddings`（`local_only` 头，回包校验 loopback）；否则直连 oMLX `/v1/embeddings`（只认 loopback） | 与 chat 同一条开关、同一条隐私校验；iDoris 管加载与驻留（ADR-046 §「模型选型、量化、加载仍归 iDoris」） | 直连 oMLX 时没有人管驻留，嵌入模型可能挤占主模型内存（§7 第 2 条） |
| B. 只走 iDoris | 没有 iDoris 就没有向量 | 最干净 | 今天默认不设 `IDORIS_URL`，大多数机器等于不开 |
| C. 进程内模型（candle / ort） | daemon 自己加载 ONNX | 不依赖外部进程 | 新增重依赖和二进制体积；模型管理又多一处；与「iDoris 管模型」冲突 |
| D. 复用 `agent24-worker` | 起 Python worker | 合同已写 | S1-7 冻结，没有 Python 实现 |

**推荐 A。** 实现放在 `agent24-models`（已有 reqwest 和 iDoris 客户端），新增一个 `EmbeddingClient`：

- 地址不是 loopback 就构造失败（复用 `is_loopback_url`），不降级成远端。
- iDoris 模式回包没有 `x-idoris-served-locality: loopback` 就报错，与 chat 相同。
- 模型名来自配置 `A24_EMBED_MODEL`，缺省按 §5 决定 1。
- 一次请求可批量多条（`input` 是数组），后台补嵌用批量，查询用单条。
- 校验条数、维度、有限值，沿用 `vector.rs:113-140` 的口径。

`agent24-memory` 已依赖 `agent24-models`，在 memory 里写一个适配器实现 `Embedder`。

**版本（`revision`）怎么定。** OpenAI 兼容接口不给模型版本。做法：启动时嵌一句固定探针，把向量存进 `kv`（namespace `embed_probe`，不需要迁移）。之后每次启动重嵌探针，余弦低于 0.999 就认定「同名模型换了内容」，`revision` 换成新的探针摘要。`revision` = `<dims>:<探针向量取 3 位小数后的 SHA-256 前 12 位>`。这样换模型、换量化都会自然触发重嵌。

### 3.2 存储

| 方案 | 说明 | 结论 |
|---|---|---|
| BLOB 列 + Rust 暴力余弦 | 已有表、已有代码、已有测试；1024 维每条 4 KiB，1 万条 40 MB，扫一遍几毫秒 | **采用** |
| sqlite-vec | 要给 sqlx 的 SQLite 加载扩展，跨平台打包和签名多一个动态库；量级上用不着 ANN | 不做 |
| 外部向量库（Qdrant 等） | 违反本地优先、无强制外部服务（`SPEC-MD-ME.md:85`） | 不做 |

暴力扫描只扫当前 `(model_id, revision)` 且回联后仍有效的行。若将来单个 owner 超过 5 万条再考虑 ANN，本期不做。

### 3.3 混合排序与回退

新增 `HybridRetriever::search(query, owner, k)`，放在 `agent24-memory`：

1. **FTS 路**：`search_any(query, owner, 20)`，与今天同一个函数。
2. **向量路**：先查 owner 当前模型有无向量行，没有就跳过，**不调嵌入**（省掉每轮一次本地请求）。有就嵌查询（超时 1.5 s），对当前模型行算余弦，丢掉低于下限 `MIN_COSINE` 的，取前 20。SQL 与 `vector.rs:236-246` 同形：回联 `mem_assertions`，过滤 `scope_owner`、`recorded_to IS NULL`、`qualified = 1`。
3. **合并**：RRF，`score = Σ 1/(60 + rank)`，同分按 `id` 升序。取前 `k`（仍是 5）。
4. **回退**：嵌入不可用、超时、报错、索引为空，都只用 FTS 路，结果与今天的 `search_any(query, owner, 5)` 完全一致（这是验收的正对照）。只打一条 `warn`，不挡 run。

和现有 `VectorRetriever::search` 的区别：那里要求索引完整才用向量，部分索引就整段退回 FTS（`vector.rs:209-231`）。混合检索不需要这条门：没嵌到的事实仍由 FTS 路找到，不会出现「只返回已嵌部分」的错答。`VectorRetriever::search` 保留，不删。

`MIN_COSINE` 是防「什么都不相关也塞 5 条进来」的闸。`eval_m1` 里「无答案不注入」是硬门，向量路不能打破它。初值按探针（VR-0）实测定，VR-5 用评测集校准后写死成常量。

### 3.4 注入点与预算

注入点不动：`SessionMemory::recall` 里把 `retriever().search_any(...)`（`session_memory.rs:169-172`）换成 `HybridRetriever`。两个调用点（`lib.rs:1133`、`:2511`）自动生效，不各改一遍。

- 消息格式、前后标记、逐条 `id`/`recorded_at`、JSON 引号都不变。
- 预算不变：1024 字节、5 条、单条 500 字符。混合检索只改变「哪 5 条」，不增加 token。
- `HostRecall` 的 `recall_ref` 仍是事实 id 列表，冷恢复复验不变。
- `SessionMemory` 增加一个可选的嵌入器（`with_embedder`），没有就等于今天的行为。

### 3.5 K1 来源标签

- `mem_embeddings` 不加分类列。分类只有一个权威：`mem_assertions.source_ref`。向量路命中后回联账本，用 `AssertionSourceRef::classification_of` 取分类（与 FTS 路同一行代码，`session_memory.rs:213-215`）。
- 重申会更新 `source_ref`（`writer.rs:731`），分类可能变严。向量里没有副本，所以不会过时。
- 召回标签仍是 `SourceRef::recalled_memory`，`mode` 恒为 `LocalOnly`。`Unknown` 事实无论经哪一路召回，标签都是 `Unknown`，外发门照常挡。
- 嵌入请求本身：事实正文和用户本轮提示都送到本机嵌入服务。LocalOnly 允许本机（K1-02 `:80`、`:92`）。入站 run 的提示是 `Unknown`，同样只到本机，可以嵌。

### 3.6 删除与生命周期

| 事件 | 向量怎么办 | 落在哪 |
|---|---|---|
| 撤回、更正（`recorded_to` 置值） | 同一事务删掉该事实的**所有** `(model_id, revision)` 行 | memory 新迁移：`AFTER UPDATE OF recorded_to ON mem_assertions WHEN NEW.recorded_to IS NOT NULL` 的触发器。迁移里顺手清掉已经关闭的事实的残留向量 |
| 重申（`recorded_to` 回到 NULL） | 不处理；后台补嵌发现缺向量会重嵌 | 触发器条件不命中 |
| 补嵌与撤回并发 | 插入语句改成 `INSERT … SELECT … WHERE EXISTS(仍有效的该事实)`，撤回已提交就插 0 行 | `vector.rs:174-189` |
| 换模型 / 换版本 | 新版本行缺失 → 混合检索只用 FTS 路补位 → 后台重嵌 → 新版本对该 owner 完整后，删掉旧版本行（磁盘预算） | 后台任务。改掉「reindex 从不删旧行」（`vector.rs:12-16`）这一条，只在新版本完整后删 |
| 全个人域 purge（生命周期 ADR，待实现） | 按 `scope_owner` 删所有模型的行，并受 purge fence 约束 | 本期提供 `delete_owner_embeddings(owner)` 并把生命周期 ADR 第 39 行从「N/A」改为删除口径。purge 本体不在本期 |
| 180 天入站正文清理 | 没有向量可删：入站 run 写不了事实（`retain.rs:224-228`），向量只嵌事实 | 加一条测试：入站 run 结束后 `mem_embeddings` 行数不变。若将来要嵌会话原文，`purge_session` 必须同事务删（§5 决定 2） |

向量是可重建投影（`SPEC-MD-ME.md:86-87`）。整表删掉后台会重嵌，召回期间退回 FTS。

### 3.7 隐私

- 默认没有任何云端嵌入。`EmbeddingClient` 只接受 loopback 地址；iDoris 模式强制 `local_only` 并校验回包。用户开了云端主模型也不影响嵌入走向。
- 暂停个人记忆时，召回入口最早就返回（`session_memory.rs:160-165`），不嵌查询。后台补嵌同样先查暂停开关。
- 召回出的文字进了远端模型的提示，这件事现在就存在，与向量无关（`MEMORY-STRATEGY.md:132`）。外发门照旧。

### 3.8 故障模式

| 情况 | 行为 |
|---|---|
| 没装嵌入模型 / 模型加载失败 | 启动时探针失败，打一条 `warn`，不建嵌入器；召回 = 纯 FTS。过一段时间（如 10 分钟）重试探针 |
| 查询嵌入超时（1.5 s）或报错 | 本轮只用 FTS 路；不缓存失败，下一轮再试 |
| 索引滞后（新记住的事实还没嵌） | FTS 路照常能找到它；后台收到写入通知后补嵌 |
| 模型同名换内容 | 探针余弦 < 0.999 → 新 `revision` → 走换版本流程 |
| 返回维度不对 / 非有限值 / 条数不对 | 拒绝整批，不写入；后台告警并退避 |
| 磁盘紧张 | 补嵌写入前过 `disk_gate`（`WriteClass::Content`），不够就停，不删别的东西腾地方 |
| 远端地址被配成嵌入地址 | 构造失败，等同没有嵌入器 |

## 4. 分片计划

集成分支 `ab/vector-recall`（本方案不建分支）。task 分支 `ab/vector-recall-NN-<短名>`，PR 基线是集成分支。「规模」是该片产品代码估计行数，不含测试，超过 300 再拆。VR-2、VR-3 合入集成分支前等 PR-Daemon APPROVE，其余片 CI 全绿加本地门禁后可合。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（正例、反例、变异） | 风险 | 派给 |
|---|---|---|---|---|---|---|---|
| VR-0 探针（不出 PR） | 在笔记本上确认：所选嵌入模型能被 oMLX 加载；iDoris 模式下 `/v1/embeddings` 能出 loopback 回包；维度、冷启动与热延迟、是否挤掉主模型；中英泰三句近义句的余弦 | 只写结果到本文 §7 | 0 | §5 决定 1 | 记下实测数字和命令。若首选模型加载失败（今天 jina 就失败了），换备选再测，不写代码绕过 | 模型下载 1 GB 左右；探针不能在 Mac mini 上跑 | Sonnet |
| VR-1 本地嵌入客户端 | `agent24-models::EmbeddingClient`（iDoris / 直连 oMLX 两种模式，只认 loopback，批量，校验）；`agent24-memory` 里的 `Embedder` 适配器；探针向量定 `revision` | `agent24-models/src/`（新文件）、`agent24-memory/src/vector.rs` 或新文件 | ~220 | 无 | 正例：本地桩服务返回 1024 维，得到 `Embedding` 且 `revision` 稳定。反例：`http://10.0.0.2` 构造失败；iDoris 回包缺 loopback 头报错；维度不符、`NaN`、条数不符都报错。变异：去掉 loopback 检查，远端地址用例变红；探针余弦阈值改成 0，「同名换内容」用例（桩换一组向量）变红 | 把 iDoris 头写错导致回到远端；`revision` 不稳定造成反复重嵌 | Grok |
| VR-2 删除与补嵌正确性（**迁移**） | memory 新迁移：`recorded_to` 置值即删向量的触发器，并清已关闭事实的残留；`reindex` 插入改为「仍有效才插」；嵌纯文本 `object`；`delete_owner_embeddings`；新版本完整后删旧版本行 | `agent24-memory/migrations/`（编号开工时定，今天最新是 0020）、`vector.rs`、`lib.rs` | ~150 | 无（用测试嵌入器）。**迁移，等 PR-Daemon APPROVE** | 正例：撤回后该 id 在所有模型下 0 行；更正后旧 id 0 行；重申后补嵌重新出现。并发：在嵌入与插入之间注入撤回，插入 0 行。旧库升级：带残留行的 0020 库开库后残留清零、有效行不动。变异：删掉触发器，撤回用例变红；把插入改回无条件，并发用例变红；旧版本清理改成「新版本未完整也删」，部分索引用例变红 | 触发器误伤重申；迁移在旧库上锁时间长 | Grok |
| VR-3 混合检索接进召回 | `HybridRetriever`（FTS 20 + 向量 20 + RRF + `MIN_COSINE` + 空索引不嵌查询 + 1.5 s 超时回退）；`SessionMemory::with_embedder`，`recall` 改调混合检索；分类只从账本取 | `agent24-memory/src/`（新文件）、`agent24-agent/src/session_memory.rs`；测试 | ~250 | VR-2。**来源标签相关，等 PR-Daemon APPROVE**。改 `agent24-agent` 后跑 `cargo test -j 4 -p agent24d --bin agent24d` | 正例：换说法的提问（FTS 0 命中）经向量路召回，事实进召回消息。标签：一条 `Unknown` 事实只经向量路命中，run 标签是 `MemoryRecall` + `Unknown`，外发被挡。回退：嵌入器报错 / 超时 / 无向量，召回消息与纯 FTS 逐字节相同。暂停：嵌入器调用计数为 0。跨 owner 0 泄漏。`eval_m1` 两条硬门在混合模式下仍过。变异：向量路把分类写死成 `Ordinary`，标签用例变红；去掉 `MIN_COSINE`，「无答案不注入」变红；去掉回联过滤 `recorded_to IS NULL`，撤回用例变红 | 向量路绕过回联直接读表，丢了 owner 或撤回过滤；超时没生效拖慢每轮 | Sonnet，Opus 验收 |
| VR-4 后台补嵌与 daemon 接线 | `serve` 就绪后 `tokio::spawn`：建 `EmbeddingClient`、跑探针、`reindex(owner)`；记住成功后用 `Notify` 触发补嵌；换版本流程；暂停与 `disk_gate` 检查；探针失败定时重试；就绪后一行状态日志（模型、已嵌/总数） | `agent24d/src/server.rs`（`session_memory` 构造，`:616-638`）、`serve_steps.rs`、`agent24-agent` 记住成功处 | ~200 | VR-1、VR-3 | 正例：空索引 + 桩嵌入器，记住一条后数秒内该事实有向量。反例：嵌入服务不在，daemon 照常就绪，召回为 FTS；磁盘门报不足，不插入；暂停时不补嵌。入站：入站 run 前后 `mem_embeddings` 行数相同。变异：去掉就绪后再起任务（改成就绪前同步跑），就绪超时用例变红；去掉暂停检查，暂停用例变红 | 后台任务和 run 抢 SQLite 写锁；补嵌风暴 | Grok |
| VR-5 评测与文档收口 | `eval_m1` 增加至少 20 条中文换说法用例（记录项）；在笔记本真模型上跑一次，记录纯 FTS 与混合的 Hit@5；校准 `MIN_COSINE`；改 `SPEC-MD-ME` MD-6 行、生命周期 ADR 第 39 行、`MEMORY-STRATEGY` P2 状态、里程碑表 | `agent24-memory/src/eval_m1.rs`、`docs/…` | ~80 + 用例 | VR-0、VR-4 | 混合 Hit@5 高于纯 FTS，数字和命令写进文档可复现；原有硬门不退。真模型评测不进 CI（CI 用测试嵌入器），文档写明。`pre-pr-check` 通过 | 用例写成只对向量有利；阈值过拟合 | Sonnet |

依赖：VR-1 与 VR-2 可并行；VR-3 等 VR-2；VR-4 等 VR-1、VR-3；VR-5 收尾。VR-0 在 VR-1 开工前做完。只有 VR-2 带迁移（agent24-memory 的迁移目录，不是 agent24-store）。

**评审带入（#1843 PR-Daemon，开工时照做）**
- VR-3 / VR-4（Medium）：嵌入服务「接受连接却不回」时每轮都要等满 1.5 秒。加一个小熔断：一次超时后 N 秒内跳过向量路，直接走关键词；测试「连续两次超时只付一次约 1.5 秒」。
- VR-1：直连 oMLX 模式只按 URL 判本机，没有 iDoris 模式那样的回包本机标头；本机代理再转发到外网也能通过。这是配置选择不是攻击面，§3.7 照实写明两种模式的保证不同。
- VR-2：嵌入输入改成纯文本 `object` 后，旧格式向量仍挂在同一 revision 下。生产今天 0 行，无害；探针 / revision 输入里带上文本格式版本，开发库里的旧行会自动重嵌。
- VR-2：新版本补嵌完成后删旧版本行，切回旧模型会全部重嵌。千条量级可接受，§3.6 写一句。

## 5. 已拍板（jason，2026-11-13）

1. 嵌入模型用 **bge-m3**；VR-0 实测 oMLX 加载不了再换 Qwen3-Embedding-0.6B。
2. 这一版**只给「记住的事」做向量**，聊天原文以后单开一期。
3. **模型在就自动用**，不加设置开关；暂停个人记忆时一并停。

以下保留拍板前的选项说明。

**决定 1：用哪个嵌入模型，要不要下载。** 本机现有的 jina 中文模型 oMLX 加载不了，而且只懂中英文，不懂泰语。选项一是 bge-m3（约 1.1 GB，硬件分档文档里已经选的就是它，中英泰都支持）；选项二是 Qwen3-Embedding-0.6B（更小，也是多语言，但还没在分档里评过）。两者都按「点到才下载、下载后校验」处理，不进安装包，8 GB 机器默认不开、只用关键词。推荐 bge-m3，VR-0 实测加载不了再换 Qwen3-Embedding-0.6B。

**决定 2：向量只管「记住的事」，还是也管聊过的话。** 今天召回只认用户明说「记住」的事实，本方案也只给这些事实做向量。如果也给历史对话原文做向量，召回会更「懂上下文」，但要处理按会话删除（ADR 已决定延后）、入站陌生人文字（`Unknown`）混进来、180 天清理要同步删向量，磁盘也会大几十倍。推荐这一版只做「记住的事」，聊天原文以后单开一期。

**决定 3：装好模型后是不是默认就开。** 选项一是模型在就自动用，不加新开关，暂停个人记忆时一并停；选项二是设置里加一个「语义召回」开关，默认关。自动开的代价是每轮多一次本机嵌入请求（预计几十毫秒，超时 1.5 秒就退回关键词）。推荐选项一，不加开关。

## 6. 明确不做

- sqlite-vec、ANN 索引、外部向量库。
- 给会话原文、知识库（WeKnora）、Documents 做向量。知识库的嵌入归 Knowledge 组件（ADR-039 §2 第 56 行、ADR-KB-01）。
- 云端嵌入、任何经远端 provider 的嵌入回退。
- 进程内加载模型；启用 `agent24-worker` 的 `/v1/embed`。
- 重排（reranker）、查询改写、多向量。
- 改召回预算、条数、召回消息格式、消息来源闭集、K1 标签结构。
- 协议、OpenAPI、桌面界面改动。状态只打日志。
- 全个人域 purge 本体（生命周期 ADR P1-08）；本期只交 `delete_owner_embeddings` 和删除口径。
- 共享空间、多用户、跨 owner 检索。
- 后台巩固、自动抽取候选事实。

## 7. 开工核实与 VR-0 / VR-5 回填

2026-11-13，笔记本，oMLX `http://127.0.0.1:8088`，模型 `bge-m3-mlx-fp16`。没有停或重启 oMLX，没有用 admin 接口。jina 仍列在 `/v1/models` 里，但加载失败（开工时那条 `Received 60 parameters not in model`）没有再测。

1. **嵌入模型能不能被 oMLX 加载。已测，能。** `POST /v1/embeddings` 返回 1024 维，向量 L2 范数 1.0。同一探针句两次嵌入余弦 **1.000000**。近义裸句：中文「我喜欢在雨天读书 / 下雨的时候我爱看书」**0.933**，英文 **0.920**，泰文 **0.963**。带生产前缀 `user said_to_remember` 的中文近义 **0.817**。无关：股票 **0.399**，蒸鱼 **0.370**。英文近义对泰文近义 **0.923**（跨语言很近，不当成无关）。
2. **直连 oMLX 时会不会挤掉主模型。这次没测。** 按约束不重启、不用 admin，看不到驻留和挤占。
3. **iDoris 的 embed 角色卡和 `x-idoris-served-locality`。这次没测。** 没起 iDoris。真模型评测用 `EmbeddingClient::direct`，不读 `IDORIS_URL`。
4. **探针定 `revision`。** 同一句两次余弦 1.000000，`LocalEmbedder` 的 0.999 阈值在这次输出上过。没有做两次进程重启对照。
5. **memory 迁移编号。** 本分支最新是 `0021_embedding_close.sql`（开工清单写的 0020 已经过时）。
6. **`chat_memory_prelude` 的生产调用方。** 仍只确认它调同一个 `recall`，没有在这次改调用方。
7. **全个人域 purge。** 本体仍未做。向量侧已有 `delete_owner_embeddings`；当前 key 完整后 `reindex` 删该 owner 其它 revision。见生命周期 ADR 向量行。
8. **磁盘。** bge-m3 约 1.1 GB 仍是分档文档的数，这次没重称。1024 维、已归一化已实测。没有在用户真库上量向量表体积。
9. **延迟（服务已热）。** 单条探针：偏冷 **0.035 s**，热 **0.030 s**。60 条一批 **10.8 s**（约 0.18 s/条）。查询超时仍是 1.5 s；`eval_m1_omlx` 把建索引超时放到 60 s，避免冷启动误开熔断。

### VR-5 真模型评测（2026-11-13，不进 CI）

同一套 `eval_m1` 用例：原有可答 14 条 + 中文换说法 20 条 + 无答案反例 4 条。事实按生产格式嵌入（`user said_to_remember {事实}`），查询嵌裸问句。`MIN_COSINE` **保留 0.62**：换说法对目标事实的余弦最低 **0.648**（`p15-mail`），4 条无答案对全库最高 **0.516**（比特币助记词）。0.62 在这道缝里。升到 0.65 会丢掉 0.648 的邮箱；降到 0.55 会贴近 0.516 的无答案。

| | Hit@5 | 换说法 Hit@5 | 无答案注入 |
|---|---|---|---|
| 纯 FTS（同一库再跑 `search_any`） | 原 14 条仍单独记；换说法不进原分母 | **0/20** | **0/4** |
| 混合（FTS + 向量 + RRF） | **34/34**（原 14 + 换说法 20） | **20/20** | **0/4** |

混合高于纯 FTS。英语/中性无答案注入 0/6，security leak 0。CI 用测试嵌入器跑 `eval_m1` 与 `eval_m1_hybrid`，同样两条硬门（Hit@5 按 13/14 扩到含换说法的集合；无答案注入 0，且分母至少 4）。

命令（`--exact` 必须带模块路径，否则滤不到这个测试）：

```text
export PATH="$HOME/Library/Application Support/mbx/bin:$PATH"
export OMLX_URL=http://127.0.0.1:8088
export OMLX_API_KEY="$(python3 -c 'import json,os; print(json.load(open(os.path.expanduser("~/.omlx/settings.json")))["auth"]["api_key"])')"
cd rust && cargo test -j 4 -p agent24-memory --lib eval_m1::eval_m1_omlx -- --ignored --exact --nocapture
```
