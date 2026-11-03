# Agent24 可进化性评估（Opus 初评，2026-11-01）

> 对应 main `bb434b61`。这是三步评审的第一步：Opus 初评 → Codex（gpt-6-astra）独立评估 → Fable 裁判整合。最终版见 `EVOLVABILITY-ASSESSMENT-2026-11-01.md`。
> 评估问题（jason 提出）：
> 1. 现在的结构能不能支撑 Agent24 持续进化？
> 2. 组件之间有没有标准的通信方式和协议，换掉一个会不会影响其他？
> 3. 面向未来能不能支持自进化、记忆升级、训练自己的模型？

## 结论

在「换组件」这件事上结构做得好；但自进化、记忆升级、训练自己的模型，目前只有接口骨架，缺关键闭环。

## 做得好的

1. **daemon 是本体，桌面、CLI、TUI 都是客户端。** 所有外壳走 v1 REST/WS，用生成的客户端（`packages/api-client`），有契约测试兜底。换外壳不影响内核。
2. **外部组件是独立进程，锁版本、校验 sha256、按需下载。**
   - iDoris（`rust/crates/agent24-idoris/idoris.lock.json`）、Hyphae（`rust/crates/agent24-comm/hyphae.lock.json`）、AgentEar、Documents 都是这样接入的。
   - 例子：iDoris 从 v0.3.1 升到 v0.4.0，只改了锁和适配层，没动其他组件。
   - iDoris 的 wire `contract_version` 从 1.0.1 升到 1.1.0，只做了加法。
3. **内核里可替换的点都抽成了 trait。**
   - 模型：`ModelProvider`。
   - 工具：`Tool`、`ModuleToolRuntime`。
   - 知识库：`KnowledgeBackend`、`KnowledgeTransport`。
   - 决策：`DecisionBackend`。
   - 硬件：`HardwareProbe`。
   - 记忆：约 16 个接口，包括 `Retriever`、`Embedder`、`Consolidator`、`Summarizer`、`MemoryWriter`、`KnowledgeBase` 等。
   - 领域模块：`DomainModule`、`KernelCtx`、`ScopedMemory`。
   - ML：`MlWorker`。
4. **安全底座齐全**：fail-closed 审批（policy）、哈希链审计（store）、能力令牌与提权（CORE-2）、通用模块工具契约（ADR-K1-01）。这是自进化的前提：系统改自己也必须经过审批、留下审计、能够回滚。

## 主要问题

1. **扩展方式太多，互不统一。**
   - L6 有四类外挂：进程内 DomainModule、进程外领域 OS、附着模块、sidecar。另外还有 MCP 和能力中心的组件安装器。
   - 每类都有自己的生命周期、授权和协议。
   - 版本协商只覆盖一部分：`agent24-protocol`、`os-proto`、`agent24-idoris`、`agent24-knowledge`、`sidecar-host-protocol` 有 contract/protocol version；Hyphae 走子进程加 CLI 契约，AgentEar 走自己的附着协议，没有统一握手。
   - 后果是换不同组件的代价差别很大。这是「换一个不影响其他」最大的隐患。
2. **记忆升级缺迁移通道。**
   - 记忆接口抽得好，但升级靠 SQL 迁移序列。
   - 如果换 embedding 模型（要重建索引）或换记忆结构，没看到有版本化的导出/导入格式，也没有后台重建任务。
3. **自进化目前为零。**
   - Skill 加载器（#1037）没有立项。
   - prompt、skill、路由策略这类「行为产物」没有版本管理。
   - 评测没有接进改动流程。
   - 现有的 eval 系统和审批流水线可以当基础。
4. **训练自己的模型只有接口。**
   - `agent24-worker` 只有 Rust 侧的 wire、client、mock，没有调用方，Python worker 也没实现。
   - runs、tool_calls、审计里有训练素材，但没有经过同意的数据集导出。
5. **架构文档过时。** `docs/ARCHITECTURE-LAYERS.md` 写于 10-03，K1、CORE-2、组件安装器之后的状态都没有更新进去。

## 建议（按优先级）

1. **写一份「组件契约 v1」ADR，把扩展方式收成一种。** 内容包括：
   - 清单：id、版本、契约版本、能力；
   - 锁和签名；
   - 握手时协商版本；
   - 健康检查；
   - 按组件发放的能力令牌。

   之后按 iDoris → Hyphae → AgentEar → Documents 的顺序逐个迁移。
2. **把记忆做成有版本的子系统。** 定义可移植的导出格式和 schema 版本，加上后台重建索引任务。换 embedding 或算法时走「导出 → 重建 → 校验 → 切换」。
3. **自进化从 Skill 开始。** 把 prompt、skill、路由策略当作有版本的产物，每次改动都走「评测打分 → 审批 → 灰度 → 可回滚」。系统不许静默修改自己。
4. **训练归 iDoris，Agent24 提供数据。** Agent24 负责经用户同意的数据导出；训练在 iDoris 或 worker 中进行；新模型在 iDoris 登记，Agent24 只按 TaskProfile 路由。
5. **更新 ARCHITECTURE-LAYERS**，反映当前的真实状态。
