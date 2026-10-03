# Open Design 下一阶段 → 第三方工具接入 Workflow → OpenCreator 集成

> 立于 2026-10-03（M10 落地 main 之后）。本文定义 Open Design 集成线**之后**的里程碑。
> 执行状态以 [`tasks.md`](tasks.md)「Open Design / 第三方工具线台账」为准；本文只定义"做什么、怎么验收"。
> 架构现状见 [`../ARCHITECTURE-LAYERS.md`](../ARCHITECTURE-LAYERS.md)（§④ 现行路径 vs 目标形态、§7 未完成清单）。

---

## 0. 起点：M10 交付了什么、没交付什么

**已完成（M10，2026-10-03）**
- integration 分支经 #660 以 merge commit `ce861e4` 落地 main，合并后 main 的代码与 integration 的 `3ed7d38` 逐字节相同；合并前后的发布 gate 全绿。
- 设计记录经 #662 补落 main（[`../open-design-workspace/`](../open-design-workspace/README.md)：ADR-001…006、G1/G2、G8、STATUS 等）。
- 约 200 个叠加 PR 已核实并关闭；评审遗留项在 [#661](https://github.com/iDoris-ai/Agent24/issues/661)、[#663](https://github.com/iDoris-ai/Agent24/issues/663)。
- [`../design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md`](../design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md) 保留为 **M10 的历史审计记录**：M10 怎么验证、怎么落主干、哪些安全约束和发布 gate 已完成。

**按 M10 自身定义完成，但产品路径尚未生效**（详见 ARCHITECTURE-LAYERS §④、§7）：

| ADR 承诺 | 现状 |
|---|---|
| ADR-005：第三方只拿 `CreativeRuntime` 受限令牌，全权凭据不落盘 | 🟡 授权器、mint / 吊销、路由策略都在 main 上；但 daemon 默认 `legacy_single_token`，Desktop 以 `serve --port 0` 启动，`agent24 acp` 用的是 `daemon.json` 里的全权 bearer |
| ADR-002：Open Design 的 run 绑定 scratch workspace | 🟡 workspace 服务在 main 上；但 `agent24 acp` 建 session / run 时 `workspace_id: None`，走 legacy 文件权限 |
| ADR-004：通用 `SidecarManager` + Rust `agent24-sidecar-host` 托管 | 🟡 两者都在 main 上；但 Desktop 用专门写的 `CreativeServeWeb` 拉起 Open Design |

**教训**：M10 把"组件落地"和"产品路径生效"当成了同一件事来验收。从 OD-M11 起，每个里程碑都要把"**默认产品路径上可观察到的行为**"列为单独的验收项。

---

## 1. OD-M11 — 产品路径生效（Activation）

目标：让 ADR-001/002/004/005 的承诺在**默认的 Desktop 产品路径**上真正成立。

| Feature | 内容 | 验收（必须在打包后的 Desktop 上可观察） |
|---|---|---|
| F11.1 capability 启动 | Desktop `BackendManager` 以 `--auth-mode capabilities --host-bootstrap-stdio` 拉起 daemon，经 ready pipe 接收 `ProductHost` 并只留在 main 进程内存；定义 legacy CLI / TUI 与 capability daemon 的共存策略（两种模式互斥，见 ADR-005「Auth modes」） | `daemon.json` 不含任何凭据；Desktop 全部现有功能回归通过；CLI 遇到 capability daemon 的行为符合文档 |
| F11.2 Creative 附着与令牌 | 打开 Creative 时，宿主为 `{workspace, Open Design 项目}` 创建或恢复附着，按 ADR-005 向 host-private runtime 目录投递 `CreativeRuntime` 令牌；关闭 / 退出 / 代际变化时吊销 | E2E 断言：Open Design 侧进程拿不到 `ProductHost`；吊销后旧令牌被拒 |
| F11.3 ACP 绑定 workspace | `agent24 acp` 从 handoff 读取令牌和 `workspace_id`，创建 workspace-bound 的 session / run | E2E 断言：Open Design 发起的 run 带 `workspace_id`，文件工具只能落在该 workspace 根内；跨 workspace 访问被拒 |
| F11.4 托管器取舍 | 二选一，写 ADR 记录：(a) 把 Creative 托管迁到 `SidecarManager` + `agent24-sidecar-host`；(b) 承认 `CreativeServeWeb` 为正式托管器，把 `SidecarManager` / sidecar-host 标为第二个工具时再用 | ADR 已合入；落选的一方要么删除，要么在 ARCHITECTURE-LAYERS 里明确标注"保留给 TPI"，不再处于"实现了但没人用"的状态 |
| F11.5 验收 harness 回归化 | 把 M10 的 M4 exact-SHA harness（`refs/pull/654/head`）整理进 main，作为可重复运行的工作流，并加上 F11.1–F11.3 的断言 | harness 在 main 上能按 SHA 手动触发并通过 |

**依赖**：无外部依赖；和 M1（记忆）互不阻塞，可以并行。

## 2. OD-M12 — 收尾与债务

| Feature | 内容 | 来源 |
|---|---|---|
| F12.1 评审遗留 | #663 A 区 5 条（含 #277 闰秒测试假阳性、#284 编码后写失败的序列状态、#519 槽位复用退避）；#661 三条（bounds 时序回归测试、generation-aware bounds、bounds 上限） | issues #661 / #663 |
| F12.2 Workspace 产品面 | 开放 `WorkspaceService` 面向产品的构造（宿主准入 / 租约），给出 workspace 列表 / 释放 / 续期的产品入口 | ADR-002、G1/G2 |
| F12.3 写回策略 | 定义 `writeback_policy` 除 `external` 外的取值、写回审批与并发策略（写 ADR，不急于实现） | ADR-001 §7、ADR-002 |
| F12.4 跨平台完整冒烟 | ADR-001 的 MVP 只要求 macOS 完整冒烟、Windows / Linux 构建与资源校验；补 Windows / Linux 的打包后 Creative 冒烟 | ADR-001「MVP 能力边界」 |
| F12.5 上游同步节奏 | `iDoris-ai/open-design-agent24` fork 的 pin 升级流程（多久同步、谁审、跑哪些 gate） | ADR-001 §4 |
| F12.6 架构文档维护 | ARCHITECTURE-LAYERS 的状态标记随 OD-M11 / M12 更新；internal-AI（iDoris-Components）的同步副本跟进 | 本次 |

## 3. TPI-W — 第三方开源工具接入 Workflow

目标：把 M10 的经验沉淀成**可重复的流程**，让第二个工具（OpenCreator）不再走一遍 M10 的弯路。前置：OD-M11 的 F11.1–F11.3 完成，否则 workflow 里的"受限令牌 + workspace"一步没有可复用的实现。

| Feature | 产出 |
|---|---|
| W.1 接入 Playbook | `docs/design/THIRD-PARTY-TOOL-INTEGRATION-PLAYBOOK.md`。内容：边界 ADR 模板（照 ADR-001）、fork + pin + 许可证（Apache-2.0 保留 NOTICE）检查清单、托管 / 桥 / 令牌 / workspace 四个接缝的选择题、E2E harness 模板、按切片落 main 的规则（ADR 随第一片进 main；"产品路径接线"单列验收） |
| W.2 去 Creative 专名 | ADR：把 `Audience::CreativeRuntime`、`creative_attachment_id`、`CreativeCapabilityBroker`、`CreativeServeWeb` 泛化为按工具区分的形式（例如 `ToolRuntime{tool}` / `tool_attachment_id`），并给出迁移步骤；Open Design 作为第一个实例迁移过去 |
| W.3 Harness 模板 | 基于 F11.5，把 harness 参数化（工具 fork SHA、启动方式、断言集），新工具只填配置 |
| W.4 评估清单 | 接入前必答：对方是否自带 agent loop / 审批 / 记忆 / 调度（与 Agent24 的权威重叠时谁让位）；外部服务与数据出境（是否经 iDoris 脱敏）；许可证与商标；资源体积与打包 |

## 4. OC — OpenCreator 集成

**对象**：[krillinai/OpenCreator](https://github.com/krillinai/OpenCreator)（Apache-2.0，原 KrillinAI）。面向创作者的本地 AI 工作区，提供视频翻译、配音、图像 / 视频生成、文章与短视频脚本等工具，以及 Skills、MCP、定时任务、记忆。结构为 React Web 前端（`apps/web`）+ 本地 Runtime daemon（HTTP + SSE，Bearer token）+ Electron 宿主。

**与 Open Design 的关键差异**：OpenCreator **自带执行引擎**。它的 Runtime 以 `codex exec --json` 为唯一执行内核（stdin 输入 prompt，stdout JSONL 事件），并自带审批、记忆、调度、Skills、MCP 管理（见其 `docs/2026-07-03-codex-native-agent-runtime-design.md`、`docs/runtime-api-for-ui-v1.md`）。这与 ADR-001「Agent24 是唯一 AI 控制面」直接重叠，接入前必须先裁决权威归属。

| 里程碑 | 内容 | 验收 |
|---|---|---|
| OC-0 调研 spike | 读透 OpenCreator 的 Runtime 契约与执行内核替换点，评估候选接缝（**均为待验证假设**）：<br>(a) Agent24 提供兼容 `codex exec --json` 事件流的执行器，OpenCreator Runtime 把执行交给 Agent24（类比 `agent24 acp`）；<br>(b) Codex 以 Agent24 / iDoris 作为模型 provider，执行仍在 Codex（只统一模型与出境，不统一工具权限）；<br>(c) 只把 OpenCreator 的创作工具（转写、翻译、配音、渲染）作为 Agent24 的工具 / Skills 引入，不引入其 Runtime | spike 报告 + 推荐方案；按 W.4 清单逐项回答 |
| OC-1 边界 ADR | 按 Playbook 写 OpenCreator 版 ADR-001：控制面归属、审批 / 记忆 / 调度谁让位、workspace 与大媒体文件（视频、音频）的存放与 TTL、外部服务（yt-dlp、图像 / 视频生成服务、云端转写）的出境策略 | ADR 合入 main |
| OC-2 fork + pin + 托管 | fork 到 `iDoris-ai`、pin、许可证与 NOTICE；用泛化后的托管器 + 令牌 + workspace 接入 Desktop | 打包后可在 Agent24 Desktop 内打开 OpenCreator；run 是 workspace-bound，第三方只持有受限令牌 |
| OC-3 E2E 与落地 | 用 W.3 harness 跑 exact-SHA E2E；按切片落 main | 合并后 gate 全绿；ARCHITECTURE-LAYERS 与 internal-AI 文档更新 |

## 5. 顺序与依赖

```
OD-M11（产品路径生效） ──┬──▶ TPI-W（Playbook / 去专名 / harness 模板） ──▶ OC-0 → OC-1 → OC-2 → OC-3
                         └──▶ OD-M12（收尾与债务，可与 TPI-W 并行）
M1（记忆即产品，暂停中，恢复前提已满足）—— 与本线并行，互不阻塞
```

**待 jason 拍板**：
1. OD-M11 与 M1 的优先级（建议并行，M1 走 B 机 / Codex 线，OD-M11 走本机）。
2. F11.4 托管器取舍（a 或 b）。
3. OC-0 是否可以提前启动做纯调研（不写代码），与 OD-M11 并行。
