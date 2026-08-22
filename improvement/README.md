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
- [x] ✅ **`mem_checkpoints` 的 owner 维已修**(PR #128,已合入 main):migration 0010 改 `PRIMARY KEY (scope_owner, name)`、三个 API 加 owner、`checkpoint()` 改用该 owner 自己的 max(旧实现用全表 `MAX(seq)`,是同一缺陷的第二半)。3 条回归测试。历史行故意丢弃(书签可重建,重扫优于静默丢失)。
- [ ] 判定 **`kv` 的 namespace-vs-owner 隔离模型**:是否需要把 namespace 绑定到 owner,或改用 owner 维。
- [ ] 收紧 `mem_events` 的 `scope_owner` CHECK 到 `trim(...)`(需新迁移;0002 已发布不可改)。
- [ ] **领域 OS 之间的记忆隔离/共享模型**:Sin90 与 Cos72 各自挂载时,底层 memory 是**同库不同 scope**、**不同库**、还是**可选共享的显式通道**?给出方案对比 + 推荐 + 污染面分析。
- [ ] 落地 ADR-029 里那个**尚未实现**的洞:`KernelCtx::memory(scope, grants) -> ScopedMemory`(能力受限句柄,不是 ambient `MemoryStore`)。这是隔离能否成立的关键,属 M-E(ME-1)范围。
- [ ] **模块目录的 symlink 安全**:ME-1b-a 的挂载器只做了「目标已是 symlink 就**不用它、把该模块降级为 503**」的**检查**(catch 掉真实会发生的那种:`~/.agent24/os/cos72` 软链到 sin90 目录 = 两个 OS 共用一个库),但**不是保证** —— `root` 的任一祖先仍可以是软链,且 `symlink_metadata` 到 `create_dir_all` 之间存在 TOCTOU。真正的隔离需要 `openat` 式目录句柄(`cap-std` / `openat2`)。契约文字已同步收窄,不再声称做到了。
- [ ] **领域 OS 命名空间应否改为 `/api/v1/os/<name>`**:现在是 `/api/v1/<name>`(ADR-029 / SPEC-MD-ME §2 钉死),后果是**模块名可能撞上内核路由段**——axum 对精确路由重叠会 **panic**,即一个第三方 OS 取名 `health` 就能让 daemon 起不来。ME-1b-a 用「保留段名单 + 从 `build_router` 源码反推的防过期测试」挡住了,但那是**检查**;`/api/v1/os/<name>` 会让这类冲突**不可表达**。属于要改 ADR 的架构决定,列此待判。
- [x] ✅ **Sin90 数据迁移已实现**(ME-1b-b):`Sin90Store::open_migrating_from` —— 目标**存在且已初始化**则永远赢(降级后再升级不会被旧库覆盖);**光有文件不算数** —— 零字节/半建的目标库、以及「合法 SQLite 但不是 Sin90」或「迁移未完成」的旧库,全部**拒绝并给出可操作提示**(且明确叫用户**不要删**,因为那个文件可能比旧库更新),因为 `quick_check` 只证明完整性、从不证明身份、`VACUUM INTO` 出一致快照(**不是**搬 `.db`/`-wal`/`-shm` 三个文件:搬不成原子,且崩溃后已提交数据可能只在 WAL 里)、`quick_check` 后原子 rename、失败返回 `Err` 让模块降级而**不是**掉进 `create_if_missing` 产出一个能用但空的 Sin90、旧库原地留作回滚快照。9 条测试,并用变异验证过(把 `VACUUM INTO` 换成朴素 `fs::copy` → 三条测试失败,其中 WAL 那条直接是 `left: 0`)。**降级后不可写同步**仍是已知限制(不做软链,WAL 锁与路径绑定)。
- [ ] 给出**跨 OS 共享**的显式机制设计(若确实需要):共享什么(assertions? artifacts?)、谁授权、怎么审计、怎么撤销。

**今日事实基线(不是结论,是现状快照;经 #126 复审逐表核对后订正)**:
- 记忆层 **11 张表里 10 张**有 `scope_owner` 且强制非空,读写路径 owner-scoped(#115/#119/#122/#123/#124/#125 六轮复审逐条打过跨 owner 探针)。**一个已修 + 一个待判**:
  - ✅ **`mem_checkpoints`(已于 PR #128 修复,下述为修复前状态)完全没有 owner 列**,且 API 也没有 owner 参数(`checkpoint_at(name, seq)` / `checkpoint_seq(name)`)—— **两个 owner 用同名 checkpoint 会共享同一行**,一方推进会让另一方的增量扫描跳过事件。**形状与 #119 的 `retract` 一模一样**(没有 owner 参数,所以没有任何 `WHERE` 能救),那六轮复审各自只打了它们触碰的表,**这张一次都没被碰到**。→ 已修,见 TODO-A 第一条。**基线现为 10/11**(仅剩 `kv` 用 namespace 模型待判)。
  - ⚠️ **`kv`(L0)用 `PRIMARY KEY (namespace, key)` 隔离,即 namespace 而非 owner** —— 是**另一套隔离模型**,不一定是缺陷,但正该由 TODO-A 判断:namespace 是否由 owner 派生?两个 OS 挂同一 owner 时会不会撞 namespace?
- `Scope` 有 `owner / agent / session / run` 四维,但**只有 owner 维在存储层强制**;`agent` 维目前**未被任何查询使用**(全仓 0 处 `scope_agent` 列)—— 这正是「不同 OS 挂在同一 owner 下会不会互污」的关键缺口。
- `ScopedMemory` / `KernelCtx` **尚未实现**(SPEC §2 只有契约草案;ME-1a 契约 crate 在 **PR #127,尚未合入**;ME-1b+ 才接内核)。
- ⚠️ **非空 CHECK 强度不齐**:`mem_events` 是 `CHECK(scope_owner <> '')`,其余八张是 `CHECK(trim(scope_owner) <> '')` —— **纯空格 owner 在 `mem_events` 过得去,在别处过不去**。(0002 是已发布迁移不可改,需新迁移收紧。)
- ✅ **一个负载相关的偶发失败已定位并修复**(2026-08-22):`cargo test --workspace` 满载时出现过 `163 passed; 1 failed`,重跑 14 次全绿。**没当它没发生过**——记忆层是所有 OS 共用的地基,1/164 的偶发等于生产的偶发。追下去是 `consolidator::tests::incremental_equals_full_rerun`:它**分别建两个库**,事件时间戳取墙钟秒,而 `Consolidation.at` 是源事件 `at` 的 max —— 两半只要跨秒,"两个语料相同"的前提就不成立。**是测试的缺陷,不是 MD-5 的**(实现本来就不用墙钟)。已改为固定时间戳,并额外钉一条 `at` 断言,使得将来若有人改成用墙钟会**直接失败而不是重新变飘**。教训:**任何"A 等于 B"的测试,先确认 A 和 B 真的由同一份输入构造**。
- ⚠️ **`agent24-store` 有五处列表查询只按秒级时间戳排序,没有 tie-breaker**(2026-08-22,Codex 在复查上一条时顺带扫出;**当前没有测试会踩到,所以不是现存 flake,但是同一个隐患的另一半**):`repo.rs:117` sessions、`repo.rs:165` runs、`repo.rs:321` tool calls、`repo.rs:408` approvals、`repo.rs:639` standing grants —— 同一秒内创建的多行**返回顺序未定义**,对调用方(和将来任何断言顺序的测试)都是不确定性。对照:`agent24-sin90-store` 已经用 `rowid` 兜底(`spike00.rs:475` 有专门的同秒顺序测试)。待办:给这五处补 `id`/`rowid` 次序键。

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
