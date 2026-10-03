# Agent24 × Open Design — Prototype-First 里程碑计划

> 修订日期：2026-09-29
>
> 状态：等待用户确认后执行
>
> 长期目标不变：完整产品化、安全加固、桌面集成、跨平台、主干合并、正式发布。

## 1. 本次路线修正

此前执行顺序把 P2 workspace authority、安全、恢复与异常路径放在了真实 Creative 正向链路之前。长期方向没有错，但不适合作为产品价值验证的第一交付。

从本修订开始改为：**先跑通最小正向原型，再把已经设计好的 authority / lifecycle / recovery 能力按里程碑叠加。**

原则：

- Open Design 继续保持独立 fork、薄改动、可跟随上游更新。
- Agent24 继续是统一 Shell / AI authority；Open Design 继续是 Creative Workspace。
- 已完成的 workspace / lease / authority 基础保留，不返工、不删除。
- 原型阶段不追求完整 fail-closed、安全恢复、跨平台完备性；这些进入后续增强阶段。
- 先证明产品体验和技术主链路成立，再决定是否继续投资完整产品化。

## 2. Open Design 嵌入机制：原型直接复用

Open Design 已提供适合宿主产品集成的现成表面：

- `od daemon start --serve-web`：启动 daemon + Web UI，不启动 Open Design Electron；
- Web UI 与 daemon 是现成产品面，不需要复制或重写 Studio / Preview / Export；
- Agent runtime 通过声明式 `RuntimeAgentDef` 注册；
- Open Design 已有共享的 `acp-json-rpc` runtime engine。

因此 Prototype 不再先扩展 Agent24 的完整 Workspace 安全内核，而直接采用：

```text
Agent24 Electron
  └─ Creative route
      └─ WebContentsView -> Open Design --serve-web
                              |
                              └─ RuntimeAgentDef: agent24
                                      |
                                      └─ agent24 acp -> agent24d
```

Open Design fork 的目标改动仍应非常薄：优先只增加 Agent24 runtime definition / registry，以及必要的宿主兼容配置；不得深改 Studio、Preview、Export、Design System 或核心 UI。

## 3. Prototype Gate — 第一版必须先看到的结果

### M0 — 冻结最小原型合同

目标：只定义一条 happy path，不继续扩大安全范围。

原型合同：

1. Agent24 有一个 `Creative` 入口。
2. Agent24 启动/发现 Open Design `--serve-web` sidecar。
3. Agent24 用 `WebContentsView` 打开 Open Design Web UI。
4. Open Design 注册 `agent24` runtime。
5. Open Design prompt 经 `agent24 acp` 到达现有 `agent24d`。
6. 使用一个受控 workspace / cwd 完成一次真实文件生成或修改。
7. Open Design Preview 能看到这次文件变化。

原型明确暂缓：完整 capability 隔离、lease 恢复、daemon crash recovery、跨 workspace 对抗矩阵、完整 approval/grant scoping、三平台打包。

### M1 — Embedded Creative Shell

交付：

- Agent24 Creative route；
- 启动 Open Design `od daemon start --serve-web`；
- `WebContentsView` 加载其 Web UI；
- 最小生命周期：start / health / stop；
- 开发模式先跑通，不先要求完整 packaged supervisor。

验收：用户从 Agent24 导航进入 Creative，看到真实 Open Design UI，并可正常浏览项目/Studio/Preview。

### M2 — 最薄 `agent24 acp`

交付最小 ACP happy path：

- initialize；
- session/new；
- session/prompt；
- streaming message；
- terminal success/error；
- cancel 若现有结构容易接入则同阶段完成，否则放 M4。

这一阶段允许复用现有 Agent24 session/run API，不要求完整恢复语义。

### M3 — Open Design `agent24` runtime adapter

在 Open Design fork 中只做薄适配：

- 新增一个 `RuntimeAgentDef`；
- registry 注册；
- 使用现有 `acp-json-rpc` engine；
- binary/version detection；
- 不修改通用 runtime engine，除非出现真实 blocker。

验收：Open Design runtime selector 能选择/检测 Agent24，并把 prompt 发到 Agent24。

### M4 — 第一条真实 Creative E2E

这是 Prototype 的核心验收门。

用户可完成：

1. 在 Agent24 打开 Creative；
2. 创建/打开一个 Open Design project；
3. 选择 Agent24 runtime；
4. 输入一个设计需求；
5. Agent24 agent 执行并写入该 project/workspace；
6. Open Design Preview 显示生成结果；
7. 第二轮 prompt 能继续修改同一结果。

达到 M4 后，即交付第一个可判断产品价值的 **Agent24 × Open Design Prototype**。

## 4. Prototype 之后的增强里程碑

### M5 — Workspace Authority v1

把现有 #555–#562 等工作接入真实主链路：opaque workspace identity、trusted root、atomic run admission、terminal lease release、startup orphan reconciliation。

目标从“理论完整”改为“围绕已经跑通的 E2E 逐项替换临时 seam”。

### M6 — Tool / Approval 隔离

- immutable WorkspaceHandle；
- ToolContext workspace binding；
- fs / shell / explorer / subagent inheritance；
- approval / grant scope；
- event / audit workspace identity；
- 双 workspace 隔离测试。

### M7 — Recovery & Reliability

- pending approval durable resume；
- terminal lease rehydration；
- orphan/restart reconciliation；
- crash / cancellation / partial failure；
- lifecycle cleanup / TTL。

### M8 — Desktop Productization

- session partition / CSP / navigation；
- sidecar supervisor；
- packaged resource discovery；
- degraded / restart UX；
- product branding / settings / runtime defaults。

### M9 — Cross-platform + Security Hardening

- macOS / Windows / Linux matrix；
- capability isolation；
- adversarial authority tests；
- package/license/SBOM；
- upstream-sync regression suite。

### M10 — Mainline + Release

- sync latest main；
- collapse/merge integration stack；
- release candidate；
- installer / Mother Test；
- CHANGELOG/version/tag/GitHub Release/assets；
- dogfood and rollback gate。

## 5. 新的进度定义

以后不再只报一个百分比。

- **Prototype progress**：M0–M4，回答“现在能不能看到并使用集成产品”。
- **Productization progress**：M5–M10，回答“是否达到可正式发布的长期标准”。

当前状态（本修订时）：

- Prototype：基础组件很多已存在，但主链路未连通；下一目标是尽快完成 M1–M4。
- Productization：已有大量 P2 / sidecar / authority 基础，可在 Prototype 验证后继续复用。

## 6. 执行纪律

- 每一个 milestone 先做最短 happy path；只有真实 blocker 才增加前置工作。
- 安全、恢复、对抗测试不再阻塞首次正向 E2E，除非会造成明显数据破坏或无法运行。
- 每个 milestone 完成后必须有可演示结果，而不是只增加底层 primitive。
- Open Design fork 保持薄改；优先使用 `--serve-web`、现成 Web UI 和现成 ACP runtime engine。
- 已设计的长期安全架构继续保留在 ADR / PLAN 中，不删除，只调整落地顺序。

