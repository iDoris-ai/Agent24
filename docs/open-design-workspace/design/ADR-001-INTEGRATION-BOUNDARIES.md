# ADR-001：Agent24 × Open Design 集成边界

> 状态：Accepted / P0 frozen
>
> 决策日期：2026-09-19

## 背景

Open Design 提供完整 Creative workflow；Agent24 已有模型路由、run、工具、审批、审计和桌面 Shell。两者都包含 daemon 与宿主能力。如果不先冻结边界，workspace、权限和进程生命周期会出现两个事实来源。

## 决策

1. **Agent24 是唯一 AI control plane。** 模型选择、session/run、工具执行、审批、用量和审计由 `agent24d` 负责。
2. **Open Design 是 Creative Workspace。** 它负责项目体验、prompt composition、Studio、Design System、Skills、Preview、Export 和 artifact lifecycle，但不取得 Agent24 工具权限。
3. **Agent24 Electron 是唯一桌面 main process。** Open Design 以受管 headless sidecar 和隔离 `WebContentsView` 运行，不再启动第二个 Electron main。
4. **源码保持独立。** Open Design 使用 `iDoris-ai/open-design-agent24` fork；Agent24 仓库只保存稳定契约、host、bridge、pin 和集成测试。
5. **workspace authority 属于 Agent24。** 公开 run/IPC/ACP 边界只传 opaque `workspace_id`；只有本机可信宿主层可在已注册记录中解析 canonical root。
6. **审批 authority 属于 Agent24。** ACP/Open Design 的 permission response 不是授权凭据，不能让等待审批的 Agent24 tool 继续。
7. **source/writeback/deploy 在 Open Design 之外。** MVP workspace 为 disposable `orchestrator_scratch`，`writeback=external`；Open Design 只返回 artifact/result metadata。
8. **ACP 是 runtime 边界。** Open Design 复用通用 ACP JSON-RPC engine；`agent24 acp` 连接已存在的产品 daemon，不静默启动第二套 authority。
9. **Credential 按 audience 分层。** `product_host` 全权 capability 只在可信 parent 内存中；Open Design/ACP 仅获得 workspace/session/generation/TTL 受限的 `creative_runtime` capability。全权 credential 不写入 Creative 可读取的 discovery 文件。

## 所有权

| 能力 | 唯一 owner | 消费方 |
| --- | --- | --- |
| workspace registry、lease、TTL | Agent24 core | ACP、desktop host、integration |
| run/session/model/tool/approval/audit | Agent24 core | ACP bridge |
| Creative project/preview/export | Open Design daemon | Creative Web UI |
| child process 与 view lifecycle | Agent24 Electron main | Agent24 renderer |
| capability mint/revoke、approval decision、grant、shutdown | Agent24 product host | Creative 无权调用 |
| source checkout/writeback/deploy | Agent24 orchestrator 或显式后续流程 | Open Design 只提供结果 |
| 上游同步与 pin | Open Design fork maintainers | integration CI |

## MVP 能力边界

支持：

- Open Design project、conversation、files、Studio、Design System、Skills、Preview、Export；
- Agent24 模型、连续会话、流式文本、tool 状态、取消、usage 和等待 Agent24 审批；
- 一个 Agent24 app 内的 Creative 页面；
- disposable scratch workspace 的创建、保留、过期和审计；
- macOS 完整 smoke，Windows/Linux 构建与资源校验。

暂不支持：

- Open Design 直接修改 source checkout 或执行 deploy；
- 由 Open Design 注入任意 MCP server 或替 Agent24 自动批准工具；
- 浏览器直接连接 Agent24 bearer WebSocket；
- 在公开 API、renderer IPC 或 ACP 中传任意绝对路径；
- 多 Electron main、Tauri 重写、Domain OS/OOP mount；
- 任意 Open Design runtime 与 Agent24 runtime 混合共享同一权限上下文。

## 与 Agent24 其他主干工作的关系

T8.5c-W-wire、ME-3d、T9 和 parked Cos72 workspace 不是当前 sidecar + ACP 路线的硬依赖。若未来改成 Domain OS package mount，必须新建 ADR，不得在本实现中悄悄引入。

## 可替换性与改变成本

| 将来变化 | 保持不变的 seam | 预计工程时间 | 风险 |
| --- | --- | ---: | --- |
| WebContentsView → 原生 React 深集成 | workspace、ACP、sidecar contract | 3–6 周 | 上游 UI diff 和 CSS/路由冲突高 |
| ACP → Agent24 专用协议 | workspace 与 authority | 2–4 周 | 需同时改两仓，失去通用 runtime |
| fork → vendor/monorepo | pin 与产品边界 | 1–3 周 | 上游同步成本上升 |
| scratch → local/remote workspace backend | opaque ID、lease、Run binding | 1–2 周/新 backend | 需要新增 lifecycle 与安全测试 |
| Electron → Tauri | daemon/ACP/workspace | 4–8 周 | 宿主与打包基本重做 |

估时为契约已保持稳定、1 名熟悉代码库的工程师；不含外部 review 和签名发布等待。

## 禁止的不变量破坏

- 同一个进程链中出现两个可执行 Agent24 工具的 control plane；
- Open Design 或 renderer 从 `cwd`/path 自动获得未登记的访问权；
- 释放仍有 active run lease 的 workspace；
- 使用模糊进程匹配终止 sidecar；
- 为 Creative 放宽 Agent24 renderer 的全局 CSP；
- 日志、URL、普通 renderer IPC 暴露 bearer、sidecar token 或 canonical root。
- capability auth mode 下将 `product_host` credential 写入 `daemon.json`、环境变量或 workspace handoff。

## MVP threat model

经过 pin、hash、签名和版本校验的 bundled Open Design sidecar 属于本地应用 TCB。capability 分层必须阻止协议错误、意外调用和 Creative token 泄露造成的 Agent24 API 越权，但 MVP 不宣称能抵御同用户完全恶意的 native/Node 进程直接使用 OS 权限。该更强威胁模型需要 ADR-005 所述 per-workspace OS sandbox。
