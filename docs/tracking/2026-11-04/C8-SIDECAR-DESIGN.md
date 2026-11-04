# C8 设计：Open Design 托管改为经过 `SidecarManager`

> 实现以 #1580 为准（例如退出等待落在新增的 `creative-quit.ts`，不在 `main.ts` before-quit）。

> 基线：origin/main `163dd2d3`（2026-11-04）。只读调研，没有改任何文件。
> 名称说明：`docs/agent/PLAN-OD-NEXT.md` F11.4 里，"迁到 SidecarManager + agent24-sidecar-host"写的是 (a)，"保留 CreativeServeWeb"写的是 (b)。本文按 jason 的原话理解：**只用 TS 的 `SidecarManager`，这次不接 Rust 的 `agent24-sidecar-host`**。如果 jason 指的是 F11.4 那个 (b)，本文不适用。

## 0. 结论

- `CreativeServeWeb` 保留，但只做"选启动方式 + 下载前置 + 对外状态"。进程的启动、就绪、健康检查、停止、进程树回收、代次栅栏全部交给一个 `SidecarManager` 实例。
- 新增一个文件 `open-design-sidecar.ts`，里面是 OD 专用的 launcher、health 和 POSIX 进程组 tree controller。
- `sidecar-manager.ts` 需要 3 处小的通用改动：
  1. 加状态事件 `onStatus`；
  2. 允许从 `failed` 直接 `start()`（前提是没有残留 owner）；
  3. 端点 secret 改为可选（OD 没有 secret，见拍板 1）。
- 建议一个 PR 完成，另附一个可选的小 PR 迁 checkout 模式。

## 1. 现状对照

| 职责 | 现在在 `creative-serve-web.ts` 的位置 | 迁移后归属 |
|---|---|---|
| 选路径：checkout、旧的内置资源、按需组件 | `startOnce` :355-458 | **留在 CSW** |
| 下载前置：`needs-download`、`notInstalled`、`size` | :376-440，读 `component.status()` | **留在 CSW**；下载本身仍是 `open-design-component.ts`，不变 |
| 启动前校验：resourceSafeBase 祖先链、入口文件 hash、realpath 对齐（M1/M2/M4） | :396-414、:548 | **留在 CSW**，在 `manager.start()` 之前执行 |
| 写 managed config（0600）、data/runtime 目录 | :549-563 | 移进 launcher 的 `launch()`，每次启动重写一次 |
| spawn：Helper runtime（#1556）、`ELECTRON_RUN_AS_NODE`、`AGENT24_BIN`、剥掉 `A24_*` | :567-577，以及 `resolveNodeRuntimeExecutable` / `resolveBundledAgent24Cli` / `creativeChildEnvironment` | **launcher**。三个辅助函数原样复用，新增 `detached: true` |
| 就绪：JSON ready 行（≤16 KiB、协议 v2、pin、ownerPid）+ 轮询 `/api/ready` | `waitForHeadlessReady` :752、`waitUntilReady` :801 | **launcher 的 `ready` Promise**，结果是 `{ endpoint: { origin: webOrigin } }`；`parseAgent24HeadlessReady` 不变 |
| 就绪总时限 | 两段各 15 s，可能叠到 30 s | `spec.readyTimeoutMs = 30_000`；adapter 内部两段各 14 s，保证先报 OD 自己的错误文本 |
| 健康检查 | **没有**，ready 之后不再探测 | **health**：`GET ${webOrigin}/api/ready`，200 算成功（经 web 反代到 daemon，关闭中返回 503，见 fork `server.ts:8041`） |
| 崩溃检测 | `child.once('exit')` → `stopped`，phase 变 `failed`（:591） | manager `handleExit` → `failed`，清掉 handoff，整树强制回收 |
| 停止 / 进程树回收 | `terminateOwnedChild` 只对直接子进程发 SIGTERM；headless 60 s 后**拒绝强杀**（:87、:613、:653） | **stopper**：`exactTreeStopper` + 新的 tree controller（§2.1） |
| 端点 / secret 交接 | `current.origin` 内存字段 | **handoff**：`MemoryEndpointHandoff`；CSW 用 `handoff.current()` 取 origin 和 instanceId |
| 代次栅栏 | `this.child !== child` 比对，加 `starting` / `stopping` 去重 | manager 的 `generation` / `isCurrent` 挡住迟到的 ready、exit、health。**CSW 的 `starting` 去重必须保留**，因为 `manager.start()` 在非 stopped 状态下会立即返回当前状态，不会等待 |
| 视图请求栅栏 `CreativeViewRequestFence` | main.ts :101-189 | **留在 main.ts**，不动 |
| 状态上报（C3b）`headlessPhase` / `headlessPhaseForReport` | :263-319 | 改为由 manager 状态派生，见 §2.4；main.ts 的 reporter 接线不动 |
| 日志 | `console.error` 和 stderr 尾部 2 KB | **logger**：`[sidecar] event fields` 写到 console；adapter 持续读 stderr，保留尾部 2 KB，退出时打 warn |
| WebContentsView、`persist:agent24-creative` 分区、导航拦截 | main.ts :143-180 | **留在 main.ts**，不动（分区改造在范围外，见 §4） |
| Creative.tsx 的 `creativeShow` / `creativeRestart` / `creativeHide` | 页面挂载或安装完成时调用 `creativeShow`，重启按钮调用 `creativeRestart` | IPC 形状不变。main 内部加一条：**instanceId 变了就强制 reload 视图**（新实例端口可能和旧实例相同，现在只比 origin 会留下旧页面） |
| 退出 | will-quit 里 `void creativeServeWeb?.stop()`，**不 await**，进程可能成为孤儿 | main.ts 的 before-quit 改为有界等待停止，见 §2.1 |

## 2. 行为差异与决策建议

### 2.1 停止语义（60 s 拒绝强杀 → killAfterMs）

**当初为什么拒绝强杀**：headless launcher 以普通方式 spawn，和 Electron main 在**同一个进程组**里。host 唯一确定拥有的只有 launcher 这一个 pid。
- 如果只对 launcher 发 SIGKILL，daemon 和 web 两个 sidecar 会成为孤儿，跳过 OD 自己的有序关闭（fork `agent24-headless.ts:256` 的 `runtime.close()`，按 web 再 daemon 的顺序退役），还继续占着端口和数据目录。
- 又不能按组杀，因为那会把 Agent24 自己也杀掉。
- 所以只能长时间等待，然后报 `failed`，保留 owner（`observeRetainedOwner`）。ADR-004 §7 "ownership 不能完整验证则不 kill"讲的就是这个。

**建议**：
- launcher 以 `detached: true` 启动，让 pgid 等于 launcher 的 pid。这同时满足 manager 在 POSIX 上的硬约束（`sidecar-manager.ts` 要求 `dedicatedProcessGroup === true` 且 `processGroupId === pid`）。fork 的 web 和 daemon sidecar 在 POSIX 上不 detach（`packages/sidecar/src/operations.ts:354`），会留在这个组里。
- tree controller（新文件）：
  - `signal(false)`：**只对 launcher pid** 发 SIGTERM，保留 OD 的有序关闭。不要对整个组发 TERM，否则 web 和 daemon 会同时收到信号，打乱关闭顺序。
  - `signal(true)`：`process.kill(-pgid, 'SIGKILL')`。
  - `isEmpty()`：`process.kill(-pgid, 0)` 返回 ESRCH 就算空。
  - 第一次观察到组为空之后，controller 置为 `retired`，之后所有信号都不再发出，防止 pgid 被复用后误杀别的进程组。
- 宽限期：`termGraceMs: 15_000`（ADR-038 D5 的默认值），`killAfterMs: 5_000`。e2e 里记录实际停止耗时；如果 p95 超过 10 s，再回来调整。
- **已知残留，写进文档，不在 C8 修**：OD daemon 拉起 agent 运行时用的是 `detached: process.platform !== 'win32'`（fork `server.ts:14208`），也就是说 `agent24 acp` 这类子进程在自己的进程组里。强杀阶段的组 kill 够不到它们，只能靠 15 s 宽限内 OD 自己回收。另外 Electron main 本身被 SIGKILL 时，进程组也会留下来，这需要 sidecar-host 的父进程死亡监视或 fork 侧改动。
- 退出顺序：before-quit 里，如果 Creative 不是 `stopped`，执行：
  1. `preventDefault`；
  2. 销毁视图；
  3. `await creativeServeWeb.stop()`，总上限 22 s；
  4. 再次 `app.quit()`。

  `backendManager.stop()` 仍放在 will-quit，保证 OD 先于 agent24d 停止。
- Windows：manager 在 win32 上不要求进程组；而按需 v2 路径在 win32 上本来就 fail-closed（`open-design-component.ts:331` 的注释）。C8 中 launcher 在 win32 上直接返回 `failed: unsupported platform`，Job Object 留给 sidecar-host。

### 2.2 健康检查

现在 OD **没有**运行期健康检查。建议参数：

| 参数 | 值 |
|---|---|
| `healthIntervalMs` | 10_000 |
| `healthTimeoutMs` | 3_000 |
| `maxHealthFailures` | 3（D5 的默认 N） |

连续失败约 30 s 后进入 `degraded`，一次成功就回到 `healthy`，对外报 `ready`（这是 manager 现成的行为）。**`degraded` 不触发重启**：用户可能有正在进行的生成任务，OD 高负载时 `/api/ready` 变慢也算一次失败。`degraded` 只上报，同时在 CSW 状态里加一个 `degraded: true` 字段，只在 main 内部使用，不进 renderer。

### 2.3 崩溃重启

manager 没有重启策略，而 ADR-004 §2 的 `restart` 字段没有实现。

**建议在 C8 保持现状语义，也就是"懒重启"**：
- 崩溃后 manager 进入 `failed`，并强制回收整个组。
- 用户下次 `creativeShow`（换页回来，或者安装完成触发），或者点"重启"（`creativeRestart`）时，走 `failed → starting`。
- 为此要在 manager 里放开一个条件：`state === 'failed' && active === null && detached.size === 0` 时允许 `start()`。如果 stop 失败、owner 还留着，仍然禁止启动，现有测试 "retains failed ownership…" 要继续通过。
- 这样也避免为了重试先 `stop()`，因为那样会产生 D5 没有定义的 `failed → stopped` 转移（daemon 会记 `unexpected_transition`，见 `component_routes.rs:680`）。

自动有界重启（例如退避 1/5/15 s、10 分钟内最多 3 次）需要 main 主动 reload 视图、推送状态给 renderer（ADR-004 §6）。建议另开一片，见拍板 3。

### 2.4 和 C3b `headlessPhase` 对齐

- manager 加 `onStatus(listener)`：每次 `this.state` 赋值都触发，listener 抛出的异常吞掉并打日志。CSW 订阅它，然后调用 `emitStatus()`，reporter 照旧 `notify()`。没有这个事件的话，崩溃和 degraded 要等 30 s 心跳才会上报。
- `headlessPhase` 不再手工赋值，改成派生值：
  - `preflightPhase`：spawn 之前的失败（完整性校验、stateRoot、runtimeExecutable）设为 `'failed'`，下一次 start 时清掉；
  - 否则，在 `launchedSinceClear` 为真时取 `manager.status().state`；
  - 否则为 `null`。
- 这样能保住现在的语义：首次启动前报 `installed` 而不是 `stopped`；checkout 模式为 `null`；`headlessPhaseForReport` 里清掉残留 `stopped` / `failed` 的规则不变，只是把"清成 null"改成"把 `launchedSinceClear` 置为 false"。`child === null` 的判断改成 `manager.status().instanceId === undefined`。
- `healthy` 已经由 `mapSidecarState` 映射为 `ready`，`open-design-report.ts` 不用改，只更新注释。
- 需要确认的 D5 序列：
  - `installed → starting → ready`（中间可能有 degraded）；
  - 崩溃：`ready → failed → starting`；
  - 用户停止：`ready → stopping → stopped`；
  - 停止失败：`stopping → failed`；
  - 升级期间崩溃，安装回来时仍是 `failed`（保留现有测试 :666）。

### 2.5 secret

`validateReady` 要求 secret 至少 32 字节，而 OD 的 ready 行没有 secret，它的 loopback 端口也不鉴权。不要伪造一个 OD 并不校验的随机 secret 去凑合约，那只是装样子。建议在 `SidecarReady.endpoint` 上加 `auth: 'none' | 'secret'`：`'none'` 时不要求 secret，handoff 只存 origin；`'secret'` 时保持现有校验。见拍板 1。

## 3. 分片

### PR-1（主片，可以单独合入）

**改动文件**：
- `apps/desktop/src/main/sidecar-manager.ts`：加 `onStatus`、允许从 `failed` 启动、`auth: 'none'`。
- `sidecar-contract.ts`：把 endpoint 里的 `secret` 改为可选。
- 新增 `open-design-sidecar.ts`：launcher、health、tree controller、OD spec 常量。
- `creative-serve-web.ts`：headless 分支改走 manager；删掉 headless 部分的 `terminateOwnedChild`、`observeRetainedOwner`、`HEADLESS_STOP_TIMEOUT_MS`；状态里加 `instanceId` / `degraded`。
- `main.ts`：before-quit 有界等待；instanceId 变化时强制 reload。
- 文档：
  - `ARCHITECTURE-LAYERS.md` ④ 表的"进程托管"行改为现行 `SidecarManager`（TS，POSIX 独立进程组），注明 sidecar-host 未接线、Windows 还没有 Job Object；
  - `ADR-038` 的 :63、:174、C8 行；
  - ADR-004 §2 加注"restart 尚未实现，C8 是懒重启"。

**vitest**：
- `sidecar-manager.test.ts` 新增：onStatus 的顺序和去重；无 owner 时可从 failed 启动；有残留 owner 时禁止；`auth: 'none'` 通过、`'secret'` 时弱 secret 仍被拒；listener 抛错不影响状态机。
- 新增 `open-design-sidecar.test.ts`：
  - 伪造 spawn，断言 `detached: true`、Helper 路径、`ELECTRON_RUN_AS_NODE`、`AGENT24_BIN`（保留用户覆盖）、`A24_*` 已剥掉；
  - ready 行的各种失败，以及 ownerPid 不匹配；
  - tree controller 用注入的 `kill`：TERM 只发给 pid，KILL 发给 `-pgid`，ESRCH 算空，retired 之后不再发信号，EPERM 算非空。
- `creative-serve-web.test.ts`：
  - :357 那条 "never force-kills only the owner" 改写为"宽限期后对组发 SIGKILL，组为空才算 stopped"；
  - :546-:713 的 phase 用例全部保留，改成经 manager 驱动；
  - 新增：崩溃后 `creativeShow` 得到新 instanceId；degraded 时不重启；stop 失败后 start 被拒。
- `open-design-report-wiring.test.ts` 不变，它只检查 `headlessPhaseForReport` 的调用方式。

**e2e 冒烟**（scratchpad 里的 `drive-odx.mjs`，用打包签名的 app）：
1. 原有断言全部保留（Dock 不出现多余前台应用、`AGENT24_BIN`、没有外连等）；
2. `ps -o pid,pgid` 显示 launcher 的 pgid 等于自己的 pid，且不等于 Electron 的 pgid；
3. `kill -9` daemon sidecar 后，`GET /api/v1/components`（host 视图）里 open-design 是 `failed`；再次 `creativeShow` 回到 `ready`，instanceId 变了，旧 pgid 已空；
4. 对 web sidecar 发 `kill -STOP`，约 35 s 后报 `degraded`；`kill -CONT` 后回到 `ready`；
5. 把脚本末尾的 "SIGTERM 后 3 s 就 SIGKILL" 改为 SIGTERM 后最多等 25 s，再断言 `ps | grep $HOME` 里没有任何 OD 进程，并记录停止耗时。这一步要先确认 Electron 收到 SIGTERM 会走 before-quit。

**回退风险**：中等。
- 最可能出问题的是 `detached` 之后 OD 的子进程行为变化（fork 有没有依赖与父进程同组），以及 before-quit 拖慢退出（上限 22 s）。
- 回退方式是直接 revert 这个 PR，不留双路径开关，免得两套路径都要测。
- reporter 协议、IPC 形状、renderer 都不变，回退面小。

### PR-2（可选，S 规模）：checkout 模式也走 manager

用第二个 launcher：解析 `[od] listening on …`，宽限 5 s。删掉剩下的 `terminateOwnedChild`。phase 仍报 `null`。先确认 `od.mjs daemon start --serve-web` 在前台运行、不会自己 fork 后退出。vitest 用 :220、:876、:889 那几条改写。e2e 不覆盖这条路径，需要手动 `A24_OPEN_DESIGN_DIR` 冒烟一次。回退风险低，只影响开发者。

## 4. 范围外的三项

| 项 | 和 C8 的关系 | 建议 |
|---|---|---|
| 非持久的 session 分区（按 app 实例 + workspace 派生，ADR-004 §5） | 只在视图层，C8 不改 main.ts 里视图的创建方式。它依赖 C8 提供的 sidecar instanceId（app 重启时由 host 重新注入认证），也是拍板 1 选项 (b) 里注入 secret 的落点 | **另开一片**，排在 C8 之后，和 "OD 端点 secret + 会话 header 注入（需要 fork 改动）" 合成一片 "Creative 隔离" |
| `CreativeRuntime` 受限令牌（ADR-005） | 令牌要绑定 sidecar 代次，C8 的 `instanceId` / `generation` 是挂钩点。但主体工作在 daemon 侧：路由级授权、broker、handoff 目录、撤销时关流 | **另开一条线**（L 规模，跨 Rust 和 fork），不和 C8 合并 |
| 每个 OD 项目绑定一个 workspace | ACP / daemon 侧（`agent24 acp` 的 `workspace_id: None`），和托管无关 | **另开**，和受限令牌同一条线，排在令牌之前或同时做 |

## 5. 需要 jason 拍板（3 条）

1. **端点 secret**：选 (a) 在合约里加 `auth: 'none'`，C8 先如实记录 OD 端口不鉴权（推荐）；还是 (b) 现在就让 fork 支持 host 生成的 secret，并由 WebContentsView 注入 header（C8 会变成跨仓、L 规模）？
2. **停止宽限和强杀**：同意把 60 s 后拒绝强杀改为 TERM 只发给 launcher、15 s 宽限、再对整个进程组 SIGKILL 吗？同意的话，就接受 OD 自己 detach 的 agent 子进程在强杀时可能残留，以及 Electron 被强杀后进程组残留这两个已知缺口，留给 sidecar-host 或 fork 修。
3. **崩溃重启**：C8 只做懒重启（和现状一致，下次打开或点重启时恢复），自动有界重启和 ADR-004 §6 的状态推送另开一片（推荐）；还是在 C8 里一起做？

