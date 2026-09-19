# Agent24 × Open Design 实施计划（待 review）

> 状态：Draft / 未批准 / 不得开始实现
>
> 日期：2026-09-19
>
> 工作分支：`feat/open-design-workspace`
>
> 基线：Agent24 `04ccd3a0f6271e2a5b54c4668e83cac1636127ae`

## 1. 目标与非目标

### 目标

把 Open Design 的 Creative workflow 作为 Agent24 Office Suite 中的 **Creative Workspace** 接入，同时满足：

1. 尽量完整保留 Open Design 的项目、对话、Studio、Design System、Skills、Preview、Export 和 artifact lifecycle。
2. Agent24 继续作为唯一的 AI control plane，负责模型路由、会话/记忆、权限审批、工具、调度、用量与审计。
3. 最终桌面产品只保留一个 Electron main process；Agent24 是统一 Shell，Open Design 是其中一个工作区。
4. Open Design 源码保持可追踪上游，定制面保持薄且有自动化验证。
5. 所有关键选择保留可替换边界，并提前写明改变路径、成本和预计时间。

### 本轮非目标

- 不把 Open Design 变成 Agent24 的全部 UI。
- 不把 Open Design 源码直接复制进 Agent24 monorepo。
- 不在 MVP 阶段重写 Open Design 的 Studio、preview、export、design-system 或 plugin 内核。
- 不在本计划获批前创建远端 fork、修改产品代码、发布包或部署。
- 不在首版切换到 Tauri。

## 2. 已核实的当前事实

### Agent24

- Electron + React 已是参考桌面 Shell；核心运行时是 Rust `agent24d`。
- 外壳通过 v1 REST + WebSocket 使用 daemon。
- `POST /api/v1/runs` 已提供异步 run，事件包含 `run.started`、`model.delta`、`tool.started`、`tool.completed`、审批和终态。
- CLI 当前只有一次性 `chat`、TUI、MCP 等入口，尚无 ACP 模式。
- `RunCreate` 目前只有 `prompt`、`session_id`、`model_override`、`mode`，**没有 workspace/cwd**。
- 文件读写与 shell 工具目前统一固定在 `~/.agent24/workspace`，尚不具备 Open Design 项目所需的按 run 工作区隔离。
- WebSocket 要求 bearer token，并拒绝带浏览器 Origin 的连接；原生 CLI bridge 可以连接，浏览器不能直接消费该事件流。

### Open Design 上游

- 当前主代码是 Apache-2.0；截至 2026-09-19 最新 release 为 `open-design-v0.22.2`，上游 `main` 仍活跃变化。
- 产品形态是 Next.js/React Web UI + Express daemon + Electron/packaged host，UI 与 daemon 使用 HTTP + SSE。
- runtime adapter 是数据定义，官方对新 runtime 推荐 **ACP over stdio**。
- `orchestrator-scratch` 已定义：外部 orchestrator 准备 disposable workspace；Open Design 可读写并返回结果，但 source authority、writeback、发布和部署仍归 orchestrator。
- Open Design 的价值不只是 React 组件，而是其完整 Creative workflow 和 daemon 侧项目/预览/导出能力。

### 由事实得出的结论

网页版讨论的总体方向成立，但真正的第一个技术前置不是“加一个 runtime def”，而是先补齐 Agent24 的 **按 run workspace contract**。否则 ACP 能对话，却不能安全地在正确的 Open Design 项目目录中生成和修改 artifact。

## 3. 建议冻结的技术选择

| 决策 | 本计划选择 | 选择原因 |
| --- | --- | --- |
| 产品边界 | Agent24 统一 Shell；Open Design = Creative Workspace | 不让单一垂直应用绑架整个 Office Suite 信息架构 |
| 源码边界 | 独立 `iDoris-ai/open-design-agent24` fork；Agent24 只保存集成协议、启动与 pin | 上游同步清晰，避免 monorepo 内大规模 vendor diff |
| 上游版本 | release tag/commit SHA 固定；人工 review 的同步 PR | 可复现；不让 `main` 的任意提交破坏产品构建 |
| runtime 接口 | 新增 `agent24 acp`，采用 ACP-over-stdio | 符合 Open Design 官方推荐；OD 侧只需很薄的 runtime def |
| daemon 连接 | ACP CLI 发现并连接现有 `agent24d`；不可用时按明确策略失败，首版不静默拉起另一个产品 daemon | 避免出现两个 control plane 或隐蔽状态 |
| workspace 标识 | opaque `workspace_id`，不让 run API 接受任意裸路径 | 可做 canonicalization、生命周期、授权和审计；未来支持本地/临时/远端后端 |
| workspace 类型 | 首版支持 `orchestrator_scratch`，writeback 固定为 `external` | 与 Open Design 现有 provenance 契约一致，避免 OD 直接修改源仓库 |
| 权限 authority | Agent24 本机审批为唯一 authority；OD/ACP 客户端不得代替用户自动批准 | 保持 Agent24 policy 边界，不把权限下放给 Creative UI |
| UI 宿主 | Agent24 Electron 管理 OD headless sidecars，并用 `WebContentsView` 承载 Creative 页面 | 保留单一 main process，隔离两套前端构建/CSS/路由，减少上游 diff |
| 品牌层 | 通过薄 brand/product adapter 命名为 `Agent24 Creative`（工作名） | Apache-2.0 不授予上游商标权；避免深改核心 UI |
| 同步策略 | integration branch 只 merge `origin/main`，不强制 rebase；Open Design fork 通过独立 upstream-sync PR 更新 | 适合多人/多 agent 并行，历史可审计 |

## 4. 目标架构

```text
Agent24 Electron main process
├─ Agent24 renderer
│  ├─ Voice
│  ├─ Documents
│  ├─ Creative ───────── WebContentsView ─────── Open Design Web UI
│  └─ Assistant / Runs / Memory / Settings                  │
├─ agent24d (Rust, authority)                               │ HTTP + SSE
│  ├─ sessions / runs / WS events                           ▼
│  ├─ models / memory / policy / tools            Open Design daemon
│  └─ workspace registry                         projects / preview / export
└─ Open Design headless sidecars                              │
                                                              │ spawn stdio ACP
                                                              ▼
                                                        agent24 acp
                                                              │ REST + WS
                                                              ▼
                                                          agent24d
```

控制边界：

- Open Design 管：Creative 项目体验、prompt composition、设计技能、artifact、preview、export。
- Agent24 管：身份、模型、Agent Loop、memory、policy、审批、工具执行、调度、用量、审计。
- Agent24 Shell 管：进程生命周期、窗口/宿主能力、Creative 导航、健康检查和用户可见故障恢复。
- Git/writeback/deploy：始终在 Open Design 之外，由 Agent24 orchestrator 或明确的后续流程管理。

## 5. 工作分解

每个阶段必须独立通过验收门禁后才能进入下一阶段。估时按 1 名熟悉两个代码库的工程师计算，不包含外部评审等待时间。

### P0 — 决策冻结与基线（1–2 天）

交付物：

- ADR：产品边界、源码边界、ACP、workspace authority、Electron 宿主方式。
- Open Design pin 文件：上游仓库、tag、commit、校验值。
- 集成风险登记：协议、权限、文件系统、打包、许可证、上游漂移。
- 跨仓库版本兼容矩阵草案。

验收：

- 所有 owner 明确；不存在“两个 daemon 都是 authority”的模糊职责。
- 明确 MVP 支持/不支持的 Open Design 功能清单。
- 未创建产品代码耦合前即可撤销所有选择。

### P1 — 建立 Open Design 独立 fork 与无修改基线（0.5–1 天）

交付物：

- 创建 `iDoris-ai/open-design-agent24` fork（需在本计划批准后执行）。
- 配置 `upstream = nexu-io/open-design`。
- 从固定 release/tag 建立 integration branch。
- 在 CI 中原样构建和运行上游测试，记录基线耗时和已知失败。

验收：

- fork 的首个构建不含 Agent24 产品改动。
- 能从干净 clone 复现 web、daemon、packaged/headless 构建。
- upstream remote 与 pin 可机器校验。

### P2 — Agent24 workspace contract（4–7 天）

这是后续所有工作的一票否决前置。

建议契约：

- 新增 workspace registry：创建、查询、释放/过期。
- `RunCreate` 接受 `workspace_id`，而不是任意文件路径。
- workspace 记录至少包含：kind、canonical root、provenance、base revision、writeback policy、lifecycle owner。
- 首版 kind：`orchestrator_scratch`；writeback：`external`。
- 每个 run 的文件工具和 shell cwd 绑定到自己的 canonical workspace root。
- 事件和审计记录 workspace identity，不把宿主绝对路径暴露给不需要它的 UI。
- 对路径穿越、symlink escape、workspace 删除竞态、run 并发和恢复建立测试。

验收：

- 两个并行 run 在不同 workspace 中读写，互不可见。
- 任意裸路径不能通过公开 run API 获得访问权限。
- run 取消、失败、daemon 重启不会错误清理另一个 workspace。
- OpenAPI、Rust 类型、TS 类型和 contract tests 零漂移。

### P3 — `agent24 acp` bridge（5–8 天）

支持的 ACP 最小集：

- `initialize`
- `session/new`
- `session/load`
- `session/set_config_option` 或 `session/set_model`
- `session/prompt`
- `session/cancel`
- `session/update`

事件映射：

| Agent24 | ACP/Open Design |
| --- | --- |
| `run.started` | bounded status update |
| `model.delta` | `agent_message_chunk` |
| `tool.started` | `tool_call` |
| `tool.completed` | `tool_call_update` |
| `approval.required` | pending status；审批仍留在 Agent24 host |
| `run.completed` | prompt response + usage |
| `run.failed` / `run.cancelled` | JSON-RPC failure/terminal result |

关键规则：

- stdout 只输出 ACP JSON-RPC；诊断只写 stderr。
- ACP `cwd` 必须先解析成 Agent24 `workspace_id`，不得直接绕过 registry。
- Open Design 的自动 permission 选择不构成 Agent24 审批；bridge 不接受客户端替 Agent24 做最终授权。
- session/load 要保持 Agent24 session 与 OD conversation 的稳定映射。
- 模型选择只做可验证映射，未知模型 fail closed。

验收：

- 使用通用 ACP contract fixture 完成初始化、连续两轮会话、取消、工具事件、错误和 usage。
- 无 daemon、token 过期、WS 断线、run 恢复均有确定错误语义。
- 一次需要审批的 run 只能从 Agent24 审批面继续，不能由 OD 自动放行。

### P4 — Open Design Agent24 runtime adapter（2–3 天）

上游 fork 的改动限制为：

- 一个 `RuntimeAgentDef`：`agent24`。
- registry 注册。
- 检测/version/model probe 所需的最少测试。
- 品牌/文案仍不在本阶段修改。

优先复用 Open Design 已有 `acp-json-rpc` engine，不新增 Agent24 专用事件 parser。

验收：

- Open Design 能检测 `agent24` runtime。
- 从 OD 发起的一轮请求由 `agent24d` 执行并流式显示。
- cancel、两轮 session resume、工具状态、终态与错误均正确。
- 对上游核心 engine 的修改为零；若做不到，必须回到 review 门禁说明原因。

### P5 — Orchestrator workspace 端到端（3–5 天）

交付物：

- Agent24 创建 disposable scratch workspace。
- 创建/导入 OD project 时写入 `orchestratorWorkspace.kind = scratch` 等 provenance。
- OD 在 workspace 内生成文件；结果以 manifest/diff metadata 返回 Agent24。
- Agent24 明确执行保留、丢弃、导出或后续 writeback；OD 不直接写源仓库。

验收：

- 从 Agent24 创建 Creative 任务到 OD artifact 生成形成完整闭环。
- source checkout 不被 OD 直接修改。
- 结果包可审计：run、workspace、artifact、终态和错误能互相关联。
- workspace TTL/清理只清理 lifecycle owner 明确且无活跃 run 的 scratch。

### P6 — Agent24 Electron Creative 页面（4–7 天）

交付物：

- Agent24 main process 启动、发现、健康检查和关闭 OD headless sidecars。
- Creative 路由与 `WebContentsView` 生命周期管理。
- 独立 session partition、最小 preload bridge、导航限制、外链处理和 CSP。
- view bounds、窗口缩放、隐藏/恢复、崩溃重启、开发/打包模式测试。
- 清晰的 unavailable/degraded UI，而不是白屏。

验收：

- 最终只有一个 Electron main process。
- Creative 页面在 Agent24 导航内可用；切换其他 workspace 不丢失必要状态。
- OD renderer 不获得 Agent24 未授权的 Node/Electron 能力。
- macOS、Windows、Linux 至少完成 CI/冒烟矩阵定义；首个交付平台需在 P0 冻结。

### P7 — 产品化与能力回归（3–5 天）

交付物：

- 薄 brand/product adapter：`Agent24 Creative` 工作名、导航和必要文案。
- 功能回归矩阵：project、conversation、files、design systems、skills、plugins、preview、export、image/deck/document workflow。
- Agent24 models/memory/policy/usage 的产品面整合。
- 原生 Open Design runtime 是否保留的产品策略（默认只展示 Agent24，或提供高级模式）。

验收：

- Studio/preview/export/design-system 核心代码尽量不改。
- 未支持能力明确隐藏或标记，不以半可用状态发布。
- Agent24 与 Open Design 双方升级后能跑同一套 smoke suite。

### P8 — License、打包与上游同步（3–5 天）

交付物：

- 保留 Apache-2.0 LICENSE、copyright 和必要修改声明。
- 扫描第三方 bundled assets、skills、templates、字体和 vendor 代码，生成 inventory/SBOM。
- 产品命名不暗示拥有 Open Design 商标。
- 每周/每 release 检查 upstream，自动开同步 PR，不自动 merge。
- CI：upstream tests、Agent24 ACP contract、workspace isolation、desktop packaging、license scan、端到端 smoke。

验收：

- 从固定 commit 可重现桌面构建。
- upstream sync PR 能明确显示 patchset 大小、冲突和回归。
- release artifact 带齐许可证材料，无未知许可证的 bundled 内容。

### P9 — 发布门禁与灰度（2–4 天）

交付物：

- feature flag：Creative workspace 可独立关闭。
- crash/health/latency/failed-run 指标和脱敏日志。
- 数据迁移、rollback、OD sidecar pin 回退说明。
- 小范围 dogfood，再决定正式启用。

验收：

- 关闭 Creative 不影响 Agent24 其他 workspace。
- 回退 OD pin 不破坏 Agent24 主数据；scratch 数据有明确兼容策略。
- 核心 E2E：创建项目 → 两轮迭代 → preview → export → cancel/approval → restart recovery。

## 6. 时间与里程碑

| 里程碑 | 包含阶段 | 预计工作日 | 可见结果 |
| --- | --- | ---: | --- |
| M0 设计冻结 | P0 | 1–2 | ADR、契约和风险获批 |
| M1 技术纵切 | P1–P4 | 12–19 | OD 请求经 ACP 由 Agent24 执行并流式返回 |
| M2 可用 MVP | P5–P6 | 7–12 | Agent24 Creative 页面内完成真实 workspace artifact 流程 |
| M3 发布候选 | P7–P9 | 8–14 | 品牌、能力回归、许可证、同步、打包和灰度完备 |

总计：约 **28–47 个工程日**。若由 2 人在协议/桌面两条线上并行且评审及时，日历时间预计 **4–6 周**；首个可演示技术纵切预计 **12–19 个工程日**，且不能绕过 workspace 安全门禁。

估时最大不确定性：Open Design packaged/headless 接入的实际宿主边界、Agent24 per-run tool registry 重构范围、跨平台打包。

## 7. 未来改变选择的成本

以下是“在当前建议方案上改变方向”的预计成本。时间按实现、测试和文档更新合计，不含外部等待。

| 想改变的选择 | 最佳改变时点 | 若在 MVP 前改变 | 若发布后改变 | 主要代价 |
| --- | --- | ---: | ---: | --- |
| ACP → Open Design 私有 HTTP/WS adapter | P3 前 | 3–5 天 | 7–12 天 | 新 transport、认证、重连、上游 engine diff；远程 ACP 标准仍可能变化 |
| ACP → 未来标准远程 ACP | 标准稳定后 | 3–6 天 | 5–10 天 | 替换 stdio 进程生命周期，保留同一 ACP 语义与 contract tests |
| 独立 fork → Git submodule/subtree/vendor 入 Agent24 | P1 前 | 2–5 天 | 10–20 天 | 构建、锁文件、CI、历史和上游冲突集中到 monorepo |
| 独立 fork → 只抽 React 组件 | 任何时候都不建议 | 15–30 天 | 30–60 天 | 重建 daemon、workflow、preview/export，失去上游大部分价值 |
| WebContentsView → iframe | P6 内 | 1–3 天 | 3–5 天 | CSP、frame policy、宿主能力、焦点/快捷键；实现更简单但隔离与桌面桥受限 |
| WebContentsView → 深度 React/microfrontend 合并 | P6 前 | 15–30 天 | 25–50 天 | Next/Vite 路由、依赖、CSS、状态与构建系统耦合，上游同步显著变差 |
| Agent24 Shell → Open Design Electron 为主壳 | P6 前 | 7–12 天 | 20–40 天 | Voice/Documents/Assistant 迁移、daemon authority 与主进程职责反转 |
| Electron → Tauri | P6 前 | 20–35 天 | 30–60 天 | 重写 Electron host/export/updater/preload 和 OD 宿主能力；不是简单换打包器 |
| opaque workspace ID → API 直接传裸路径 | P2 前 | 1–2 天 | 5–10 天 | 安全和审计倒退，不建议；需重做授权、路径和恢复逻辑 |
| Creative 专用 workspace → Open Design 覆盖全 Suite | P7 前 | 15–30 天 | 25–50 天 | 产品 IA、状态归属、Voice/Documents/Assistant 重构 |
| 默认仅 Agent24 runtime → 同时开放 OD 原生 runtimes | P7 | 2–4 天 | 2–5 天 | 设置/UI、支持矩阵、双 authority 解释；底层代码改动较小 |
| pin release → 追踪 upstream/main | 随时可改但不建议 | 1 天 | 1–2 天 | 构建不可复现、夜间破坏风险和回滚困难 |

设计原则：越晚改变“产品壳、源码归属、workspace authority”成本越高；“runtime 展示策略、品牌文案、pin 频率”属于可低成本调整项。

## 8. 分支、提交和同步策略

### Agent24 仓库

- `feat/open-design-workspace` 是本工作的 integration branch。
- 大块实现优先使用短分支/小 PR，再合回 integration branch，避免一个不可评审的大提交。
- `main` 更新后：先 `fetch`，查看差异和测试状态，再显式 merge `origin/main`。
- integration branch 一旦共享，不 force-push、不重写别人已基于的历史。
- 不触碰其他 worktree 中的未提交文件。

### Open Design fork

- `upstream/main` 只用于跟踪。
- 产品构建固定在 release tag/commit SHA。
- Agent24 patchset 只允许四类：runtime adapter、host adapter、workspace/provenance adapter、brand/product adapter。
- 每次同步产生 PR 和兼容矩阵更新，不自动 merge。

## 9. 测试矩阵

最低必测维度：

- 协议：ACP 初始化、会话创建/恢复、模型选择、事件、取消、错误、usage。
- workspace：隔离、路径穿越、symlink、并发、TTL、崩溃恢复、source 不可写。
- policy：本机审批唯一 authority、超时、拒绝、断线恢复。
- OD 能力：项目、对话、artifact、preview、export、design-system、skills、plugins。
- desktop：启动/关闭、resize、导航、view crash、sidecar crash、dev/prod、升级/回滚。
- packaging：首发 OS 的完整打包；其余 OS 至少建立明确 CI/手测门禁。
- upstream：当前 pin、候选新 pin、Agent24 最新 main 三方组合。
- license：主 License、NOTICE（若上游新增）、第三方资产 inventory。

## 10. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| Agent24 当前无 per-run workspace | 文件污染/无法正确生成 artifact | P2 设为硬前置，不以固定全局目录做正式方案 |
| Open Design 上游变化快 | patch 冲突、回归 | 独立 fork、固定 pin、薄 patch、同步 PR、兼容矩阵 |
| 两个 daemon 职责重叠 | 状态不一致、权限绕过 | 书面 authority map；AI loop/policy 只归 Agent24 |
| OD ACP 客户端会自动选择允许权限 | 绕过 Agent24 本机审批 | ACP bridge 不把客户端选择当最终授权，审批仅由 Agent24 host 完成 |
| Electron 中嵌两套 Web runtime | CSP、焦点、快捷键、崩溃隔离复杂 | WebContentsView 隔离、专用 partition/preload、宿主 E2E |
| 第三方 bundled 内容许可证不一 | 发布风险 | P8 inventory/SBOM 与 deny/allow policy |
| Open Design 功能面很大 | MVP 范围失控 | P0 冻结能力矩阵，先 vertical slice 后逐项回归 |
| 跨平台打包差异 | 延期 | P0 指定首发平台，其他平台分级验收 |

## 11. Review 门禁

请重点 review 以下 8 项；全部确认后才开始 P0/P1 实现：

1. 同意 Agent24 为统一 Shell，Open Design 只承担 Creative Workspace。
2. 同意独立 fork，不把 Open Design 源码直接并入 Agent24 monorepo。
3. 同意首版 runtime 采用 `agent24 acp` / ACP-over-stdio。
4. 同意先做 opaque workspace contract，再做真实 artifact 集成。
5. 同意 Agent24 本机审批是唯一 authority，Open Design 不能代批。
6. 同意首版采用 Electron `WebContentsView` + OD headless sidecars。
7. 同意首发桌面平台采用 **macOS first**，Windows/Linux 保持 CI 和后续发布门禁。
8. 同意远端 fork 目标采用 `iDoris-ai/open-design-agent24`。

批准方式建议：回复“计划通过”即可采用以上全部选择；若只需调整部分内容，直接按决策编号批注即可。

## 12. 完成定义

只有同时满足以下条件，Agent24 × Open Design 融合才算完成，而不是“页面能打开”：

- 一次真实 Creative 请求由 Agent24 执行，能连续迭代并生成可 preview/export 的 artifact。
- workspace、session、run、artifact、审批和 usage 可关联审计。
- Agent24 是模型/policy/approval authority，OD 不存在旁路调用。
- 单一 Electron main process，Creative 故障不拖垮其他工作区。
- Open Design 核心能力回归通过，patchset 足够薄，上游同步可重复执行。
- 固定版本可复现构建，可关闭、可回滚、许可证材料完整。
