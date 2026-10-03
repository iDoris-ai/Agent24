# ADR-004：Electron Creative host contract

> 状态：Accepted / P0 frozen
>
> 依赖：`a24.workspace.v1`、`agent24.open-design.v1`
>
> 覆盖：A24-OD-04～09

## 1. 进程所有权

Agent24 Electron main 是桌面宿主的唯一 owner：

```text
Agent24 Electron main
├─ agent24d                    owner: existing BackendManager / future SidecarManager
└─ Open Design daemon + web    owner: SidecarManager
   └─ agent24 acp (per session) owner: Open Design daemon
```

`agent24 acp` 不是 Electron 长驻 sidecar；Open Design 按 ACP session 启动它。宿主负责把 pin 匹配的绝对 executable 路径/受控 `PATH` 提供给 Open Design，并在停止 Open Design 进程树时回收其子进程。任何进程在同一时刻只有一个 lifecycle owner。

固定 pin 的现有 `--headless` 入口仍由一个签名 Open Design Electron entry 作为 lifecycle owner；Agent24 不直接使用它，因为那会形成第二个 Electron main。fork 必须提供薄的、可上游同步的 `agent24-headless` Node launcher：它只调用 pin 中已有的 `startPackagedSidecars`，不 import Electron `app`、不创建 BrowserWindow、不安装 MCP。

packaged 启动 seam 冻结为：

```text
Agent24 Electron main
└─ spawn current Agent24 Electron executable with ELECTRON_RUN_AS_NODE=1
   args: <resources>/open-design/agent24-headless.cjs --config <managed-config>
   cwd:  <resources>/open-design
   stdout: one versioned JSON ready line only
   stderr: diagnostics
   ├─ Open Design daemon sidecar
   └─ Open Design web sidecar
```

`agent24-headless.cjs` 是构建产物并受 pin/hash 校验；managed config 只包含 resource/data/runtime roots、pin version 和受控 runtime executable，不接受 renderer path。ready line 至少给出 protocol version、instance ID、loopback web/daemon endpoints 和 child ownership；secret 不进 URL/log。SIGTERM 依次关闭 web/daemon并等待，超时由 Agent24 SidecarManager 精确杀整个 owner process group/job object。dev 模式使用同一 launcher contract，只替换显式资源路径。

Open Creative 时，Electron main 恢复/创建 workspace+OD-project attachment，并通过 owner-authenticated `CreativeCapabilityBroker` 为每个 OD conversation 的 durable principal mint 短期 `creative_runtime` capability，绑定 daemon/host/sidecar generation 和允许的 session/run operations；它通过 project root 之外的 host-private runtime handoff 交给 Open Design/ACP。detach、lease expiry、sidecar generation 变化和 app quit 必须 revoke并主动关闭关联流，但不删除仍有效 workspace 的 durable principal/session entitlement。Open Design 属于 pin/hash 验证的本地 TCB，但不获得 product-host credential。

## 2. 通用 SidecarManager v1

```ts
interface SidecarSpecV1 {
  id: string
  version: string
  executable: {
    packagedRelativePath: string
    devOverrideEnv?: string
    args: readonly string[]
  }
  cwd: 'resource-root' | 'none'
  envAllowlist: readonly string[]
  ready: { kind: 'json-line' | 'http'; timeoutMs: number }
  health: { path: string; intervalMs: number; timeoutMs: number; maxFailures: number }
  shutdown: { termGraceMs: number; killAfterMs: number }
  restart: { mode: 'on-failure' | 'manual'; maxAttempts: number; backoffMs: readonly number[] }
}
```

状态：

```text
stopped → starting → ready → healthy
              └────────────→ failed
healthy → degraded → restarting → starting
任何非 stopped 状态 → stopping → stopped
```

不变量：

- `ready` 同时要求合法 handshake、预期版本和 loopback endpoint；`healthy` 还要求 health probe 成功。
- 每次 spawn 生成随机 `instanceId/generation`。旧 child 的迟到 ready、exit 或 health 结果不能修改新实例状态。
- ownership 记录 `sidecarId, instanceId, pid, processGroup/jobObject, endpoint, resourceRoot, startedAt`。
- 只可终止本 manager 创建且 ownership 仍匹配的 process tree；禁止 `pkill -f`、按名称 kill 或扫描并终止不明进程。
- 用户 stop 后不自动 restart；异常退出才按 bounded backoff 和 max attempts restart。
- app quit：停止接收新 Creative 操作 → 销毁 view → graceful stop → 超时后精确 process-tree kill。
- 隐藏 Creative view 不停止 sidecar；sidecar lifecycle 与 navigation lifecycle 分离。
- spawn/execFile 不经过 shell；secret 不进入 argv、URL、日志或 renderer。

## 3. Packaging 与 pin

packaged app 必须包含：

- 同一 Agent24 build 的 `agent24d` 和 `agent24`；
- pin 对应的 Open Design daemon/web/static resources；
- `open-design.pin.json`、artifact checksums、Apache-2.0 NOTICE 与 bundled SBOM。
- pin 对应的 `agent24-headless.cjs` launcher 和 daemon/web entries；禁止调用上游签名 Electron `--headless` 入口。

packaged 模式只从 `process.resourcesPath` 解析，不 fallback 到系统 `PATH`。开发模式只允许显式环境变量 override，并在日志和 UI status 标记 `devOverride=true`。启动前验证版本、manifest、SHA-256、平台后缀与 executable bit；失败时进入 `failed/unavailable`，不启动未知 binary。

macOS 为首发完整 smoke 平台。Windows/Linux 在 MVP 至少执行构建、资源解析、启动/停止 smoke；发布各平台之前必须升级为该平台完整 Electron E2E。

## 4. Creative WebContentsView

创建顺序固定为：

```text
validate workspace_id
→ Agent24 workspace lease/mount handoff
→ Open Design sidecar healthy
→ create isolated WebContentsView
→ load manager-owned exact origin
→ publish ready(workspace_id)
```

- view 只由 main 创建/销毁；renderer 不接触 `WebContentsView`、URL、端口、token 或 root。
- view lazy-create；route 切换时 show/hide，同一 workspace 不重复创建。
- workspace 切换先完成旧 workspace 的 detach，再 attach 新 workspace；不能复用旧 token/root。
- main 统一设置 bounds，并处理 resize、maximize、fullscreen、sidebar/layout 变化。
- `did-fail-load`、`render-process-gone`、`unresponsive` 产生 typed degraded status。
- view crash 只重建 view，不重启健康 sidecar；sidecar crash 保留 workspace identity，恢复 healthy 后 reload view。
- app quit 先阻止 renderer 新调用，移除/销毁 view，再按第 2 节停止 sidecars。

## 5. Session、CSP 与导航

- Creative 使用按 app instance + opaque workspace ID 派生的**非持久**独立 partition（无 `persist:` 前缀，例如 `agent24-creative-v1:<instance-hash>:<workspace-hash>`）；不得从用户路径生成 partition 名。hide/show 保留同一 view/session；detach 或 workspace 切换后销毁 view，清除该 partition 的 cookie、localStorage、cache 与 service worker。app restart 由 host 重新注入 sidecar auth，不依赖旧 cookie。
- `nodeIntegration=false`、`contextIsolation=true`、`sandbox=true`；默认无 Agent24 preload。
- 若上游功能确需 host bridge，必须另行定义 `CreativeHostBridgeV1` 的最小方法和 schema，不能加载 Agent24 通用 preload。
- Agent24 defaultSession 的 CSP 不为 Creative 放宽。Creative CSP 只安装在其 partition，允许 pin 版本所需的 loopback app/static/SSE/WebSocket source。
- exact origin allowlist 来自当前 sidecar handshake；不是任意 `localhost`，也不能由 renderer 提供。
- `window.open`、跨 origin navigation、`file://`、自定义协议默认拒绝。外部 `http/https` 只能经 main 的独立 allow/confirm policy。
- token 通过专属 session header/cookie 或等价受控机制注入，不放 query string，不暴露给 Agent24 renderer。

## 6. Opaque renderer IPC

唯一公开 contract：

```ts
interface CreativeHostApiV1 {
  openCreativeWorkspace(workspaceId: string): Promise<CreativeOpenResult>
  closeCreativeWorkspace(): Promise<void>
  getCreativeStatus(): Promise<CreativeStatus>
  onCreativeStatus(listener: (status: CreativeStatus) => void): () => void
}

type CreativeStatus =
  | { state: 'disabled' }
  | { state: 'starting'; workspaceId: string }
  | { state: 'ready'; workspaceId: string }
  | { state: 'degraded'; workspaceId?: string; code: string; retryable: boolean }
  | { state: 'stopped' }
```

main 必须验证 IPC sender/frame 属于当前 Agent24 window、校验 workspace ID schema，并通过 workspace authority 获取 lease。`CreativeHostApiV1` 的 renderer 不得传 path、URL、port、token、executable、argv 或 approval decision。Agent24 自身的 approval UI 使用 ADR-005 定义的另一条窄 typed IPC。现有通用 `backendProxy` 不能成为 Creative host 的长期 API。

## 7. 故障语义

| 故障 | 必须行为 |
| --- | --- |
| Creative renderer crash | 重建 view；保留 sidecar/workspace lease |
| Open Design daemon crash | degraded；bounded restart；healthy 后同 workspace reload |
| `agent24d` crash | 禁止新 run；等待产品 daemon 恢复，不创建第二 authority |
| `agent24 acp` crash | 当前 ACP session 明确失败；不静默切换 agent/control plane |
| app quit | 精确回收全部 owned child/process tree；workspace 按 owner policy 保留或后续释放 |
| stale process record | ownership 不能完整验证则不 kill；报告诊断 |

单次 crash 不能自动删除 scratch workspace；清理仍受 workspace lease/state contract 管理。

## 8. 最低 Electron E2E

1. app 只有一个 Electron main 和一个 Agent24 authority；
2. 首次打开 Creative 才启动 Open Design；
3. ready/health 后加载 view；bounds 在 resize/hide/show/fullscreen 正确；
4. 任意 origin navigation、`window.open`、Node/Electron/root/token 访问失败；
5. Creative CSP 允许 pin 所需资源并拒绝未授权 origin；
6. view crash 与 sidecar crash 分别按第 7 节恢复；
7. 非 `workspaceId` 的 renderer path/URL 参数被 schema 拒绝；
8. 两个 workspace 使用不同 ephemeral partition；切换并清理后不串 root、token、cookie、localStorage、cache、project 或 conversation；
9. app quit 后无 owned orphan；Open Design 与 agent24d 不互相误杀；
10. dev override 和 packaged resources 两条启动路径均测试。

## 9. 实现顺序

```text
A24-OD-05 SidecarManager
  → A24-OD-06 isolated session/security
  → A24-OD-07 WebContentsView host
  → A24-OD-08 navigation/opaque IPC
  → A24-OD-09 Electron E2E
```

A24-OD-04 CLI packaging 可与 SidecarManager 并行，但两个分支都会修改 `apps/desktop/package.json`，必须串行合入或显式 rebase。`main.ts`、preload、IPC shared types 由上述顺序中的当前 owner 独占。
