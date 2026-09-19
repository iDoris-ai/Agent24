# ADR-005：Creative capability 与 authority 隔离

> 状态：Accepted / 2026-09-19
>
> 触发：5.6-SOL 对抗审查发现当前单 bearer 模型无法兑现 ADR-001 的 authority 承诺

## 问题

当前 `~/.agent24/daemon.json` 保存一个全权 bearer；它可调用 run、approval decision、standing grant、shutdown 等路由。Open Design daemon 与它启动的 `agent24 acp` 同一 OS 用户运行，因此两者都能读取这个 `0600` 文件。文件权限只能隔离其他用户，不能隔离同用户 sidecar。

所以“ACP 不发送 permission approval”只是协议约定，不是可执行安全边界。若 Open Design sidecar 被攻陷，它可直接调用 approval API。

## 建议选择：MVP 应用层 capability 隔离 + 明确 TCB

MVP 把 pin/hash 校验的 bundled Open Design sidecar 明确列入本地应用 TCB，但不让它获得 Agent24 的全权 API credential。该选择防止协议错误、意外调用和大部分 token 泄露造成跨 workspace/API 越权；它**不声称**能抵御同用户恶意 native/Node 进程直接访问主机文件或执行系统调用。

### Credential classes

| audience | 获得方式 | 允许 | 明确拒绝 |
| --- | --- | --- | --- |
| `product_host` | daemon ready pipe 只交给原始可信 parent；不写 discovery file | approval decision、grant、capability mint/revoke、host lease、shutdown、全部产品管理 | 不适用 |
| `creative_runtime` | product host 为某 workspace mint；短 TTL；通过 project root 外的 host-private runtime handoff 交给 OD/ACP | 指定 workspace 的 session/run/transcript/model/cancel、filtered events、精确 cwd resolve | 其他 workspace/session、approval decision、grant/override、workspace create/release、host route、shutdown |

capability mode 没有共享的 `user_client` bearer。`GET /api/v1/health` 继续无需认证；`daemon.json` 只含 pid、port、version、`auth_mode=capabilities` 和 daemon generation，不含任何可执行 API 操作的 credential。

token 为 opaque random secret，daemon 只存 hash 与 claims/revocation。claims 至少有 audience、workspace ID、`creative_attachment_id`、`creative_principal_id`、daemon/host/sidecar instance generation、created/expiry、allowed operations；每个资源路由同时校验 action 和 resource identity，不能只在 middleware 检查“已登录”。

`creative_attachment_id` 是 Agent24 product host 为一个 `{workspace, Open Design project}` 创建的 durable entitlement；`creative_principal_id` 是其中一个 Open Design conversation 的 durable lineage。两者独立于某次 bearer 生命周期并持久化。Session 保存 owner principal，Run 继承同一 owner；同 workspace 的其他 principal 仍无权读取。

### Discovery 与 handoff

- capability auth mode 下，`daemon.json` 不保存任何 bearer；它只能用于发现 endpoint、版本、mode 和 generation。
- Electron main 从 daemon ready pipe 捕获 `product_host`，保存在 main process memory。
- open Creative 时，host 恢复或创建 attachment；Open Design daemon 通过 owner-authenticated `CreativeCapabilityBroker` 为当前 project/conversation 请求 handoff。broker 校验 sidecar instance、host lease、workspace、project 和 conversation 后，以 `product_host` mint/re-mint同一 principal 的 `creative_runtime` token。
- handoff 写入 project root **之外**的 host-private runtime directory：`~/.agent24/runtime/<daemon-generation>/creative/<host-instance>/`，目录 `0700`、文件 `0600`、原子替换。filename 由 canonical cwd + conversation ref 的 SHA-256 派生；该目录不在 ToolContext root、project scan、preview、export 或 artifact manifest 范围内。
- Open Design fork 给通用 `RuntimeContext` 增加 additive `conversationId`，Agent24 runtime adapter据此向 broker 取/定位 handoff；`agent24 acp` 再验证 handoff 内 workspace/attachment/principal/generation。locator 和 conversation ref 都不是 credential，文件内容才是 scoped token。
- `agent24 acp` 不读取 discovery 作为运行 authority。workspace detach、host lease expiry、sidecar generation change 或 app quit 都 revoke 并清理 handoff。

### Auth modes 与 host bootstrap

| mode | `daemon.json` | ready pipe | Creative | 兼容行为 |
| --- | --- | --- | --- | --- |
| `capabilities` | 无 token | `product_host` | 允许 | packaged desktop 专用；外部 CLI 只能 health/status，不能 attach run |
| `legacy_single_token` | 现有 broad token | 同 token | **禁止** | standalone CLI/TUI 迁移期兼容 |

两种 mode 在一次 daemon 生命周期内互斥，不能动态降级或同时暴露。packaged desktop 必须自己 spawn 并拥有 capability-mode daemon：

- 已有 `legacy_single_token` daemon 时，desktop 可显示普通不可用/冲突状态，但不得启动 Creative、不得复用其 token、不得自动 kill；用户显式停止旧 daemon 后重试。
- 已有 capability daemon 但当前 Electron 没有其 in-memory `product_host` 时，fail closed 为 `host_authority_unavailable`，不得从 discovery 恢复或自动接管。
- capability daemon 必须绑定 parent-liveness pipe/handle 和 OS process group/job object；parent crash/pipe EOF 时 bounded shutdown。这样正常 app restart 不留下拿不到 authority 的孤儿 daemon。
- 不设计隐式 host handover。未来若需要多 host attach，必须单独 ADR 定义用户在环的认证协议。

### Route/action/resource matrix

| Surface | unauth/discovery | `creative_runtime` | `product_host` |
| --- | --- | --- | --- |
| `GET /health` | allow | allow | allow |
| models | deny | read | full |
| workspace create/list/release/host lease | deny | deny | full |
| bridge cwd resolve | deny | 仅 capability.workspace_id 精确匹配 | full |
| session create/get/transcript | deny | 仅 `channel=open_design`、同 workspace、token 创建/拥有的 session | full |
| run create/get/cancel | deny | 仅同 workspace、owned session/run；显式 ID 必须与 claims 匹配 | full |
| event WS | deny | 仅 owned session/run/workspace；每次发送前复验 | full |
| approval list/read | deny | 仅当前 owned run 的脱敏 pending status，不含 decision endpoint | full |
| approval decision、grant、override、schedule、OS/module admin、shutdown、capability mint/revoke | deny | deny | full |

默认 deny：任何未列入 `creative_runtime` allowlist 的新增 route 自动拒绝，不能因增加 route 获得权限。

这里的“owned”以 durable `creative_principal_id` 判定，而不是当前 token ID。`session/new` 原子写入 owner principal；后续 Run 继承。`session/load`、transcript、reconcile 和 cancel 都同时匹配 workspace + attachment + principal。

### Revoke、stream 与 restart

- capability store 至少保存 token hash、claims、issuer daemon generation、expiry 和 revoked state；原始 token只存在于 host memory、权限受限的 runtime handoff 和 bridge memory。
- `creative_runtime` token 不跨 daemon restart：启动生成新 daemon generation，旧 token/handoff 全部无效并清理。
- attachment/principal entitlement 与 Session/Run owner 持久化，可跨 token/sidecar/daemon generation；新 daemon 的 `product_host` 只有在重新取得同 workspace host lease、验证同 OD project/conversation 映射后，才能为原 principal remint。remint 不扩大该 principal 的 session/run 集合。
- revoke、expiry、workspace/host lease终止或 generation change 必须主动关闭关联 WS/SSE；流任务在每次发送事件前复验 capability active 状态，不能只在 HTTP upgrade 时检查一次。
- revoke 与新请求竞争时，authorization check 和 resource ownership check 使用同一 active snapshot；失败返回无资源存在性差异的 `unauthorized`/`forbidden`。
- WS/SSE 只返回该 capability 所属 session/run/workspace 事件；错误与审计不泄露其他资源是否存在。

### 兼容与 rollout

- packaged Creative 必须使用 capability auth mode，不能 fallback 到 single-token。
- 现有 standalone daemon/CLI 可在迁移期保留显式 `legacy_single_token` mode，但该 mode 禁止启动 Creative sidecar，并显示安全状态；默认切换另开主干兼容任务。
- desktop renderer 的普通 backend proxy 不可转发 `/host/`、`/capabilities/`、`/bridge/`。Agent24 自身 approval UI 使用独立 typed IPC，只接受 `{approvalId, decision}`；main 校验 sender、当前 pending 状态和 decision enum 后才以 `product_host` 调用。ADR-004 禁止 approval decision 的规则只针对 `CreativeHostApiV1`，不禁止这个 Agent24-owned approval IPC。
- 负向门禁：读取 `daemon.json`、拿到 workspace A/principal A creative token、控制 ACP stdin、伪造 workspace/session/run ID，都不能批准工具、mint grant、shutdown、读取 workspace B 或同 workspace principal B 的 session/run/event。
- 恢复门禁：token/sidecar/daemon rotation 后，同 principal 可以 `session/load`、读取 transcript、reconcile/cancel 原 Run；新 token 不能仅凭相同 workspace 接管另一个 principal。

预计新增 **4–7 个工程日**（auth middleware、claims/store、mint/revoke、filtered events、desktop handoff、迁移与负向测试），并使 A24-OD-01/02/03/05 都依赖此 capability contract。

## 更强的将来选择：非 TCB / OS sandbox

若要求“即使 bundled Open Design 进程完全恶意也不能读其他文件或执行未批准系统调用”，capability token 不够；同用户 Node 进程本身拥有 OS 权限。需要每 workspace sidecar 或 broker，加 macOS sandbox profile/entitlement、Windows AppContainer/Job isolation、Linux namespace/bubblewrap，并限制 filesystem/network/process spawn。

从建议 MVP 迁移到该模型，workspace/ACP API 可保持不变，但 desktop process topology、launcher、mount handoff 和三平台 E2E 要重做一部分。预计 **2–4 周**；macOS-only 首版约 **7–10 个工程日**。这是安全边界升级，不应在普通实现 PR 中暗中发生。

## 被拒绝的选择

- 继续使用单一 bearer，只在 ACP 代码中“不调用 approval”：不能形成安全边界。
- 仅把 `daemon.json` 设为 `0600` 或换 `HOME`：同用户父/子进程仍可读。
- 把 token 放环境变量：Open Design 是 ACP 父进程，可读并转发。

## 已批准决策

用户已批准按“应用层 capability 隔离 + Open Design 属于经过 pin/hash 验证的本地 TCB”推进 MVP。capability auth 的实现和负向测试是一票否决前置；在它完成前不得启动真实 Creative runtime。OS sandbox 保留为未来独立安全升级，不属于当前 MVP。
