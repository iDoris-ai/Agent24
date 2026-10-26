# Agent24 整体架构与技术方案（2026-10-26）

> **地位**：自 2026-10-26 起，这是 Agent24 架构与技术方案的**唯一入口**。与之配套的另两份跟踪文档：
> [里程碑与执行计划](MILESTONES-2026-10-26.md)、[进度](PROGRESS-2026-10-26.md)。
> 三份文档以后只做增量修订（文首记修订日期），不再另起同类文档。
>
> **本文怎么写的**：对 main `3a433bb0` 与 9 条集成分支上的 114 份架构、设计、ADR 文档逐份核对过代码，判定每份是现行、被取代、历史记录还是与代码不符。逐份记录和证据见附录 [SOURCE-C1](tracking/2026-10-26/SOURCE-C1-architecture-docs.md)。本文只写结论和裁决。被取代的旧文档**不删正文**，只在索引里标状态并指向替代者。历史上正确、但只写在旧文档里的约束，收在第 7 节。
>
> **待评审部分**：第 1 节的产品形态与进程模型来自 ADR-037（提议，走单独评审）。ADR-037 批准前，第 1 节标 🟡；批准后按评审结论修订。

状态标记：✅ 已在产品路径上运行；🟡 已实现但未接入产品路径，或是提议、待评审；📐 只有设计。

---

## 1. 产品形态（ADR-037，🟡 待评审）

jason 2026-10-26 拍板的产品方向：

- **agent24d 是产品本体**：Rust 常驻进程，提供全部能力，通过 v1 REST/WS 暴露，`protocol/openapi.yaml` 是契约。
- **外壳都是客户端**：
  - **桌面**：Electron。先发 Mac，后端适配完成后再发 Linux、Windows。
  - **TUI / CLI**：用于开发和恢复测试，按能力与桌面对齐。纯视觉功能给「在桌面或浏览器打开」的兜底。
  - **移动端**：只做遥控（交互、审批、控制、基础预览），通过 Hyphae 连到用户自己的 daemon，不直连 HTTP。
  - **开发者**：直接调用本机 HTTP API。
- **每个用户一个常驻 daemon**：由用户级服务托管（macOS LaunchAgent / Linux `systemd --user` / Windows 登录计划任务），生命周期不跟随任何外壳。
- **客户端分三级**：
  - 一方本机外壳（桌面、TUI、CLI）：经本机登记握手拿到只属于本进程的短期令牌。登记时校验身份，macOS 要求代码签名的 Team ID，Windows 用 Authenticode，Linux 看安装路径归属。签名的一方外壳用户无感；开发构建要用户确认一次。不设共享宿主秘密文件。
  - 第三方开发者应用、Creative sidecar、插件：只能持有具名、分权、可吊销的令牌。
  - 远程设备（移动端）：走 Hyphae 配对，按作用域授权，作为独立的授权入口，不改变通信层「收到消息不触发 run」的约束。
- **开发者 API 对外宣传前必须过五道门（G-API）**：路由级资源授权、Host/Origin 校验、OpenAPI 与路由一致性 CI、事件流按令牌过滤与复验、令牌治理（只存 hash、TTL、吊销、限流配额、审计）。
- **生命周期**：常驻服务模式不依赖父进程管道；外壳与 daemon 退出时，sidecar、附着模块、领域 OS、模型 lease、run 各归谁管，有一张归属表；接入时做版本协商；迁移前自动备份，失败回滚。详见 ADR-037 决策 2。

与现状的差距（证据见 [SOURCE-ADR-037-facts](tracking/2026-10-26/SOURCE-ADR-037-facts.md)）：

| 方向 | 现状 | 差距 |
|---|---|---|
| 常驻 daemon | 单例锁、`~/.agent24/daemon.json` 发现文件、桌面复用外部 daemon、`agent24 service install`（仅 macOS）都已有 ✅ | 桌面默认把 daemon 当子进程，退出即 SIGTERM；无 systemd、无 Windows；TUI 连不上就起 `--ephemeral` 内存库。ab/weknora 的 #1184（KB-HOST-01 PR-A2）与 OD-M11 F11.1 让桌面经 stdin 管道拥有 daemon，与常驻方向相反，需按 ADR-037 改 |
| 分权令牌 | capability 令牌库（mint、吊销、TTL、代际、22 个 Operation）🟡 | `required_operation` 只区分 3 类路由，资源一律按全局处理；宿主令牌只能经父进程 stdio 管道拿到（且仅 Unix 实现）；默认是全权 legacy token |
| 浏览器防护 | 只绑 127.0.0.1 ✅；WS `/events` 拒绝带 Origin 的请求 ✅ | REST 无 Host/Origin 校验；桌面 `backendProxy` 替渲染进程把任意路径带上全权 token 转发 |
| TUI | Runs、Approvals、事件流 ✅ | 无聊天输入；OpenAPI 75 个操作中约覆盖 26 个 |
| 跨平台 | Mac arm64 签名+公证预发布 ✅；Linux 桌面 AppImage/deb ✅（v0.5.1） | `os-fd`、`os-cwd`、`workspace` 依赖 Unix；运行时只有 oMLX 与 Ollama；钥匙串不支持 Windows；发布矩阵无 Windows |
| 移动端 | 无 | 遥控设计 `REMOTE-CONTROL-HYPHAE.md` 未写；ADR-027（HTTP 直连）与 Deployment（PWA 加 tailscale）被 ADR-037 取代 |

---

## 2. 分层与组件（以代码为准）

工程结构：`rust/crates` 22 个 crate、`rust/apps` 4 个 app（agent24d、agent24-cli、agent24-documents、agent24-sidecar-host）、`apps/desktop`（Electron）、`packages/`（api-client、contract-tests、documents-accept、node-daemon、nostr-bridge、wechat-bridge）、`protocol/`（openapi.yaml 手写真源、events.schema.json 由 Rust 导出且有零漂移门、module.schema.json）。

```
L7 外壳     apps/desktop（Electron main：BackendManager ✅ · CreativeServeWeb ✅ · SidecarManager 🟡 ·
            idoris-admin（ADR-035 FD 交付）✅ · documents preload ✅）· agent24-cli（chat/tui/acp/os/comm/service/hw/decide）
            · packages/*（api-client 类型、contract-tests、nostr/wechat 桥、node-daemon = 契约测试用参考实现）
──────────── v1 REST/WS（默认 legacy 全权 token；capability 模式 🟡） ────────────
L6 扩展面   ① 进程内 DomainModule 🟡（无生产模块）② 进程外领域 OS ✅（Sin90 / Cos72 / Documenting）
            ③ 附着模块 ✅（AgentEar，A3）④ Sidecar（Open Design：现行 CreativeServeWeb+ACP ✅，目标 SidecarManager 🟡）
            · MCP 工具 ✅ · 模块工具 ✅（K1，`<module>.<op>`）· 知识 provider 🟡（ab/weknora）
L5½ OS 运行时 os-proto · os-fd · os-packages · os-sdk（模块作者 SDK，0.1.0 独立版本线）
L5 组合根   agent24d：v1 API、模块挂载与监管、附着注册、审批/调度/推理/记忆回调、consent 与模块审批（K1）、
            comm 路由（Hyphae）、hardware.json、capabilities/（🟡）
L4 内核     agent24-agent：run 生命周期与 agent loop、session_memory（跨会话 recall ✅）、来源门（R05，ab/memory-p1）
L3 能力服务 models（ModelRouter：oMLX + Ollama；TaskProfile 路由）· tools（流水线）· policy（fail-closed 审批 +
            consent_gate + write_scope）· memory（M-D）· scheduler · mcp · comm（Hyphae）· decide 🟡 · hw ✅ ·
            worker 🟡（无消费者）
L2 持久化   store（SQLite + 哈希链审计，迁移到 0018；ab/memory-p1 到 0022）· workspace（仅 Unix）· os-cwd
L1 纯核心   agent24-core（零框架依赖状态机）
L0 契约     protocol · domain · sidecar-host-protocol · protocol/*.yaml|json
```

本图修正了 `docs/ARCHITECTURE-LAYERS.md` 的几处过期说法（那份仍是分层细节的权威，但要按下面几点读）：

- retriever 已被 agent loop 消费（`session_memory.rs` 调 `kv.retriever().search_any`）；M1 已于 2026-10-07 合入 main（#720）；`ab/m1-memory` 已不存在。只有 consolidator 仍无消费者。
- LAYERS 漏写了：`agent24-documents` 与 `packages/documents-accept`；K1 的模块工具运行时、consent、模块审批；迁移 0014–0018；`policy/write_scope.rs`；桌面的 `idoris-admin.ts`、documents preload。

---

## 3. 现行技术决策（按主题）

每条都注明出处。完整 54 条及其核验见 [SOURCE-C1 §3](tracking/2026-10-26/SOURCE-C1-architecture-docs.md)。这里只列骨架。

### 3.1 进程、部署与开发流
- Rust agent24d 是唯一核心运行时，只绑 127.0.0.1，端口动态分配（ADR-026 ✅）。常驻与客户端模型见 ADR-037（🟡）。
- 单 daemon、单 SQLite、单用户 `LOCAL_USER`；org/space 跨库属 ADR-030 硬门槛，不碰（`docs/agent/architecture.md`）。
- 领域 OS 与模块全部进程外：manifest + 握手 + 回调 socket（UDS，fd 3）+ 受约束代理（ADR-029 修订、ADR-031、SPEC-ME3、FU-60）。
- 大组件按需下载、不进安装包：pin 版本 + sha256、原子落地（Open Design 10-05 裁决；ADR-DOC-01 D6；DEP-A9）。
- 开发流：`ab/<方向>` 集成分支 → task PR（≤300 行）→ release PR（人开、人审、真机验收）；集成分支每周至少吸收一次 main（AGENTS.md）。

### 3.2 协议与 API
- v1 REST + WS；OpenAPI 手写为权威；路由与 OpenAPI 有覆盖门（M2-03 #737）。由 Rust 类型生成 OpenAPI 尚未开始（K-3 / SPEC-002 B4）。
- 审批是请求-响应，流式是单向通知，`approval.required` 携带 `available_decisions`（ADR-026 §6.5）。
- 模块协议是自定义的窄 JSON-RPC 回调，不用 ACP/MCP；ACP 只用于 Open Design 桥（ADR-031、ADR-003）。
- 模块路由：`/api/v1/<name>` 加保留段名单；`/_a24/` 为内核保留（F3 裁决，2026-10-07）。
- 模块工具：`<module_id>.<operation>`、11 个错误码、不自动重试、`result_unknown`（ADR-K1-01 ✅）。

### 3.3 存储与审计
- SQLite（sqlx，WAL）加哈希链审计。**ADR-017 的「加密 SQLite」没有实现**，应改标（见 §5）。
- 决策日志由 agent24d 实现（ADR-034）。模块业务数据只存在模块自己的 SQLite 里。
- 审计只记元数据，留存 180 天，用户可查看和导出（ADR-K1-04 ✅）。

### 3.4 记忆
- 权威数据加可重建投影：EventLog（情景）、ArtifactStore、AssertionLedger（双时态）；FTS、向量、摘要都可重建（ADR-028 收口）。
- 所有权按 (org, space)，只用不可变 ID 做 key；执行点在发放句柄时；模块只拿 `ScopedMemory`（ADR-030 ✅，laws/MEMORY L-MEM-1..7）。
- 正常生命周期不丢数据；只有用户明确确认的 purge 是破坏性例外。首版 purge 只覆盖全个人域；恢复包默认加密，并在线防回滚；在途发送和外部副本如实标 partial；无痕模式首版零工具，只用单一本地 provider（生命周期 ADR，§9 于 2026-10-26 拍板，#1247 ✅ 接受，在 ab/memory-p1 上）。
- 召回消息的来源：独立表 `run_message_origins`（迁移 0021/0022），与 K1 的 `run_source_tags` 正交；发送前核对来源，无法证明就拒绝发送；发送副本按来源 kind 整形；新增 HostSystem kind（R05-1..5，ADR-MEMORY-RECALL-MESSAGE-ORIGINS ✅ 接受于 #1248，在 ab/memory-p1 上）。

### 3.5 模型与路由
- ModelRouter 按 `TaskProfile{Privacy, Complexity}` 在 Local / Remote / Lora 间路由，`LocalOnly` 对远端 fail-closed。`from_env` 只构造 oMLX（:8088）和 Ollama；主对话目前仍用 `TaskProfile::default()`。
- 硬件探测与整机内存账归 Agent24（agent24-hw）；模型选型归 iDoris；分 1–6 数字档；云端补位默认关闭，且必须由路由器强制（ADR-036 ✅，目前只落地了 HW-1）。
- iDoris 以 OpenAI 兼容 provider 接入（idoris-local / idoris-any），不做成模块（ADR-032）。provider 代码只在 ab/idoris 上。
- ADR-022 的「默认 MLX」收窄为「Apple Silicon 上默认」，其他平台的默认运行时由 iDoris 按档位选（ADR-037 🟡）。

### 3.6 工具、审批与策略
- 工具流水线：normalize → 能力白名单 → 审批门 → 超时执行；MCP 工具走同一条（ADR-026 §6.5）。
- 审批 fail-closed：`approve_for_session` 只对 Read 风险提供；`exec` 每次都要问；根外 `fs_write` 按目录授权（`policy/lib.rs`、`write_scope.rs`，#1117–#1120）。laws/APPROVAL 中 L-APPR-5/8 的「今天靠什么保证」一栏已落后于代码（见 §5）。
- 模块启用授权由宿主签发，逐工具、有期限、可撤销；安装或签名不等于同意（ADR-K1-03 ✅）。
- 数据流只有两态：`LocalOnly` 和 `CloudAuthorized`，未知一律按 `LocalOnly`。`has_remote()` 过渡门在 K1-6a 和 K1-6b 都完成并验收后撤除（ADR-K1-02 ✅）。分类、处理模式、授权三维独立（§2.1.1），这一节只在 ab/kernel 上，jason 2026-10-10 裁定。

### 3.7 扩展、通信与桌面
- 附着模块 AgentEar：自启、TCC 归 AgentEar、`local_only` 请求不得改发远端（ADR-032、A3 ✅）。
- Open Design：Agent24 是唯一 AI 控制面。现行路径是 CreativeServeWeb + `agent24 acp`（legacy token，全局持久分区）；目标形态是 SidecarManager + capability + workspace 绑定，这条线暂停（ADR-001..006）。
- Documenting：第一方、进程外，按 `document_id+revision` 寻址；引擎用 SuperDoc headless 2.18.0，不嵌入编辑画布（ADR-DOC-01..03 ✅）。
- 知识（WeKnora）进入唯一主对话（ADR-KB-01，草案，只在 ab/weknora 上）。
- Hyphae 通信在 agent24d 内（`/api/v1/comm/*`），以子进程监管、`--notify=false`、使用专用 HOME，口令存钥匙串，靠 lock + sha256 锁定版本（当前 v0.26.3）（COMM-HYPHAE D1–D8 ✅）。

### 3.8 安全边界（威胁模型）
- 进程外模块与 daemon 同 UID，模型是「合作但可能有 bug」，不防有敌意的本地二进制；`shell_exec` 只做 fchdir，不是 OS 沙箱；capability 是应用层最小权限，也不是沙箱（LAYERS §4、ADR-031）。ADR-037 写明的信任边界是「同一 OS 用户的本机进程」。
- 整个工作区只有两处窄 `unsafe`，lint 设为 `unsafe_code = "forbid"`。
- 隔离主张不得超过实际机制（L-MEM-7）。改法律必须单独开 PR（laws/README）。

---

## 4. 互相冲突的决定：裁决表

完整 24 对冲突见 [SOURCE-C1 §4](tracking/2026-10-26/SOURCE-C1-architecture-docs.md)。下表只列影响现行设计的几项。

| 冲突 | 现行方 | 处置 |
|---|---|---|
| ADR-020（Node，端口 8765）vs ADR-026 | ADR-026 | ADR-020 补标「已取代」（ADR-037 修订段） |
| ADR-022 默认 `mlx-lm` vs 代码 oMLX HTTP | 代码 | ADR-022 收窄（ADR-037） |
| ADR-024 asyncio 工作流 vs 代码（只有 scheduler） | 代码 | 补标「已取代」 |
| ADR-017 加密 SQLite vs 代码明文 | 代码 | 改标「未实现」，隐私默认方向保留 |
| ADR-016（decision.md）worker_thread 沙箱 vs 进程外模块 | ADR-029/031 | 补标「部分取代」；签名增强转为 ME-6 上游需求 |
| ADR-027 移动端直连 HTTP vs 移动端遥控经 Hyphae | ADR-037 🟡 | ADR-037 批准后补标 |
| OD ADR-001/005「桌面拥有 capability daemon」、#1184 vs 常驻 daemon | ADR-037 🟡 | ADR-037 批准后，KB-HOST-01 与 OD-M11 F11.1 按「一方本机凭据」改 |
| SPEC-001 §2/§3（`feat/*`、Copilot 评审）vs AGENTS.md | AGENTS.md | SPEC-001 补标局部过期 |
| ADR-K1-02：main 版 vs ab/kernel 版（§2.1.1） | ab/kernel 版 | 随 K1 第二批 release 合入 main |
| ADR-DOC-02：main 版 vs ab/documenting 版 | ab/documenting 版 | 随 Documenting r6 合入 |
| 全局评审门：AGENTS.md（分级）vs PLAN-MEMORY-P1（所有 PR 都要 APPROVE，2026-10-10） | **待 jason 裁决** | 见里程碑文档 §6 |

---

## 5. 需要修正的文档与代码不符（交给文档治理任务 DOCS-2）

1. `ARCHITECTURE-LAYERS.md`：记忆、M1 的说法，以及漏写的组件（见 §2）。
2. `laws/APPROVAL.md` L-APPR-5/8 的「今天靠什么保证」：要单独开一个法律 PR。
3. `decision.md` 中已被取代、却仍标「✅ 采纳」的 ADR-014/015/016/017/020/022/024/025：补标状态。
4. `README.md` 架构部分：`KERNEL_GRANTS` 是三项；当前版本是 0.5.1；ADR 已到 036/037；核心组件表要补 comm、hw、decide、documents、sidecar-host。
5. `adr/ADR-K1-03` 在 main 上有 13 个失效相对链接（ab/kernel、ab/memory-p1 上已修，随 release 带入）。

---

## 6. 文档索引（每份旧文档在新体系里的位置）

逐份状态见 [SOURCE-C1 §1](tracking/2026-10-26/SOURCE-C1-architecture-docs.md)（114 份：现行 71、历史 35、被取代 5、与代码不符 3）。读者按下表找：

| 想了解 | 现行权威 | 历史 / 被取代（只作出处） |
|---|---|---|
| 产品形态、进程模型 | 本文 §1；`decision.md` ADR-037（🟡） | ADR-018、ADR-020、ADR-027（移动端部分） |
| 分层与依赖 | 本文 §2；`ARCHITECTURE-LAYERS.md`（按 §2 的修正读） | `architecture/kernel-boundary.html`（08-22 快照）、`specs/SPEC-001` 布局部分 |
| 架构决策日志 | `decision.md`、`ADR-026-rust-core-polyglot.md`、`adr/ADR-K1-01..04`、`documenting/adr/ADR-DOC-01..03`、`open-design-workspace/design/ADR-001..006` | `ADR-016-consume-auraai-packages.md`（被 ADR-026 §6 取代） |
| 法律（不变量） | `laws/README`、`APPROVAL`（L-APPR-5/8 待修）、`CONTEXT`、`MEMORY` | — |
| 协议 | `protocol/openapi.yaml`、`specs/SPEC-002`、`specs/SPEC-ME3-OUT-OF-PROCESS`、`specs/WIRE-OOP-MODULE` | `specs/F4-nostr-channel`（被 COMM-HYPHAE 取代） |
| 记忆 | ADR-028/030、`laws/MEMORY`、`specs/SPEC-ORG-SPACE`；ab/memory-p1 上的 `ADR-MEMORY-*`、`R05-*` | `specs/SPEC-MEMORY`、`specs/SPEC-MD-ME`（部分） |
| 模块与 SDK | ADR-029/031、`design/ME4-S1..S3`、`design/A3-ATTACHED-MODULE` | FU-57/60/61/64、T7/T8/T8.5 系列（已落地设计稿，保留不变量出处） |
| 硬件与模型 | ADR-036、`design/HW-TIERS-*`、ADR-032、`design/INTEGRATION-AGENTEAR-IDORIS` | ADR-021/022/025、`WORKSTATION_PLAN.md`（保留 oMLX API 速查价值） |
| 通信 | `design/COMM-HYPHAE`、`comm/LOCK-UPGRADE-20261024`、`Deployment/HYPHAE-*` | `design/HYPHAE-CLI-INTEGRATION`、`comm/JOINT-ROUND*` |
| Open Design | OD ADR-001..006、G1/G2/G8 设计（文首状态已过期，内容有效） | `design/OPEN-DESIGN-*` 三份、OD 冻结包 |
| 跨平台 | `design/WINDOWS-PORT.md`（v2）、ADR-037 §5 | — |
| Sin90 / Pet0 | `SIN90-PET0-INTEGRATION.md` v0.3 | `specs/SIN90-domain.md` |
| iDoris | iDoris 仓库《iDoris-Agent24-边界与接口规范》1.2；ab/idoris 上的 `docs/iDoris-consumer.md` | `iDoris-integration-and-entry-router.md` |

---

## 7. 必须保留的历史要点（只写在旧文档里，但仍然约束今天的设计）

全文 20 条见 [SOURCE-C1 §6](tracking/2026-10-26/SOURCE-C1-architecture-docs.md)。评审时最常引用的几条：

1. ADR-026 §6.5 的 11 条设计硬约束：
   - 取消一等公民；
   - 审批走请求-响应，流式只做单向通知；
   - fail-closed；
   - `available_decisions` 由数据驱动；
   - 审批与沙箱正交；
   - Guardian 只接 L1 小模型；
   - 工具注册用 registry trait；
   - cron 先 pre-advance，再加连续失败自动禁用；
   - WS 用强类型枚举；
   - 反向注入；
   - 遥测脱敏。
2. ADR-026 的显式否决：不做全量重写，不先写 TS 内核，不 fork Goose/Codex，GPL 代码只读思路不复制。
3. ADR-028 Codex 收口：
   - 真双时态需要两个时间轴；
   - 自动写入走 candidate → validate → approve → commit；
   - Condenser 绝不删除原始事件。
4. ADR-030 的八条不变量，以及「不做」清单（不是沙箱、不做加密隔离、没有行级权限）。
5. ADR-031 选型的理由：ACP v2 删除了 fs/terminal；MCP 不作为模块协议。
6. ADR-035 的失陷情形与保证边界全表。
7. M10 教训：「组件落地」「产品路径接线」「验收」「发行」四种状态分别记录，不得互相代替；集成分支必须每周吸收 main。
8. SPEC-001 中仍有效的工程铁律：依赖方向、core 零框架依赖、渲染进程不直接发 HTTP、生成物不手改。
9. 每个已冻结设计稿（FU、T7、T8、T8.5 系列）文末的 Codex 评审结论，是对应不变量的出处。维护「不变量 → 设计稿 → 测试」索引，见 DOCS-3。

---

## 8. 外部接口与跨仓约定

| 外部 | 约定 / 期望 | 现状与待办 |
|---|---|---|
| iDoris | 跨仓唯一权威是 iDoris 仓库的边界规范 1.2；Agent24 只保留一个消费方指针 | 指针 `docs/iDoris-consumer.md` 只在 ab/idoris 上；随 iDoris release 进 main |
| Hyphae（原 agent-speaker，本地 `auraai/agent-speaker`） | 以进程外 sidecar 方式接入；Hyphae 提供 `groupchat --json` 契约，期望 Agent24 做群聊界面 | 锁定 v0.26.3 ✅；群聊界面未做（COMM 线） |
| AgentEar | A3 附着协议 | 已于 v0.4.0 合入 ✅ |
| Sin90 / Pet0 | 以 Agent24 的 `SIN90-PET0-INTEGRATION.md` v0.3 为权威 | Pet0 的镜像停在 v0.1；Sin90 README 仍写「没有 Rust SDK」（需外仓更新） |
| WeKnora fork | Agent24 是唯一对话入口 | ADR-KB-01 只在分支上；fork 侧文档仍写「暂停」 |
| Brood | `orgs/auraai/INTERFACES.md`、`PROFILE.md` 被本仓 CLAUDE.md 导入 | 内容严重过期（仍写 Claude Code 框架、MemPalace、agent-speaker v0.25.0）；需要外仓更新 |
| iDoris-Components | `docs/components/agent24/*` 是同步副本 | 落后 main 704 个提交，需要重新同步 |

外部需要更新的完整清单见 [SOURCE-C3 §四](tracking/2026-10-26/SOURCE-C3-progress-links.md)，执行项在里程碑文档的 DOCS-4。
