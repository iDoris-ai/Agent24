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
| ADR-005：第三方只拿 `CreativeRuntime` 受限令牌，全权凭据不落盘 | 🟡 令牌库、mint / 吊销、TTL / 代际、默认拒绝策略在 main 上；📐 **路由级资源授权未实现**：capability 模式下，除公开的 `GET /api/v1/health` 外，`CreativeRuntime` 唯一能访问的受认证路由是 `GET /api/v1/models`，其余受认证路由都要求宿主权限（`server.rs` 的 `required_operation`），durable 的 session / run 归属、事件过滤、broker / handoff 都还没有。另外 daemon 默认 `legacy_single_token`，Desktop 以 `serve --port 0` 启动，`agent24 acp` 用的是 `daemon.json` 里的全权 bearer |
| ADR-002：Open Design 的 run 绑定 scratch workspace | 🟡 workspace 服务在 main 上（**仅 Unix**）；但 `agent24 acp` 建 session / run 时 `workspace_id: None`，走 legacy 文件权限 |
| ADR-004：Creative 用按 workspace 派生的非持久 session 分区 | 📐 现为全局持久分区 `persist:agent24-creative` |
| ADR-004：通用 `SidecarManager` + Rust `agent24-sidecar-host` 托管 | 🟡 两者都在 main 上；但 Desktop 用专门写的 `CreativeServeWeb` 拉起 Open Design |

**教训**：M10 把"组件落地"和"产品路径生效"当成了同一件事来验收。从 OD-M11 起，每个里程碑都要把"**默认产品路径上可观察到的行为**"列为单独的验收项。

---

## 1. OD-M11 — 产品路径生效（Activation）

目标：让 ADR-001/002/004/005 的承诺在**默认的 Desktop 产品路径**上真正成立。

| Feature | 内容 | 验收（必须在打包后的 Desktop 上可观察） |
|---|---|---|
| F11.0a Workspace 产品面 | 开放 `WorkspaceService` 的产品构造：scratch 的创建 / 恢复、host lease、续期 / 释放、cwd resolve 路由（ADR-002 §3 的 Product REST、ADR-004 §4 的 lease / mount handoff）；Windows 暂不支持时给出明确的不可用状态 | 打包 Desktop E2E：创建 scratch → host lease → Creative attach → workspace-bound run → detach / release，全程可观察；workspace / host lease 过期后**新建** run 被拒，已有的 active run 按 ADR-002 契约完成或进入明确终态，期间根目录不删除 |
| F11.0 路由级资源授权与高权限接口隔离 | agent24d 按 ADR-005「Route/action/resource matrix」实现：从请求中提取 workspace / session / run 资源并校验；session / run 的 durable 归属（`channel=open_design`、属于令牌创建者）；事件 WS 按归属过滤并在每次发送前复验；审批只读脱敏状态；host 端的 broker 与 handoff 目录 | capability 模式下，`CreativeRuntime` 能且只能操作自己 workspace 内自己创建的 session / run；越权访问返回 403；有对抗测试覆盖。**高权限接口隔离**（ADR-002 / ADR-005）：Desktop 通用 `backendProxy`（现接受渲染进程给的任意 `/...` 路径并附带 token）必须拒绝 `/host/**`、`/capabilities/**`、`/bridge/**` 及今后的 authority 路由；`ProductHost` 不进 renderer / preload / 通用代理；高权限路由拒绝浏览器 `Origin`；审批只走独立的 typed IPC，并校验 sender / frame、当前 pending 请求与决策枚举；打包 E2E 覆盖 renderer 直接调用上述路由稳定失败 |
| F11.1 capability 启动与 daemon 所有权 | Desktop `BackendManager` 以 `--auth-mode capabilities --host-bootstrap-stdio` **自己拉起并拥有** daemon，经 ready pipe 接收 `ProductHost` 并只留在 main 进程内存；不再复用任意已发现的 daemon（现有 `backend-manager.ts` 的复用逻辑和把"capability 记录可复用"固定下来的测试都要改）；按 ADR-005「Auth modes 与 host bootstrap」处理冲突 | 逐项可观察：① 已有 legacy daemon 时，Desktop 不复用、不杀，Creative 禁用并显示冲突；② 遇到没有当前 `ProductHost` 的 capability daemon，返回 `host_authority_unavailable`，不自动接管；③ 父进程崩溃 / pipe EOF 后 daemon 有界退出；④ daemon 及其子进程树由 POSIX 进程组 / Windows Job 回收，打包 E2E 断言无孤儿；⑤ legacy 模式下禁止启动 Creative，绝不回退到全权 token；`daemon.json` 不含任何凭据 |
| F11.2 Creative 附着、broker 与 handoff | 打开 Creative 时，宿主为 `{workspace, Open Design 项目}` 创建或恢复附着；owner-authenticated 的 `CreativeCapabilityBroker` 校验 sidecar 实例、host lease、workspace、项目、conversation 后 mint `CreativeRuntime`；按 ADR-005「Discovery 与 handoff」写 handoff：位于项目根**之外**的 `~/.agent24/runtime/<daemon-generation>/creative/<host-instance>/`，目录 `0700`、文件 `0600`、原子替换，文件名由 canonical cwd + conversation ref 的 SHA-256 派生，不得进入 ToolContext / 项目扫描 / 预览 / 导出 / artifact；workspace detach、lease 过期、sidecar 代际变化、app 退出时吊销并清理 | E2E 断言：Open Design 侧拿不到 `ProductHost`；handoff 文件权限与位置符合要求；**已建立的 WS / SSE 在 revoke、过期、host lease 过期、sidecar / daemon 代际变化后被主动关闭**（不只是新请求被拒）；revoke 与新请求竞争、跨 principal 恢复、错误响应不泄露资源是否存在，都有对抗测试 |
| F11.2b Creative 分区 | 把全局持久分区 `persist:agent24-creative` 改为按 app 实例 + workspace 派生的非持久分区，detach / 切换时清理 | E2E 断言：切换 workspace 后不残留上一 workspace 的 cookie / storage |
| F11.3 ACP 绑定 workspace | `agent24 acp` 从 handoff 读取令牌和 `workspace_id`，复验 workspace / attachment / principal / generation 后，创建 workspace-bound 的 session / run；不读 discovery 文件作为运行权限 | E2E 断言：Open Design 发起的 run 带 `workspace_id`，文件工具只能落在该 workspace 根内；跨 workspace 访问被拒 |
| F11.4 托管器 | 默认方案 (a)：把 Creative 托管迁到 `SidecarManager` + `agent24-sidecar-host`，兑现 ADR-004 冻结的所有权、generation fencing、健康检查 / 重启、整个进程树回收、退出无孤儿。若选 (b) 保留 `CreativeServeWeb`：必须写新 ADR，明确 supersede ADR-004 的哪些条款，并让 `CreativeServeWeb` 达到同等保证（现状只对直接子进程发 SIGTERM / SIGKILL，没有进程组 / Job、健康退避和整树回收） | ADR 已合入；打包 E2E 断言：Creative 重启 / 崩溃 / app 退出后无孤儿进程，旧代际的请求被栅栏挡住；落选的一方删除或在 ARCHITECTURE-LAYERS 明确标注"保留给 TPI" |
| F11.5 验收 harness 回归化 | 把 M10 的 M4 exact-SHA harness（`refs/pull/654/head`）整理进 main，作为可重复运行的工作流，并覆盖 F11.0a–F11.4：authority 路由拒绝、Creative 分区隔离、父进程退出后无孤儿 daemon、handoff / revoke、ACP workspace 绑定、托管器单一所有权 | harness 在 main 上能按 SHA 手动触发并通过 |

**依赖**：F11.0a 与 F11.0 是 F11.1–F11.3 的前提（否则开启 capability 模式后 Open Design 无法建会话）。**跨仓依赖**：ADR-005 要求 Open Design fork 给通用 `RuntimeContext` 增加 `conversationId`，并由 runtime adapter 据此定位 handoff；这部分在 `iDoris-ai/open-design-agent24` 实现，需要更新 exact-SHA pin，并补跨仓契约测试（F11.2 / F11.3）。Open Design 全线当前 `PAUSED`，不与 M1 并行（2026-10-03 裁决，见 §5）；恢复后仍按上述内部依赖顺序执行。

**平台**：workspace-bound run 当前仅 Unix（Windows 上 `WorkspaceService` 不组合）。OD-M11 先在 macOS / Linux 生效；Windows 的 workspace 支持列入 OD-M12 F12.4。

## 2. OD-M12 — 收尾与债务

| Feature | 内容 | 来源 |
|---|---|---|
| F12.1 评审遗留 | #663 A 区 5 条（含 #277 闰秒测试假阳性、#284 编码后写失败的序列状态、#519 槽位复用退避）；#661 三条（bounds 时序回归测试、generation-aware bounds、bounds 上限） | issues #661 / #663 |
| F12.2 Workspace 管理界面 | 在 F11.0a 的 API 之上，给用户提供 workspace 列表 / 释放 / 续期 / 清理失败处理的界面与 CLI | ADR-002、G1/G2 |
| F12.3 写回策略 | 定义 `writeback_policy` 除 `external` 外的取值、写回审批与并发策略（写 ADR，不急于实现） | ADR-001 §7、ADR-002 |
| F12.4 跨平台 | ADR-001 的 MVP 只要求 macOS 完整冒烟、Windows / Linux 构建与资源校验：补 Windows / Linux 的打包后 Creative 冒烟；补 Windows 上的 workspace 权限实现（当前 `UnsupportedPlatform`） | ADR-001「MVP 能力边界」、`agent24-workspace` 非 Unix 分支 |
| F12.5 上游同步与 fork 待合项 | `iDoris-ai/open-design-agent24` fork 的 pin 升级流程（多久同步、谁审、跑哪些 gate）；处理 fork 仍开着的 PR #4 `fix/agent24-headless-install-root`（把 headless 安装根钉到 resources，M10 的 pin `327de28` 未包含） | ADR-001 §4、fork PR #4 |
| F12.6 架构文档维护 | ARCHITECTURE-LAYERS 的状态标记随 OD-M11 / M12 更新；internal-AI（iDoris-Components）的同步副本跟进 | 本次 |

## 3. TPI-W — 第三方开源工具接入 Workflow

目标：把 M10 的经验沉淀成**可重复的流程**，让第二个工具（OpenCreator）不再走一遍 M10 的弯路。前置：OD-M11 的 F11.0a–F11.3 完成，否则 workflow 里的"受限令牌 + workspace"一步没有可复用的实现。

| Feature | 产出 |
|---|---|
| W.1 接入 Playbook | `docs/design/THIRD-PARTY-TOOL-INTEGRATION-PLAYBOOK.md`。内容：边界 ADR 模板（照 ADR-001）、fork + pin + 许可证（Apache-2.0 保留 NOTICE）检查清单、托管 / 桥 / 令牌 / workspace 四个接缝的选择题、E2E harness 模板、按切片落 main 的规则（ADR 随第一片进 main；"产品路径接线"单列验收） |
| W.2 去 Creative 专名 | ADR：把 `Audience::CreativeRuntime`、`creative_attachment_id`、`CreativeCapabilityBroker`、`CreativeServeWeb` 泛化为按工具区分的形式（例如 `ToolRuntime{tool}` / `tool_attachment_id`），并给出迁移步骤；Open Design 作为第一个实例迁移过去 |
| W.3 Harness 模板 | 基于 F11.5，把 harness 参数化（工具 fork SHA、启动方式、断言集），新工具只填配置 |
| W.4 评估清单与验收门 | 接入前必答：对方是否自带 agent loop / 审批 / 记忆 / 调度（与 Agent24 的权威重叠时谁让位）；外部服务与数据出境——LLM 负载是否经 iDoris 脱敏（[COMPONENT-ROADMAP](COMPONENT-ROADMAP.md) C5 A-3a），**非 LLM 出站**（下载、生成服务、IM 等）是否由该工具进程自带启动断言（A-3b），并在 harness 中加反向测试（未声明的出站目标被拒）；许可证与商标；资源体积与打包 |

## 4. OC — OpenCreator 集成

> 以下对 OpenCreator 的描述来自 2026-10-03 对其 README 与两份设计文档的初读，**均为待 OC-0 按 exact SHA 核实的线索**（许可证、目录结构、执行内核、功能列表都要在 spike 报告里重新确认）。

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
OD-M11（产品路径生效） ──┬──▶ TPI-W（Playbook / 去专名 / harness 模板） ──▶ OC-1 → OC-2 → OC-3
                         │   （OC-0 纯调研无前置，可与 OD-M11 并行）
                         └──▶ OD-M12（收尾与债务，可与 TPI-W 并行）
```

**jason 裁决（2026-10-03）**：Open Design 线**在 M10 告一阶段**。本文全部里程碑（OD-M11 / OD-M12 / TPI-W / OC，含 OC-0 调研）状态为 `PAUSED`，**不与 M1 并行**；主干先做 M1（记忆即产品）。恢复时从 OD-M11 起步，F11.4 托管器取舍届时再定。§0 列出的「产品路径尚未生效」缺口在恢复前一直成立：当前 Desktop 里的 Open Design 是**演示形态**（全权 token、无 workspace 绑定），不能当作第三方隔离已生效来对外描述。
