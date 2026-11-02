# Agent24 可进化性评估（最终版，2026-11-01）

索引：[架构总入口](README.md)・[ADR 索引](../adr/README.md)

> 对应 main `bb434b61`。这是三步评审的最终版：Opus 初评 → Codex gpt-6-astra 独立评估 → Fable 裁判对照代码整合。
> 输入：[Opus 初评](EVOLVABILITY-ASSESSMENT-2026-11-01-opus.md)、[Codex astra 评估](EVOLVABILITY-ASSESSMENT-2026-11-01-codex-astra.md)。
> 裁判只做静态核查（grep / 读源码），不构建、不测试。

## 结论

1. **能支撑持续进化**：daemon 本体 + 客户端外壳、trait 化的内核扩展点、fail-closed 审批与审计，这套底子够用，可以继续增量建。
2. **通信契约「外壳侧有、组件侧半有」**：客户端走 v1 REST/WS 带版本检查；进程外组件各走各的（HTTP / CLI JSON / JSON-RPC / attach socket），版本协商只有 iDoris、AgentEar/领域 OS（os-proto）、Knowledge 三处是真的。换同品牌升级版基本不影响别人，换不同品牌组件要写适配器。内部存储层更差：`SessionMemory` 直接拿具体类型，换数据库或检索实现会牵动调用方。
3. **自进化、记忆升级、训练自己的模型：三件都只有骨架**。记忆有可重建的派生索引和迁移脚本，但升级保护开关是关的；Skill 加载器没开工；ML worker 没有任何消费者，也没有训练接口。

## 裁决表

| 论点 | 来源 | 裁决 | 证据 |
|---|---|---|---|
| `migration_coordinator` `ENABLED=false`，生产启动不走它；`open_memory_base` 失败后无记忆继续跑 | Codex | **成立** | `rust/apps/agent24d/src/migration_coordinator.rs:20` `pub const ENABLED: bool = false`；`main.rs:111` 用 `const _: () = assert!(!ENABLED)` 钉死；`server.rs:521-537` 失败只 `tracing::warn!` 然后返回 `None`，`server.rs:2097` 直接接受 `None`。`MemoryConn::`/`StoreConn::` 在 agent24d 里除 coordinator 外零调用。 |
| `SessionMemory` 直接持有具体 `KvStore`/`SessionLog`，召回直接调 FTS | Codex | **成立** | `rust/crates/agent24-agent/src/session_memory.rs:60-71` 字段是 `kv: KvStore, log: SessionLog`，只有 `summarizer: Arc<dyn Summarizer>` 是 trait；`recall()` 第 168 行 `self.kv.retriever().search_any(...)`，`retriever()` 返回具体 `FtsRetriever`（`agent24-memory/src/lib.rs:576`）。`vector.rs` 的向量召回在 daemon/agent 里无调用方。 |
| memory crate 有约 16 个可替换 trait | Opus | **部分成立** | 数量对（16 个 `pub trait`，见 `agent24-memory/src/*.rs`），但**出了 crate 只有 `Summarizer` 被当 trait 用**。`Retriever`、`Embedder`、`MemoryWriter`、`KnowledgeBase` 等在 crate 外没有 `dyn` 引用，也没有第二实现（`Retriever` 唯一实现是 `FtsRetriever`，`Embedder` 生产实现只有 `HashEmbedder`）。trait 存在，但没在替换边界上起作用。 |
| 记忆没有版本化导出/导入和后台重建任务 | Opus | **部分成立** | 导出/导入：成立。`memory_routes.rs` 只有 list/search/retract/settings，无 export；memory 侧没有 schema-version 常量（只有 store 侧 `applied_schema_version`），靠 `migrations/0001…0020` 序列。重建：**不成立**，见下行。 |
| `FtsRetriever::rebuild` 可以重建索引 | Codex | **成立，但范围窄** | `retriever.rs:55` 有 `rebuild()`，`lib.rs:467` `rebuild_fts_if_needed` 在 `KvStore::open` 时按 `mem_fts_state.needs_rebuild='1'` 标记触发，有回滚测试。它只重建 FTS 投影，**不覆盖换 embedding 模型后重算向量**，也不是后台任务。 |
| 外部组件契约不统一 | 双方 | **成立** | iDoris：HTTP + `idoris.lock.json`（sha256、`contract_major/min_contract_minor`、`check_contract`）——**唯一**带语义版本协商的组件。Hyphae：子进程 CLI + JSON envelope，`hyphae.lock.json` 只锁二进制 sha256，**无契约版本字段**，依赖 Hyphae 文档约定。AgentEar：attach socket JSON-RPC，走 `os-proto` initialize，有 `protocol_versions` 区间协商和 `manifest_digest` 校验（Opus 定稿时更正：`attach_registry.rs:913` 的 `protocol_version: 1` 是测试数据；Agent24 不锁 AgentEar 二进制）；领域 OS 走 `os-proto/initialize.rs` 的 `version::negotiate`。Knowledge（WeKnora）：`agent24-knowledge` HTTP，`protocol_version` 常量 1，`capabilities.protocol_versions` 可协商。Documents：Rust 侧只有 `agent24-documents-client`（127.0.0.1 + bearer），**没找到锁文件或版本协商**。五个组件、四种传输；有握手协商的是 iDoris、AgentEar、Knowledge 三个。 |
| `agent24-components::fetch_verified` 依赖 iDoris 锁结构 | Codex | **成立** | `agent24-components/src/lib.rs:36-46` 用 `IdorisLock::embedded().clone()` 再改 `release_url_base/tag/assets` 伪造一把锁；注释明说缓存目录「must live inside an `idoris` tree」。Cargo 唯一内部依赖是 `agent24-idoris`。唯一调用方是 `hyphae_install.rs:35`——Hyphae 的下载被塞进 iDoris 的锁模型里。 |
| ARCHITECTURE-LAYERS.md 过时 | Opus（泛）/ Codex（点名） | **成立，Codex 的点名准确** | 文件头写「对应 `122fff7`」，最后改于 10-15。第 134 行「iDoris provider P4 接线未做（无 `IDORIS_URL`）」——现在 `agent24-models/src/router.rs:359` 已读 `IDORIS_URL`，`idoris.rs` 有完整 provider 和 `X-iDoris-Privacy` 头。第 137/301 行「retriever 尚未被 agent loop 消费」——`agent24-agent/src/lib.rs:1028, 2034` 已调 `recall`。第 51 行「worker 无消费者」仍然对。 |
| worker 有没有调用方 | 双方 | **无调用方，成立** | `agent24-worker` 只在 `rust/Cargo.toml` 列为成员；其余三处引用（`net.rs:590`、`tui/mod.rs:416`、`main.rs:4016`）全是注释。`MlWorker` trait 只有 `embed/transcribe/health`，无训练接口。仓库里没有 Python worker。 |
| Skill 加载器没立项 | Opus | **成立** | 代码里无 `SkillLoader/skill_loader/SkillRegistry`；issue #1037 状态 OPEN，标题自带「待立项」。 |

**双方共识**（都成立）：外壳 ↔ daemon 契约好；内核扩展点 trait 化；审批/审计/能力令牌是自进化的前提；训练与自进化缺闭环。
**Codex 独有且重要**：迁移保护未接线；存储层具体类型耦合；`fetch_verified` 寄生 iDoris 锁。
**Opus 独有且成立**：Skill/prompt/路由策略无版本管理；缺经同意的训练数据导出。

## 优点

- **外壳随便换**。`agent24-protocol/src/version.rs` 的 `PROTOCOL_VERSION` + `protocol_compatibility()`，桌面/CLI/TUI 都过这一道。
- **iDoris 是契约的样板**。锁文件同时钉制品哈希、契约 major/min-minor、能力清单；`check_contract` 拒绝不兼容版本。v0.3.1→v0.4.0 和 wire 1.0.1→1.1.0 都只改了锁和适配层。
- **领域 OS 和 Knowledge 有真协商**。`os-proto` 的 `version::negotiate` 和 `agent24-knowledge` 的 `protocol_versions` 都能拒绝不匹配的对端。
- **记忆原文与索引分离**。`SessionLog` 存事件，FTS 是可丢可重建的投影，`WriteGate` 原子提交。这给后面换检索算法留了路。
- **安全底座**：fail-closed 审批、哈希链审计、`capabilities/` 下的能力令牌与委托。系统将来改自己，有地方挂审批和回滚。

## 问题与风险（按严重程度）

1. **高：升级时记忆可能静默丢失。** 迁移协调器写好了、测了、但 `ENABLED=false`。现在每次发版（10-28 一天两次）store 和 memory 各自迁移，memory 打不开就 warn 一下无记忆跑。用户不会看日志，只会发现「它忘了我」。这是对「记忆升级」最直接的威胁。
2. **中高：内部存储替换比外部组件替换还贵。** `SessionMemory` 和 `RunManager` 拿具体类型，16 个 trait 里只有 `Summarizer` 真的在边界上。换 SQLite、换检索、接向量召回，都要动 agent crate 的调用方。trait 多不等于可替换。
3. **中高：五个外部组件四种接法。** 只有 iDoris 有完整的「锁 + 契约版本 + 能力」三件套。Hyphae 锁只锁二进制，Documents 是同仓库构建、不需要锁，但也没有版本协商；AgentEar 有握手协商，但 Agent24 不锁它的二进制。`fetch_verified` 还借 iDoris 的锁结构用。今天的后果：接一个新组件要再发明一遍下载、校验、握手；换不同品牌组件要重写适配层。这是「换一个不影响其他」最大的隐患，但它伤的是开发效率，不像第 1 条直接伤用户数据。
4. **中：记忆没有可移植格式。** 没有导出/导入，没有 memory 侧 schema 版本号。换 embedding 模型要重算向量时，`rebuild()` 不管这事。
5. **中：自进化没有起点。** Skill 加载器未立项；prompt、路由策略不是有版本的产物；`eval/` 只有 decide 一项，没接进改动流程。
6. **低（现阶段）：训练只有一个空接口。** `MlWorker` 无调用方、无训练方法、无 Python 实现。runs/tool_calls/audit 里有素材，但没有经用户同意的数据集导出。
7. **低：架构文档落后代码三周以上**，且错在关键处（iDoris 已接线、召回已消费）。读文档做设计会做错。

## 建议（按优先级）

1. **接线迁移协调器（Codex 第 1 条）。** 第一步：把 `server.rs:2097` 的 `open_memory_base` 换成 `migration_coordinator::run`，`ENABLED` 翻 true，删掉 `main.rs:111` 的断言；验收用跨库失败注入、崩溃恢复、以及 daemon 对外明确报「记忆降级」状态而不是 warn。
2. **写「组件契约 v1」ADR（Opus 第 1 条，排第二）。** 理由：第 1 条保护的是用户记忆。（Opus 定稿更正：它不只是翻开关。按 `docs/agent/CORE-5-MIGRATION-GUARD-DESIGN.md` §4，接线前还剩 5f 停靠与协议、5g 外壳识别、5h 安装器回退、5i 授权 retry 四片，是一条独立小工作线，可以和 ADR 并行。）ADR 要设计、评审、再逐个迁移，周期长，先把短而险的做掉。**范围覆盖**：进程外外部组件（iDoris、Hyphae、AgentEar、Documents、Knowledge）——统一清单（id、版本、契约 major/min-minor、能力列表）、锁文件格式（sha256，有签名时带签名）、握手时的版本协商、健康检查、错误码与取消语义、按组件发放的能力令牌。**不覆盖**：进程内 trait（`ModelProvider`、`Tool`、memory 各 trait），客户端 REST/WS v1（已有），内部存储替换，数据迁移格式，MCP（外部标准）。**不强推单一传输**：HTTP、CLI envelope、JSON-RPC 作为同一契约的三种绑定，不然 Hyphae 要重写。第一步：把 `IdorisLock` 抽成 `agent24-components::ComponentLock`，让 `fetch_verified` 不再借 iDoris 的锁；Hyphae 锁补 `contract_major/min_contract_minor`。迁移顺序 iDoris（对齐）→ Hyphae → Documents → AgentEar。
3. **从召回入口注入 `Retriever` trait（Codex 第 2 条）。** 第一步：`SessionMemory::new` 多收一个 `Arc<dyn Retriever>`，默认传 `kv.retriever()`；用一个 mock 第二实现跑通现有召回测试，证明边界成立。先不动 `KvStore`/`SessionLog`。
4. **记忆加版本与导出。** 第一步：memory 侧加 `MEMORY_SCHEMA_VERSION` 常量和 `GET /api/v1/memory/export`（JSONL，含 schema 版本）；向量重算先做成 CLI 子命令，不做后台任务。
5. **自进化从 Skill 开始，立项 #1037。** 第一步：定义 Skill 清单格式（id、版本、来源、哈希），加载走审批，改动留审计；prompt/路由策略先只做「有版本号、可回滚」，评测门槛以后再加。
6. **更新 ARCHITECTURE-LAYERS.md。** 第一步：改第 134、137、301 行和文件头 commit；worker 那行保留。训练本身按 Opus 第 4 条归 iDoris，Agent24 先只做经同意的数据导出，本轮不排期。

## 未核实项

- 外部组件的实际运行兼容性（没有起进程、没有跑握手）。
- Documents 组件的安装/锁是否在 TS 侧或 Desktop 侧实现；Rust 侧没找到。
- `agent24-sidecar-host-protocol` 只在 `frame.rs:308` 见到 `version: 1`，没核实是否有协商逻辑。
- iDoris 仓库那边有没有训练能力。
- 向量召回（`vector.rs`）的质量和是否值得接入，只确认了没接。
- `MEMORY-STRATEGY.md` 和生命周期 ADR 里的规划，一律不当作已实现。
