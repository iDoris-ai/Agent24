# Agent24 结构组件路线图（Component × Milestone）

> 立于 2026-10-03。本文回答：**每个核心结构组件现在到哪了、下一步做什么、在哪个里程碑完成、彼此怎么依赖。**
> - 架构现状（各层是什么、状态标记 ✅ / 🟡 / 📐 的含义）见 [`../ARCHITECTURE-LAYERS.md`](../ARCHITECTURE-LAYERS.md)。
> - 执行状态以 [`tasks.md`](tasks.md) 为准；各里程碑的详细定义在各自的计划文档（文中逐条链接）。
> - **编号原则**：里程碑的**权威编号**沿用各计划文档（记忆的 P0–P5、Open Design 的 OD-M11 / F11.*、ADR-032 的 P3–P5、iDoris 的 ID-*、通信的 COMM-*、部署的 DEP-*、SPEC-002/B4 等）。本文另有的 K-* / W-* / A-* / O-* / W3-0 只是**组件视图别名**（便于按组件阅读），每个别名都写明它对应的源编号；**依赖关系一律以源编号表达**，别名不单独作为排序节点。跨组件的排期用 §3 的「波次」表达，**波次是建议，待 jason 拍板**。
> - 与 iDoris-Components（internal-AI）仓库的规划对齐情况见 §4。

---

## 1. 组件一览

| # | 结构组件 | 主要 crate / 位置 | 现状一句话 |
|---|---|---|---|
| C1 | 内核与运行时 | `agent24-core` / `store` / `agent` / `protocol` / `agent24d` | ✅ 稳定；run、审批、审计、调度、领域 OS 挂载都在默认路径上 |
| C2 | 模型网关与路由 | `agent24-models`（`ModelRouter`） | 🟡 路由机制已实现，但主对话路径传默认 `TaskProfile`；只接了 oMLX / Ollama；iDoris 未接 |
| C3 | 记忆 | `agent24-memory`、`ab/m1-memory` 分支 | 🟡 main 上 agent loop 只用 D1（KV + CanonicalSession）；M1 在分支上已完成大半 |
| C4 | Workspace（文件系统工作区） | `agent24-workspace` / `store` / `os-cwd` | 🟡 仅 Unix、无对外路由、Open Design 未绑定 |
| C5 | 权限、能力与法律 | `agent24-policy`、`agent24d/capabilities`、`docs/laws/` | 审批 ✅、Guardian ✅（可选）；capability 🟡；法律三部 |
| C6 | 领域 OS 生态 | `os-proto` / `os-fd` / `os-packages` / `os-sdk`；Sin90、Cos72 外仓 | ✅ ME-3 / ME-4 已交付（v0.5.0）；进程内模块缝 🟡 无实例 |
| C7 | 第三方工具集成 | Desktop `CreativeServeWeb`、`agent24 acp`、`agent24-sidecar-host`、`agent24-mcp` | Open Design 现行路径 ✅；目标形态 🟡/📐；MCP ✅ |
| C8 | 通信（Hyphae） | `agent24-comm` | ✅ COMM-1…4b 已交付；UI 与联调未做 |
| C9 | 语音（AgentEar） | 附着模块（A3） | ✅ P0–P2（非流式单轮） |
| C10 | 桌面、CLI 与发布 | `apps/desktop`、`agent24-cli`、`docs/Deployment/` | ✅ v0.5.1：CLI macOS + Linux，Desktop 仅 Linux；macOS 签名包被 Apple 账号阻塞 |
| C11 | 组织化 | ADR-030、SPEC-ORG-SPACE | F8 ✅；F9–F11 等第二个真实用户 |
| C12 | Web3 身份与结算（AAStar） | — | 📐 Agent24 代码中**尚无接线**，只在生态规划中 |

---

## 2. 逐组件路线

每个组件：**现状 → 里程碑（按顺序）→ 依赖**。「📐 → X」表示该能力今天只有设计，计划在里程碑 X 完成。

### C1 内核与运行时

- **现状**：run 生命周期、取消、fail-closed 审批、哈希链审计、调度、领域 OS 挂载 ✅。OpenAPI 仍为手写权威（events schema 已由 Rust 导出）。
- **里程碑**
  1. **K-1 记忆接线**：agent loop 改用 `SessionLog`，并在 run 前召回注入。由 C3 的 M1/P0 交付（已在 `ab/m1-memory` 完成 T03–T07），合回 main 后 ✅。
  2. **K-2 审计字段约束**：`append_audit` 目前接受任意 JSON，需要加字段白名单 / 黑名单，防止 prompt 等敏感内容被永久写进哈希链（来源：internal-AI 代码审计 R13）。📐 → 波次 A。
  3. **K-3 = SPEC-002/B4**：OpenAPI 改为由 Rust 类型生成，CI 零漂移（[SPEC-002](../specs/SPEC-002-protocol.md)）。📐 → 波次 B，无依赖，可随时插入。
  4. **K-4 ML Worker（⊂ C3 P2）**：Python worker 服务端（[docs/specs/TASKS.md](../specs/TASKS.md) D4b，等真实消费者）作为 P2 的一部分交付：K-4a worker 服务端 → P2 向量召回接线 → K-4b 首个消费者验收。📐 → 随 C3 P2。
- **依赖**：K-1 ← C3 P0；K-4 不单独排序，属于 C3 P2。

### C2 模型网关与路由

- **现状**：`ModelRouter` 按 `TaskProfile`（`Privacy × Complexity` → `Tier`）选 provider，`LocalOnly` 对远端 fail-closed ✅。但主对话、子 agent、REST 聊天都传 `TaskProfile::default()`，只有 Guardian 与模块推理回调会设置 `LocalOnly` 🟡。`from_env` 只构造 oMLX / Ollama；Agent24 内**没有出境脱敏代码**（已定由 iDoris 出口网关负责）。
- **里程碑**（ID-* 见 [iDoris-integration-and-entry-router.md](../iDoris-integration-and-entry-router.md)，其建议顺序为 ID-1 → ID-2；P3–P5 见 [decision.md ADR-032](../decision.md)，冻结顺序为 P3 → P4 → P5）
  1. **ID-1 任务画像生成**（无硬前置）：由入口路由（先规则版，再 Semantic Router 级小模型）为每个 run 生成 `TaskProfile`，替换主路径上的 `TaskProfile::default()`。📐 → 波次 A。需拍板：画像在 Agent24 侧生成，还是交给 iDoris 网关兜底（internal-AI docs/11 里程碑 1.2 的同一决断点）。
  2. **ADR-032 P3 — 推理回调流式**（`_a24/model/stream`，目标首字 ≤ 3s）。📐 → 波次 A / B。
  3. **ID-2 = ADR-032 P4 — iDoris provider 接线**：`IDORIS_URL`，拆成 `idoris-local`（强制 `local_only`）与 `idoris-any`；iDoris 回报实际落点。按 ADR-032 顺序排在 P3 之后；若要先做 P4，需在 ADR-032 记录「执行顺序重排、不改编号」。**对接前先核对跨仓字段**（该提案已知与 iDoris 现状有出入）。📐 → 波次 B。
  4. **出境脱敏**：在 iDoris 出口网关实现（不在 Agent24 内实现）。顺序：**先冻结隐私法律 L-PRV（C5 A-3，波次 A）→ ID-2 / 脱敏实现 → 变异验收通过 → 才在默认路径启用**。📐 → 波次 B。
  5. **ID-3**（`X-iDoris-*` 头与 `LocalOnly` 贯穿主对话路径）、**ID-5**（经 iDoris 的危险动作复用 fail-closed 审批门）：依赖 ID-2。📐 → 波次 B。
  6. **ID-4 跨平台推理后端**（Win / Linux 用 vLLM / llama.cpp / Ollama，依赖 ID-2），是 C10 Windows 发布的前置之一。📐 → 波次 B / C。**ID-6** 动态硬件推荐（依赖 ID-4）：低优先级、未排期。
- **依赖**：ID-3 / ID-4 / ID-5 ← ID-2；ID-6 ← ID-4；ID-2 排在 ADR-032 P3 之后；默认路径启用脱敏 ← C5 A-3；C9 的流式体验 ← ADR-032 P3；C3 的 P2（本地向量 / 抽取）← 本地模型可用；C10 Windows ← ID-4。

### C3 记忆

- **现状**：main 上 agent loop 只用 D1 的 `KvStore` + `CanonicalSession`（有损摘要折叠）；M-D 高层库（EventLog、断言账本、检索、巩固等）已实现并有测试，但 main 上未接入。**M1 在 `ab/m1-memory` 分支已合入 T01–T07、T07a、T09**（PR #636–#652）；分支台账未同步，以提交历史为准。
- **里程碑**（定义见 `ab/m1-memory` 分支的 `docs/agent/M1-PLAN-v2.md` 与 `docs/research/MEMORY-STRATEGY.md`；拍板见 [`roadmap.md`](roadmap.md) 2026-10-02 记录）
  1. **P0 = M1 收尾**：T07.1（召回注入改为带标注的数据块）、T08（召回评测基线）、T10（记忆 REST + 总开关 + 来源展示）、T11（桌面「记忆」页）。然后**先把 main（含 M10 的 633 个提交）合进 `ab/m1-memory` 并重跑全部 gate，再以 merge 方式落 main**，吸取 M10「分支漂太久」的教训。📐 → 波次 A。
  2. **P1 个人记忆可控（M1.5）**：先出 ADR「原文生命周期内不丢 vs 用户清除权」；回执事件、双时态与用户 supersede、程序性偏好、导出、purge、无痕会话。📐 → 波次 B。
  3. **P2 本地智能**：向量召回 + RRF、本地 LLM 候选抽取（进待确认区）、中文评测集 ≥100 例；**包含 K-4（ML Worker 服务端与首个消费者）**。📐 → 波次 B / C（依赖 C2 本地模型可用）。
  4. **P3 共享空间**：先做社区（Cos72），`team / prj / com` SpaceId、按操作判定、`policy_epoch`、显式 publish / grant。📐 → 波次 C。与 C6 O-3（Cos72 M4）**协同设计**。授权模型归 C11：**P3 触发并交付 F9 的社区子集**（spaces + grants + 交集判定 + 审计事件，契约由 C11 定义）；F9 的其余部分（groups、企业场景）以及 F10 / F11 仍等第二个真实用户。
  5. **P4 企业治理** / **P5 高级记忆**（时态图、巩固 / 反思、**AgentEar 语音摄入**）。📐 → 远期。
- **依赖**：C1 K-1 ← P0；C6 O-3（Cos72 M4）← P0（roadmap 明写「依赖 M1 全部完成」）；P1 ← P0；P2 ← P1（K-4 ⊂ P2）；P3 ← P1，并带出 C11 F9 社区子集；C9 语音摄入 ← P1 的事件 schema。

### C4 Workspace（文件系统工作区）

> 本节的 workspace 指 L2 的**文件系统工作区**。仓库里另有两个同名概念：Cos72 的「社区 Workspace 底座」（C6，roadmap M4）和 SPEC-ORG-SPACE 的「作用域 Workspace」（C11，F10）。三者不是一回事，见 W-N 的命名任务。

- **现状**：受信构造只有 `orchestrator_scratch`（写回 external、串行、TTL 24h / 最长 7 天）；描述符钉住根、`fs_*` 路径约束、`shell_exec` 只钉 cwd（不是 OS 沙箱）🟡。仅 Unix；对外没有任何 workspace 路由；Open Design 的 run 不绑定 workspace。
- **里程碑**
  1. **W-1 = OD-M11 F11.0a / F11.3（产品面 + 绑定）**：scratch 的创建 / 恢复、host lease、续期 / 释放、cwd resolve 路由；Open Design 的 run 改为 workspace-bound。📐 → 波次 A（[PLAN-OD-NEXT](PLAN-OD-NEXT.md)）。
  2. **W-2 = OD-M12 F12.2 / F12.4**：workspace 管理界面与 CLI；Windows 上的 workspace 权限实现（当前 `UnsupportedPlatform`）。📐 → 波次 B；Windows 部分是 C10 Windows 发布的前置。
  3. **W-3 第三方嵌入通用化（= TPI-W 中与 workspace 相关的部分）**：把「一个工具项目 ↔ 一个 workspace」的附着模型从 Open Design 专用（`creative_attachment_id`）泛化为 `tool_attachment`，并写进接入 Playbook：任何第三方工具都通过「一个 workspace + 一个限定该 workspace 的令牌」接入。📐 → 波次 B。
  4. **W-4 OpenCreator 适配**（= OC-1 的设计内容、OC-2 的实现内容，不是 OC 的外部前置）：大媒体文件（视频、音频）的存放、配额与 TTL；外部服务的出境策略；可能需要新的 workspace 种类。📐 → 波次 C。
  5. **W-5 写回与新种类**：`writeback_policy` 除 `external` 外的取值、写回审批、源码 checkout 类 workspace、并发策略（先 ADR = OD-M12 F12.3，再实现）。📐 → 波次 C。
  6. **W-N 命名收敛**：给三个「Workspace」定不同的名字（例如 文件系统工作区 / 社区工作台 / 记忆作用域），在 ADR 与代码注释中统一。📐 → 随 W-3 一起做。
- **依赖**（以源编号表达）：OD-M11 内部为 **F11.0a + F11.0 → F11.1 / F11.2 / F11.3**——W-1（F11.0a、F11.3）与 C5 A-1（F11.0、F11.1、F11.2）只是同一里程碑按组件的归属标签，互相交织，不能整体排先后；TPI-W ← F11.0a–F11.3；W-3 ⊂ TPI-W；W-4 由 OC-1 设计、OC-2 实现；C10 Windows ← W-2（F12.4）。

### C5 权限、能力与法律

- **现状**：fail-closed 审批 ✅；Guardian ✅（默认关闭）；模块侧能力句柄 ✅；外部客户端 capability：令牌基础设施 🟡，路由级资源授权 📐，产品路径仍是 legacy 全权 bearer；Desktop 通用 `backendProxy` 未做高权限接口隔离。法律：上下文、记忆、审批三部。
- **里程碑**
  1. **A-1 = OD-M11 F11.0 / F11.1 / F11.2**：路由级资源授权与高权限接口隔离、Desktop 自有 capability daemon（无孤儿）、broker / handoff / 吊销。📐 → 波次 A。
  2. **A-2 = TPI-W W.2**：`CreativeRuntime` 等 Creative 专名泛化为按工具区分的 audience（如 `ToolRuntime{tool}`）。📐 → 波次 B。
  3. **A-3 隐私法律 L-PRV**：把「出境必经 iDoris 网关脱敏」「`LocalOnly` 贯穿主路径」等写成法律并配变异测试（来源：internal-AI docs/06 建议）。**法律先于实现冻结**，是 C2 脱敏在默认路径启用的上线门；同时补 `docs/laws/EDITIONS.md`（SPEC-EDITIONS 的下一步）。📐 → 波次 A（与 ID-2 同批）。
  4. **A-4 记忆授权演进**：`Authorizer` 在 P1 细化操作类型，P3 加 module principal / delegation / `policy_epoch`（随 C3）。
  5. **A-5 ADR-032 P5**：gate 闭集执行动作扩展（排除 `builtin`）、proposal → 宿主确认 UI → 回执留档（随 C9）。
  6. **A-6 ME-6 模块签名**（sigstore keyless + 信任策略；签名只回答「谁写的」，不提供隔离）。📐 → 远期，需拍板。
- **依赖**：A-1 / W-1 的内部顺序见 C4；C7 OC-1 ← A-2（TPI-W W.2）；C2 默认路径脱敏 ← A-3；A-4 随 C3 P1 / P3；A-5 与 C9 P3 同步。

### C6 领域 OS 生态

- **现状**：进程外模块协议、包管理、监管、SDK 原型、调度 / 推理 / 记忆 / 审批回调 ✅（ME-3、ME-4，v0.5.0）；Sin90、Cos72（mytask 最小样例）已在独立仓库发布。进程内模块缝 🟡（无生产实例）。
- **里程碑**（[PLAN-ME4](PLAN-ME4-OS-CAPABILITIES.md)「不在本轮」、[roadmap.md](roadmap.md) M4 / M5）
  1. **O-1 ME-4 收尾债**：Codex 补审债（ME4-CODEX-DEBT）、FU-104（Node 参考模块补 scheduler / model / approval 演示）。📐 → 波次 A。
  2. **O-2a 模块受众与挂载策略**：manifest 加 `audience` 字段，解决「哪些模块互为替代」今天说不出来的问题（PLAN-OOP §七 的角色场景 OS 决策）。📐 → 波次 B。
  3. **O-2b `_a24/memory/scoped/*`**：依赖 C3 M1 personal-space 收口与 C11 F9（[SPEC-ME3](../specs/SPEC-ME3-OUT-OF-PROCESS.md) 已把 scoped / shared-space 语义推迟到 F8c / F9），随 F9 排期。**`Policy` 回调**：未排期，出现稳定消费者再启动。
  4. **O-3 Cos72 Workspace 底座（roadmap M4）**：`CosEntity` 双向图、统一查询面、事件落底座、`workspace.search/get/link` 工具。依赖 C3 P0。📐 → 波次 C。internal-AI 规划把 4seas 定位为 Cos72 的一个实例，O-3 / O-4 是它的前置。
  5. **O-4 Cos72 三件套（roadmap M5）**：myshop、myvote、渠道接入（复用微信 / Nostr），含 Skill 分发 SD-1…4。📐 → 波次 C 之后。
  6. **O-5 签名** = C5 A-6。
- **依赖**：O-3 ← C3 P0（与 C3 P3 协同设计）；O-4 ← O-3、C8 COMM-7（渠道）；O-2b ← C11 F9。

### C7 第三方工具集成

- **现状**：Open Design 现行路径 ✅（`CreativeServeWeb` + `agent24 acp`，legacy token，无 workspace 绑定）；目标形态组件 🟡；MCP ✅。M10 已完成（[落地指南](../design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md)保留为历史审计记录）。
- **里程碑**（定义见 [PLAN-OD-NEXT](PLAN-OD-NEXT.md)）
  1. **OD-M11 产品路径生效** → 波次 A。
  2. **TPI-W 第三方工具接入 Workflow**（Playbook、去 Creative 专名、harness 模板、评估清单）→ 波次 B；**OD-M12 收尾**并行。
  3. **OC OpenCreator**：OC-0 调研（可提前，只做调研不写代码）→ OC-1 边界 ADR（先裁决：OpenCreator 自带 Codex 执行引擎，与「Agent24 是唯一 AI 控制面」重叠）→ OC-2 接入 → OC-3 E2E 落地。→ 波次 B / C。
  4. **其它候选**：MediaBot 等外部工具接入时同样走 TPI-W；凡是出网生成（如 Open Design 默认云端生图）都要经出口策略。
- **依赖**：OD-M11 内部 F11.0a + F11.0 → F11.1–F11.3；TPI-W ← F11.0a–F11.3；OC-0 无前置（纯调研可提前）；OC-1 ← TPI-W（含 W.2 / W-3），并在 OC-1 中产出 W-4 设计；OC-2 实现 W-4；OC-3 ← OC-2。

### C8 通信（Hyphae）

- **现状**：`agent24-comm` 的身份、联系人、relay、导入、daemon 监管、send / history / outbox、状态探针 ✅（COMM-1a…4b）。
- **里程碑**（[COMM-HYPHAE](../design/COMM-HYPHAE.md) §8）
  1. **COMM-5b** 结构约束测试（依赖 allowlist 等）→ 波次 A。
  2. **COMM-6** UI：身份 / 联系人 / relay / daemon 状态 / 导入向导 → 波次 A。
  3. **COMM-7** UI：收件历史 / outbox / 重试 + 双仓联调 → 波次 A / B。
  4. **入站授权收口（T01-E）**：入站执行统一走高层授权路径（与 C5 对齐），是四仓联调验收的前置。→ 波次 B。
  5. 升级 `hyphae.lock.json` 到含 daemon 锁的版本；发布包加入 `hyphae`（与 C10 配合）。
- **依赖**：T01-E ← C5；O-4 渠道接入 ← COMM-7。

### C9 语音（AgentEar）

- **现状**：A3 附着模块 P0–P2 ✅（非流式、单轮）。
- **里程碑**（[ADR-032](../decision.md)、[A3 设计 §12](../design/A3-ATTACHED-MODULE.md)）
  1. **P3 交互闭环**：proposal → 宿主确认 UI → gate 可执行集合 → 回执留档；回复文本展示。→ 波次 B（与 C5 A-5 同步）。
  2. **流式**：依赖 C2 的 ADR-032 P3（`_a24/model/stream`）。→ 波次 B。
  3. **多轮**：授予 `_a24/memory/private/*` 或宿主会话回调（依赖 C3 P1）。→ 波次 C。
  4. **语音摄入记忆** = C3 P5。
- **依赖**：C2 P3、C3 P1、C5 A-5。

### C10 桌面、CLI 与发布

- **现状**：v0.5.1 ✅：CLI 覆盖 macOS + Linux；Desktop 只发布 Linux，macOS dmg 等 DEP-B（v0.5.2）。DEP-A 全部完成。
- **里程碑**（`docs/Deployment/`）
  1. **DEP-B v0.5.2 签名**（macOS dmg、CLI / 模块签名公证）：**BLOCKED**（Apple 账号申请中）。
  2. **DEP-C Windows（v0.6）**：签名方式拍板、移植实现、发布。依赖 C4 W-2（Windows workspace）与 sidecar-host 的 ProcessKit 复用。→ 波次 B / C。
  3. **DEP-C0 自动更新**；**DEP-C4 / C5 / C6** 远程访问与移动端（MVP = PWA 遥控）。→ 波次 C。
  4. Desktop 本身的结构改造（自有 capability daemon、托管器）由 C7 OD-M11 承担。
- **依赖**：Windows ← C4 W-2（F12.4）、C2 ID-4、sidecar-host 的 ProcessKit 复用。

### C11 组织化

- **现状**：ADR-030 F8（所有权 = (组织, 空间)）✅。
- **里程碑**：F9（grants + groups + 交集判定 + 审计事件）→ F10（持久化作用域 Workspace）→ F11（`asserted_by` + 冲突断言）。**F9 的社区子集由 C3 P3 触发（波次 C）**；其余部分等第二个真实用户，不排期（[roadmap.md](roadmap.md) M3）。
- **不能等的前置**（[SPEC-ORG-SPACE](../specs/SPEC-ORG-SPACE.md) §9）：① 共享空间的不可变 ID（随 C3 P3 定）；② 组织 / 空间是否跨库或跨地区（随 C3 P4 定）；③ agent loop 自身记忆归属迁到 personal space（= C3 P0 的 M1-T02 / T05，已在 `ab/m1-memory` 完成，合回 main 后生效）。

### C12 Web3 身份与结算（AAStar）

- **现状**：📐 Agent24 代码中**没有任何 AirAccount / SuperPaymaster 接线**；生态文档（Brood `INTERFACES.md`）列为「规划中、可选」。
- **里程碑**：**W3-0 接口 ADR**：明确 agent 身份（AirAccount）与 gasless 执行（SuperPaymaster）在 Agent24 中的落点（很可能是一个领域 OS 或能力），以及与 C5 授权、C11 组织身份的关系。📐 → 未排期，待拍板。在此之前，对外文档不应把 Web3 写成已上线能力。

---

## 3. 跨组件波次（建议，待 jason 拍板）

| 波次 | 主要内容 | 并行性说明 |
|---|---|---|
| **A（近期）** | C3 P0（M1 收尾并合回 main）· C7 OD-M11（含 C4 W-1、C5 A-1）· C8 COMM-5b / 6（COMM-7 可延至 B）· C5 A-3 隐私法律冻结 · C2 ID-1 画像（规则版）+ ADR-032 P3 流式（可延至 B）· C1 K-2 审计约束 · C6 O-1 收尾债 · C10 DEP-B（等账号） | M1 走 B 机 / Codex 线，OD-M11 走本机，互不阻塞（[PLAN-OD-NEXT §5](PLAN-OD-NEXT.md)） |
| **B（中期）** | C7 TPI-W + OD-M12（含 C4 W-2 / W-3、C5 A-2）· C7 OC-0（可提前）/ OC-1 · C3 P1 · C2 ID-2 iDoris provider + 出境脱敏、ID-3、ID-5、ID-4（B / C）· C9 P3 · C6 O-2a · C1 K-3（SPEC-002/B4）· C10 Windows（v0.6，可能延至 C） | TPI-W 只依赖 OD-M11 的 F11.0a–F11.3 |
| **C（远期）** | C7 OC-2 / OC-3（含 C4 W-4）· C3 P2 / P3 + C6 Cos72 M4 / M5（4seas 实例）· C4 W-5 写回 · C9 多轮 · C10 自动更新 / 远程 / 移动 · C5 A-6 签名 | Cos72 M4 依赖 C3 P0，并与 P3 同步设计 |
| 未排期 | C11 F9–F11（等第二个用户）及依赖它的 C6 O-2b · Policy 回调 · C12 W3-0 · C3 P4 / P5 · C2 ID-6 | |

**关键依赖链**（一眼看清）

```
OD-M11:  F11.0a + F11.0 ──▶ F11.1 / F11.2 / F11.3 ──▶ TPI-W（含 W-3、W.2）──▶ OC-1（设计 W-4）──▶ OC-2（实现 W-4）──▶ OC-3
                                                      └─ OD-M12 与 TPI-W 并行；OC-0 调研可提前
记忆:    C3 P0（M1 合回 main）──▶ C1 K-1 ──▶ C3 P1 ──▶ C3 P2（含 K-4）/ P3（带出 C11 F9 社区子集）
         C3 P0 ──▶ C6 O-3（Cos72 M4）──▶ O-4（M5，4seas 实例）      （P3 与 O-3 协同设计）
模型:    C2 ID-1 画像（无前置）；ADR-032 P3 ──▶ P4 = ID-2 ──▶ ID-3 / ID-4 / ID-5；ID-4 ──▶ ID-6
隐私:    C5 A-3 法律冻结 ──▶ ID-2 / 出境脱敏（iDoris）──▶ 变异验收 ──▶ 默认路径启用
Windows: C2 ID-2 ──▶ ID-4 ┐
         C4 W-2（F12.4）──┼──▶ C10 Windows v0.6
         ProcessKit 复用 ─┘
```

---

## 4. 与 iDoris-Components（internal-AI）规划的对齐

| internal-AI 的规划 / 说法 | 对应本文 | 对齐结论 |
|---|---|---|
| 4seas = Cos72 社区 OS 的一个实例，走 `agent24-os-proto` 契约（docs/06） | C6 O-3 / O-4 | 一致；O-3 依赖 C3 P0 |
| C3 统一模型调度终态归 iDoris，16GB 节点过渡期由 Agent24 `ModelRouter` 承担（docs/03、docs/10） | C2 ID-2 / ID-1 | 一致；画像在哪一侧生成待拍板 |
| 出境脱敏执行者归 iDoris 出口网关（2026-09-07 拍板） | C2 第 4 项、C5 A-3 | 一致；Agent24 侧只负责 `LocalOnly` 贯穿与法律 |
| 新增隐私法律 L-PRV-1/2（docs/06） | C5 A-3 | 采纳，排波次 A（先于脱敏实现冻结） |
| 记忆是 Agent24 的模块，避免与 MemPalace 重复建设（docs/05、BR-19） | C3 | 一致：记忆归 Agent24 C3 |
| 旧组件主文档里的「Guardian 安全门禁」「三级路由」「21.5 万行」 | C2 / C5、§1 | 已在 internal-AI 主文档按代码改写：路由机制在、画像未生成；Guardian 可选。规模口径：2026-10-03 main 的 Rust 源码约 18.7 万行（不含独立测试文件约 16.4 万行）；internal-AI 引用的 64,885 行是 M10 落地前的旧统计 |
| README 把 Web3（AAStar）写成已上线支柱 | C12 | **不一致**：Agent24 无接线，应改为「规划中」 |
| AgentEar 声称已集成 TTS | C9 | 需在 AgentEar 仓库核实；Agent24 侧只依赖 `speak` 反向命令 |

---

## 5. 维护规则

- 某个里程碑完成时：同时更新本文对应组件的「现状」、[ARCHITECTURE-LAYERS](../ARCHITECTURE-LAYERS.md) 的状态标记、[tasks.md](tasks.md) 台账，以及 internal-AI 的同步副本。
- 新增结构组件或里程碑时：先在本文登记（沿用该组件已有的编号），再写详细计划文档。
