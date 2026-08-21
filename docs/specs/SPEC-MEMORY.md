# SPEC-MEMORY — M-D 记忆重做实现蓝图

> 权威决策见 [ADR-028](../decision.md)（分层模型）与 [ADR-029](../decision.md)（内核↔领域 OS 边界）。
> 本文是 **M-D 里程碑的实现蓝图**：层→trait→crate→分期→验收。设计首要属性：**可进化 / 可替换 / 可组合**（每层一个 trait 缝，可换实现、可加层，不动其余）。
>
> ⚠️ **2026-08-21 Codex 复审后修订**（见 ADR-028「Codex 复审收口」）：本蓝图正朝**"权威 + 投影"**重构——三个持久权威（`EventLog` / `ArtifactStore` / `AssertionLedger`）+ 可重建投影，"工作/情景/语义/程序"降为产品词汇。下方分层表是过渡视图；`Fact` 的双时相、`KernelCtx` handle、MD-1 定义已按收口修正，其余段落待 spike 后整体重写。

## 0. 设计不变量

1. **本地优先，零云硬依赖**：SQLite + 本地嵌入（oMLX）+ 可选本地向量；不引入 Neo4j / 外部向量服务。
2. **文件即真源、SQLite 即可重建索引**：L1/L4 的 markdown 为权威，索引可删可重建（`memory rebuild`）。
3. **领域 OS 无关**（ADR-029）：记忆是内核通用能力；Sin90/Cos72 用它，领域态留在各自 DB。
4. **作用域一等公民**：每条记忆带 `scope { user?, agent?, session?, run? }`。
5. **trait 缝优先**：先定 trait + 一个最简实现 + 测试，再谈优化。

## 1. 分层与 trait 缝

| 层 | crate | 核心 trait | 最简实现（M-D 内） | 后续可换 |
|---|---|---|---|---|
| **L0 KV** | `memory-core` | `KvStore`（已有） | SQLite | — |
| **L1 Working/Core** | `memory-core` | `CoreMemory { get_block, append, replace, apply_patch }` | KV-backed blocks（persona/prefs/focus） | markdown-backed |
| **L2 Episodic** | `memory-episodic` | `Condenser { condense(events, budget) -> Context }` | `RecentWindow` + `LlmSummary`（移植现 Summarizer） | mask / amortized-forget / attention |
| **L3 Semantic** | `memory-semantic` | `MemoryWriter { write(turn) -> Vec<Change> }` · `Retriever { search(query, scope, budget) }` | Writer=显式 put；Retriever=SQLite FTS | 两阶段 LLM writer；本地向量；edges 表 |
| **L4 Procedural/知识** | `memory-knowledge` | `KnowledgeSource { triggers(), inject(ctx) }` | markdown 文件 + 触发词匹配 | 语义触发；SkillBank |

`MemoryStore` facade（`memory-core`）组合各层，暴露给 agent loop / handlers；每层可 `None`（未启用）。

### 关键类型

```rust
struct Scope { user: Option<String>, agent: Option<String>, session: Option<String>, run: Option<String> }
// 真·双时相(Codex 收口修正):valid-time + recorded-time 两个区间,断言不可变、更新=新版本
struct Assertion { id, scope, kind, body: Value,
                   valid_from: String, valid_to: Option<String>,        // 领域有效时间
                   recorded_from: String, recorded_to: Option<String>, // 记录/事务时间
                   evidence: Vec<EventId>, confidence: f32, speaker: Option<String>,
                   writer_version: String, supersedes: Option<String> }
// 语义写=候选管线,不 mutate:CandidateExtracted→Validated→Approved→AssertionCommitted→Indexed
enum WriteDecision { Commit(Assertion), Reject{reason}, NeedsApproval }
struct Context { messages: Vec<Msg>, tokens: usize }                                                 // Condenser 产物
```

- **双时相**：更新一条事实 = 给旧的置 `invalid_at` + Add 新的；查询默认 `invalid_at IS NULL`，可按 `as_of` 时点回看。
- **Embedder**：`trait Embedder { embed(text) -> Vec<f32> }`，默认 `OmlxEmbedder`（走 agent24-models 的本地端点），可 `NoopEmbedder`（纯 FTS）。

## 2. 分期（跟消费者走，逐层独立可发）

- **M-D.1 — 评测/恢复 spike**（Codex 收口修正：不是只重命名 trait）：在现有事件流上实现**两个投影器**——确定性 `RecentWindow` + 保留尾部的 `LlmSummary`——**绝不删原始事件**，各记录 source event IDs + 模型/prompt 版本 + 安装的 checkpoint（对齐 codex compaction）。验收：现有 session 测试全绿 + **崩溃/重启/幂等重放测试** + 小语料测（token 预算 / 关键事实保留 / 工具-动作因果 / 投毒排除 / 跨 scope 泄漏 / 时间纠正）。**过了才冻结公开 trait、才决定是否拆 crate。**
- **M-D.2 — L1 CoreMemory**：block 存取 + `apply_patch`；agent loop 把 persona/偏好注入 prompt。验收：block 往返 + patch 幂等 + 人工编辑 KV 后 agent 读到。
- **M-D.3 — L3 Semantic（FTS 版）**：`Fact` 表 + 双时相 + `MemoryWriter`(显式) + `Retriever`(FTS + scope + 预算)；落一个"跨会话记住"消费者。验收：写-查-失效-as_of 回看四条 + scope 隔离。
- **M-D.3b — 本地向量（可选）**：`OmlxEmbedder` + SQLite 向量检索；两阶段 LLM writer。验收：语义召回优于纯 FTS 的对照。
- **M-D.4 — L4 知识/SkillBank**：markdown 权威 + 触发注入。验收：触发词命中→注入；`memory rebuild` 重建索引。

## 3. 与内核/领域 OS 的边界

- 记忆 crate **不依赖** sin90（保持内核领域无关，ADR-029）。
- 领域 OS 若需长期记忆，经 `KernelCtx` 拿一个**能力受限的 filtered handle**（限定其 scope/权限，**非 ambient `MemoryStore`**——否则 Sin90 DB 物理隔离被架空，Codex 收口）；领域事件仍走各自 DB + `EventBody::Module`。
- L2 Episodic 复用内核 runs/events 脊柱，不另造事件系统。

## 4. 不做（本里程碑）

- 图数据库、外部向量服务、跨设备同步（P4）、多 agent 共享记忆（后置，需消费者）。
