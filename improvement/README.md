# improvement/ — 持续架构改进与外部借鉴跟踪

> **状态:🅃🄾🄳🄾 —— 已建容器,尚未开工。** 用户 2026-08-22 明确:先记录、不执行,等 milestone 告一段落再开始。
>
> 目的:让「借鉴业界最先进 agent」从**一次性的开头调研**变成**长期维护的子项目**。用户原话:「不是只在开头时候借鉴，到后边就忘了这事儿」「我认为你做这个任务做的不透彻，要持续的跟踪汇报」。

---

## 0. 这个目录怎么用

```
improvement/
  README.md          ← 本文:章程 + TODO 台账(唯一状态源)
  inbox/             ← 用户投喂:新文章 / 新架构 / 新 skill / 新工具(原文或链接 + 一句话来源)
  analysis/          ← 我的产出:每篇/每仓库一份分析,含「值不值得改、怎么改、不改的理由」
  decisions/         ← 结论沉淀:改了什么(链到 PR/ADR)、明确不改的存档
```

**投喂 → 处理的约定**:用户把材料丢进 `inbox/`,我在下一个改进批次里逐篇处理,产出 `analysis/<slug>.md`,结论三选一:
1. **值得改 + 现在改** → 开 ADR / PR,链接回本表;
2. **值得改 + 现在不改** → 记入「延后清单」并写明触发条件(等哪个 milestone / 等哪个依赖);
3. **不值得改** → 也**必须归档**并写明理由(用户原话:「即便当下不值得动工更改，那也把这个文章记录下来，如果他确实未来可能用得到的话」)。

---

## 1. TODO-A:架构自检(可插拔 / 可组合 / 领域 OS 隔离)

用户诉求原文:「持续的分析我们自己的架构，是不是可拔插可组合」「我切换不同的 Sin90 或 Cos72,它们之间的 RAG 数据库或底层 memory 是不是可以相通的?或者说用什么方式可以把它们两个相同，但本身是独立的，总不能相互污染。这是我一个诉求，我要你 check 确认」。

**要交付的结论(尚未做)**:
- [ ] 逐层核对 M-D 记忆底座的**可插拔缝**是否真的可换实现(不是名义上的 trait):`Condenser` / `EventStore` / `ArtifactStore` / `AssertionStore` / `Retriever` / `Embedder` / `InsightSynth` / `MemoryWriter` / `KnowledgeBase` / `TaskTrace`。
- [ ] 🔴 **修 `mem_checkpoints` 的 owner 维**(已知缺陷,见下方基线):加 owner 列 + API 加 owner 参数 + 跨 owner 测试。**优先级高于其余自检** —— 这是已在 main 上的真实跨租户缺口,不是设计问题。
- [ ] 判定 **`kv` 的 namespace-vs-owner 隔离模型**:是否需要把 namespace 绑定到 owner,或改用 owner 维。
- [ ] 收紧 `mem_events` 的 `scope_owner` CHECK 到 `trim(...)`(需新迁移;0002 已发布不可改)。
- [ ] **领域 OS 之间的记忆隔离/共享模型**:Sin90 与 Cos72 各自挂载时,底层 memory 是**同库不同 scope**、**不同库**、还是**可选共享的显式通道**?给出方案对比 + 推荐 + 污染面分析。
- [ ] 落地 ADR-029 里那个**尚未实现**的洞:`KernelCtx::memory(scope, grants) -> ScopedMemory`(能力受限句柄,不是 ambient `MemoryStore`)。这是隔离能否成立的关键,属 M-E(ME-1)范围。
- [ ] 给出**跨 OS 共享**的显式机制设计(若确实需要):共享什么(assertions? artifacts?)、谁授权、怎么审计、怎么撤销。

**今日事实基线(不是结论,是现状快照;经 #126 复审逐表核对后订正)**:
- 记忆层 **11 张表里 9 张**有 `scope_owner` 且强制非空,读写路径 owner-scoped(#115/#119/#122/#123/#124/#125 六轮复审逐条打过跨 owner 探针)。**两个例外**:
  - 🔴 **`mem_checkpoints` 完全没有 owner 列**,且 API 也没有 owner 参数(`checkpoint_at(name, seq)` / `checkpoint_seq(name)`)—— **两个 owner 用同名 checkpoint 会共享同一行**,一方推进会让另一方的增量扫描跳过事件。**形状与 #119 的 `retract` 一模一样**(没有 owner 参数,所以没有任何 `WHERE` 能救),那六轮复审各自只打了它们触碰的表,**这张一次都没被碰到**。→ 已列为 TODO-A 的**已知缺陷**,单独修。
  - ⚠️ **`kv`(L0)用 `PRIMARY KEY (namespace, key)` 隔离,即 namespace 而非 owner** —— 是**另一套隔离模型**,不一定是缺陷,但正该由 TODO-A 判断:namespace 是否由 owner 派生?两个 OS 挂同一 owner 时会不会撞 namespace?
- `Scope` 有 `owner / agent / session / run` 四维,但**只有 owner 维在存储层强制**;`agent` 维目前**未被任何查询使用**(全仓 0 处 `scope_agent` 列)—— 这正是「不同 OS 挂在同一 owner 下会不会互污」的关键缺口。
- `ScopedMemory` / `KernelCtx` **尚未实现**(SPEC §2 只有契约草案;ME-1a 已落契约 crate,ME-1b+ 才接内核)。
- ⚠️ **非空 CHECK 强度不齐**:`mem_events` 是 `CHECK(scope_owner <> '')`,其余八张是 `CHECK(trim(scope_owner) <> '')` —— **纯空格 owner 在 `mem_events` 过得去,在别处过不去**。(0002 是已发布迁移不可改,需新迁移收紧。)

---

## 2. TODO-B:心智模型校准(底座 + OS = 不同 agent)

用户原话:「如果说 Agent24 是一个底座，是一个硬件底座的话，那么配备了不同 OS 的 Agent24 就是不同的真正的活起来的 agent。底座是一个，但是配备不同的操作系统，它可能转换为了不同的 agent。你觉得这样理解的模型，对我们自己的描述准确不准确?」

- [ ] 把这个心智模型与 ADR-029(内核↔领域 OS 边界)对齐,写成一份可对外表述的**架构叙事**(README/官网可复用),并标出模型**成立的前提**与**当前尚未成立的部分**。

---

## 3. TODO-C:先进 agent 深度调研(长期子项目,每仓库一个独立任务)

用户原话:「另外一些最先进的 agent，我认为你并没有深入调研和借鉴。比如说 Pi agent，比如说 Claude Code，比如说 Codex CLI，这些我需要你透彻的去分析理解它，每一个仓库的分析当成一个独立的任务，透彻的分析理解它，然后对比我们的架构，借鉴他们的先进的架构思路，甚至一些具体的代码。」

| # | 目标 | 状态 | 产出 |
|---|---|---|---|
| C1 | **Claude Code** | ⬜ 未开始 | `analysis/claude-code.md` |
| C2 | **Codex CLI** | ⬜ 未开始(前期只读过 memory 相关模块,**不透彻** —— 用户点名批评) | `analysis/codex-cli.md` |
| C3 | **Pi agent** | ⬜ 未开始 | `analysis/pi-agent.md` |
| C4 | 后续用户指定 | ⬜ | |

**每个任务的完成标准**(避免重演「浅尝辄止」):
1. 克隆到 repo 外(`~/Dev/auraai/agent24-*-research/`,**不提交**);
2. **真读代码**,分析要带 `file:line`;
3. 覆盖:整体架构 / 会话与上下文管理 / 工具与权限模型 / 记忆与持久化 / 扩展机制(plugin/skill/MCP)/ 错误与恢复 / 测试策略;
4. 逐条对比 Agent24 当前实现,输出**可执行的借鉴项**(哪个文件、改什么、值不值);
5. 结论进 `decisions/`,并在本表更新状态 + 日期。

---

## 4. 已完成的借鉴(存档,防止「后边就忘了」)

M-D 阶段已落地的借鉴(源:`agent24-memory-research/RESEARCH-REPORT-v2.md`,18 仓库读源码):
codex(trait 形状/路径安全)· goose(TokenEstimator/hide-not-delete)· OpenHands(Condenser view-delta)· basic-memory(双谱系对账)· graphiti(双时相)· Dense-Mem(写门)· memobase(巩固)· gemini-cli(审核门控 inbox)· aider(预算渲染)· TencentDB(符号轨迹)· cognee(确定性 id)。
详见 `docs/specs/SPEC-MD-ME.md` §0.4 借鉴映射表(**在本仓库内,可跟随**)。
> ⚠️ 原始调研报告 `agent24-memory-research/RESEARCH-REPORT-v2.md`(18 仓库读源码)按本章程约定放在 **repo 外、不提交**,因此**不可跟随**。11 条已落地借鉴的**结论摘要**待沉进 `decisions/`,避免那个目录一旦丢失、第 4 节只剩一串名字。→ 列入 TODO-A。

**已评估但明确不采纳(带理由)**:
- **ColBERT / late-interaction 多向量检索**(sentence-transformers v6):不作主索引 —— ~40× 存储 + MaxSim 需 Qdrant/Weaviate/Vespa/Milvus 原生支持 = ADR-028 §0.1 禁止的强制外部向量服务;且在语义聚合任务上会输。**采纳其两段式** recall→rerank,留作 MD-6x `Reranker` 缝(与 `OmlxEmbedder` 同挂 D4b)。已记入 SPEC 借鉴映射表。

---

## 5. 开工条件

用户 2026-08-22 指令:**「等 milestone 都搞一阶段之后再开始」**。当前 M-D 主线 MD-1..8 已交付;M-E 尚未开始。开工前请用户确认批次与优先级。
