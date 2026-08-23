# Agent24 M1 立项调研 — research

> 「为什么做、凭什么做」。
> 记录日期：2026-08-23。范围限于 M1（记忆成为产品），全局定位见 `../../CLAUDE.md` 与 `../decision.md`。

## 五步框架

### 1. 要解决的问题

**记忆底座交付了，但没接进去。**

逐模块数过（2026-08-22，`SPEC-ME-FOLLOWUPS.md` F2）：`agent24-memory` 的 12 个 M-D 模块 —— condenser / event / artifact / assertion / retriever / writer / consolidator / vector / knowledge / trace / replay / eval —— **crate 外引用数全为 0**。依赖这个 crate 的只有 `agent24-agent` 和 `agent24d`，而它们**只用 `KvStore` 和 `session::{CanonicalSession, CompactionPolicy, Summarizer}`**，全是 M-D **之前**就有的东西。

后果具体而非抽象：`CanonicalSession::save(kv)` 把会话存成 **KV blob**，所以**情节权威（EventLog）里根本没有对话** —— MD-1b 的崩溃重放对真实会话无从谈起。

> ⚠️ 里程碑记的是「M-D 全交付」。**交付 ≠ 在用** —— 164 条测试证明的是底座本身正确，不是它接进了 agent loop。这个区别本轮起写清。

### 2. 现有方案全景

| 方案 | 能力 | 可借鉴 | 备注 |
|:---|:---|:---|:---|
| **仓库内 `CanonicalSession`** | 按**消息条数**触发的摘要压缩，带 no-loss 保证（摘要失败不丢消息、下次重试） | **no-loss 保证必须保住** | 但把会话存成 KV blob |
| **仓库内 `Condenser`（MD-1a）** | 按 **token 预算**触发、策略**可换**（`RecentWindowCondenser` / `LlmSummaryCondenser`）、给出带 `covers(n)` 的 `ContextProjection` | 就是为取代前者而建 | **那次替换从来没发生** |
| LangChain / LlamaIndex 的 memory | 会话缓冲 + 摘要 | 概念对齐 | 与本仓库的事件溯源模型不同构，不引入 |
| MemGPT / Letta 的分层记忆 | 分页式上下文管理 | 分层思路（已体现在 ADR-028） | 重量级，且是 Python 生态 |

### 3. 差异化立足点

**不是「再造一个 memory 库」，而是把已有的、经过 20+ 轮对抗复审的底座接进 loop。**

这个仓库真正稀有的东西不是记忆算法，是**归属与隔离被逐表探针证明过**：F1 的跨 owner 探针、F8 的 (org, space) 维度、目录驱动的 re-key。市面上的 agent memory 方案几乎都停在「能存能查」，没人回答「两个领域 OS 挂在一个人下面会不会互相污染」。

### 4. 可复用 vs 要自建

| | |
|:---|:---|
| **复用**（本仓库已有，本轮只接线） | `Condenser` 及其两个策略、`EventLog`、`replay`、`rekey_os_partition` 的事务化搬移、`mem_os_partitions` 目录、`pool_migrated_up_to` 迁移测试夹具 |
| **自建**（本轮新增，都很小） | `Authorizer` 判定接缝（约 200 行）、`SpaceId::personal`、迁移 0014、会话轮次→事件的接线 |

**本轮不写任何新的记忆算法。** 所有「新代码」都是接线、判定点、和一次数据搬家。

### 5. License / 合规边界

无新增第三方依赖，因此无新增 License 面。

合规上唯一要说清的是 **ADR-030 UC8 已记录的事实**：一旦空间有多个成员，「导出属于某人的一切」在共享空间里**没有答案**。本轮是单用户，不触发该问题，但**不得**在文档或 PR 里出现「GDPR 就绪」这类措辞 —— F1 复审已把它列为禁语。

## 结构性空白（差异化）

生态里大量 agent 框架有「记忆」，但把**记忆归属**当一等问题、并且在**零数据窗口内**把所有权维度定对的，基本没有。F8 已经做掉那一步；M1 是让它开始产生价值 —— 否则那五轮复审保护的是一个没人用的底座。

## 结论

**做，且只做接线与归属两件事。**

从 F1.1（判定接缝）切入而不是直接从 F1.3（接线）切入，是因为判定点的**调用点**会随 F1.3 增多，事后补贵一个量级；从 F1.2 而不是直接 F1.3，是因为 agent loop 的记忆分区**已排定要移动**，先接线等于建在流沙上。

第一个里程碑指向的是 acceptance.md 第一条：**「我的对话被真的记住了」**。
