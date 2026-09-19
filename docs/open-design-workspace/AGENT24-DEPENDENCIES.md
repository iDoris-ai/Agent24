# Agent24 主干依赖台账

> 状态：P0 frozen / A24-OD-00 gate passed / A24-OD-01 GET SOL passed, LIST implementing / A24-OD-05 platform owners SOL passed, actor designing
>
> 日期：2026-09-19
>
> integration branch：`feat/open-design-workspace`
>
> 已审计 Agent24 主干：`origin/main@9dfe91a0b66d2715e59dcda150ad2437b93d2bc9`

## 1. 目的

本文件回答三个问题：

1. Open Design workspace integration 能直接复用 Agent24 主干的哪些能力？
2. 哪些能力必须先由 Agent24 feature branches 实现并进入主干？
3. 哪些当前 Agent24 工作与本集成可以并行、并不是硬依赖？

本文件是依赖状态的权威入口。实现任务不得在各自分支里重新发明 workspace、ACP 或 desktop host 契约。

## 2. 主干已经具备，可直接复用

| ID | 能力 | 当前证据 | 结论 |
| --- | --- | --- | --- |
| EXIST-01 | v1 REST、OpenAPI、Rust/TS 类型与 contract tests | `protocol/openapi.yaml`、`agent24-protocol`、`packages/api-client` | 复用生成链 |
| EXIST-02 | session + async run 生命周期 | `/sessions`、`/runs`、cancel、状态机 | 复用，不重写 agent loop |
| EXIST-03 | 模型路由与 usage | `ModelRouter`、run usage | 复用 |
| EXIST-04 | 工具、审计和 fail-closed approval | `agent24-tools`、`agent24-policy`、approvals | 复用 authority，补 workspace 上下文 |
| EXIST-05 | WS 事件 | run/model/tool/approval/terminal events | 由原生 ACP bridge 消费 |
| EXIST-06 | daemon discovery 与 bearer token | ready line、动态 port/token、CLI attach | 复用 |
| EXIST-07 | 浏览器不得直连事件 WS | 拒绝带 `Origin` 的 WS upgrade | 保留，不为 OD 放宽 |
| EXIST-08 | 单一 Electron main process 与 daemon supervisor 基础 | `BackendManager`、preload、IPC proxy | 作为多 sidecar 宿主的起点 |
| EXIST-09 | 安全文件工具原语 | beneath-only/canonicalization/symlink 防护 | 从全局根迁移到 per-run root 后复用 |

## 3. 必须进入 Agent24 主干的硬依赖

### A24-OD-00 — Capability-scoped daemon authority

状态：`IMPLEMENTED / SOL PASS / PR STACK OPEN`

目标分支：`feat/a24-capability-auth`

阻塞：A24-OD-01～09、P1 之后的所有真实 Creative runtime

必须提供：

- `product_host`、`creative_runtime` audience/claims；capability mode 的 discovery 不含 token；
- 全权 credential 只经 ready pipe 交给原始可信 parent，不写 `daemon.json`；packaged desktop 必须拥有它启动的 daemon，禁止隐式 host handover；
- opaque token hash/store、mint/revoke/expiry、workspace/attachment/principal/session/instance generation binding；durable principal ownership survives token rotation without cross-conversation access；
- action + resource 双重 authorization，Creative 明确拒绝 approval decision、grant/override、host lease、capability mint、shutdown 和跨 workspace/session/run；
- project root 外 host-private handoff；WS/event 每次发送复验、revoke 主动断流、daemon restart 全部 creative token 失效；
- packaged Creative 禁止 fallback 到 single-token；standalone legacy mode 禁止启动 Creative；
- 负向测试证明读取 discovery、控制 ACP、持有 workspace/principal A token均不能取得 host authority、workspace B 或同 workspace principal B 数据；rotation 后只恢复原 principal 的 session/run。

契约：[design/ADR-005-CAPABILITY-AUTHORITY.md](design/ADR-005-CAPABILITY-AUTHORITY.md)。预计 4–7 个工程日。该层完成并通过 SOL review 之前，不启动 Open Design sidecar。

### A24-OD-01 — Opaque workspace registry

状态：`IN_PROGRESS / REGISTRY GET SOL PASS / LIST DESIGNING`

目标分支：`feat/a24-workspace-registry`

阻塞：P2 后半、P3、P5

必须提供：

- `workspace_id`，公开 API 不接受任意裸路径；
- create/get/release/expire；
- kind、canonical root、provenance、base revision、writeback policy、lifecycle owner；
- 首版 `orchestrator_scratch` + `writeback=external`；
- 持久化、TTL 和 daemon restart 恢复；
- 路径穿越、symlink escape、删除竞态与并发测试。

兼容要求：现有非 Creative run 必须继续工作；新增字段应按冻结设计提供兼容默认语义，不借机破坏既有 CLI、schedule、TUI 或渠道 run。

### A24-OD-02 — Per-run workspace binding

状态：`PLANNED`

目标分支：`feat/a24-run-workspace-binding`

依赖：A24-OD-00、A24-OD-01

阻塞：P3、P5

必须提供：

- `RunCreate`/`RunInput`/`Run` 记录 opaque `workspace_id`；
- store migration 与查询/恢复；
- `ToolContext` 携带 workspace identity 或不可伪造的 workspace handle；
- `fs_read`、`fs_write`、`shell_exec` 与 explorer/subagent 按 run 绑定 canonical root；
- 不使用全局可变 workspace，不接受 ACP 直接传 cwd 绕过 registry；
- OpenAPI、Rust 类型、TS 生成物、fixtures、contract tests 零漂移；
- run、tool call、approval 与 workspace 可关联审计，但不向普通 UI 泄露宿主绝对路径。

验收门禁：两个并行 run 使用不同 workspace 时互不可见；取消、失败和重启不会操作错误 workspace。

### A24-OD-03 — `agent24 acp` bridge

状态：`PLANNED`

目标分支：`feat/a24-acp-bridge`

依赖：A24-OD-00、A24-OD-01、A24-OD-02 的契约与实现

阻塞：P4、P5、P6 packaged flow

必须提供：

- ACP JSON-RPC over stdio；
- `initialize`、`session/new`、`session/load`、模型选择、`session/prompt`、`session/cancel`、`session/update`；
- stdout 只含协议消息，stderr 承载日志；
- 连接现有产品 daemon，不静默拉起第二个产品 control plane；
- ACP `cwd` 只能解析到已注册 workspace；
- Agent24 WS → ACP event mapping；
- WS 断线后用 REST reconcile，定义 token 失效、daemon 不可用和终态恢复；
- Open Design/ACP permission response 不得替代 Agent24 本机审批。

验收门禁：连续两轮 session、cancel、tool events、approval、terminal state、usage 与异常路径全部通过 ACP contract tests。

### A24-OD-04 — 桌面包携带 Agent24 CLI

状态：`PLANNED`

目标分支：`feat/a24-desktop-cli-packaging`

依赖：A24-OD-03

阻塞：打包环境中的 P4/P6、P8、P9

当前桌面包只携带 `agent24d`，没有 `agent24` CLI。必须：

- macOS/Windows/Linux 包内提供匹配版本的 `agent24` 与 `agent24d`；
- 从固定资源路径启动 `agent24 acp`；
- 校验版本、可执行位、签名/notarization 和平台后缀；
- 不依赖开发机 PATH 上偶然存在的 CLI。

## 4. 可在 integration 线并行实现、最终需产品合入的桌面能力

这些不是 P2/P3 的协议硬前置，但没有它们就无法交付 Agent24 Creative 桌面产品。

### A24-OD-05 — 通用 sidecar manager

状态：`FOUNDATION + HOST PROTOCOL + PLATFORM OWNERS PASS / ACTOR DESIGNING / UNWIRED`

目标分支：`feat/a24-desktop-sidecar-manager`

要求：显式进程 ownership、动态端口/token、ready/health、重启策略、手动停止、graceful → forced shutdown、进程树清理、日志和 bounded shutdown。禁止用 `pkill -f` 之类模糊匹配终止 Open Design。

当前证据：manager、host protocol、POSIX owner 与 Windows Job owner/CI 均以不超过 200 changed lines 的 PR 栈通过 SOL exact-head 复核；platform final 分别为 `247b355e0d9c693d6f580f9372c7f16eaab2fe2e` 与 `b8cfc3ddc2054953688aa94dc096e1eab08f8f28`。foundation 未接入 renderer、IPC 或产品路由；actor/supervision 通过前不得接线，也不得退化为 PID/PGID 数字信号或 `taskkill`。

### A24-OD-06 — Creative 独立 session/CSP

状态：`PLANNED`

目标分支：`feat/a24-desktop-creative-session`

依赖：A24-OD-05

要求：独立 Electron session partition、严格 origin allowlist、独立 CSP、无 Agent24 preload/Node/Electron authority、外部导航限制。不得为了 Creative 放宽 Agent24 主 renderer 的全局 CSP。

### A24-OD-07 — WebContentsView host

状态：`PLANNED`

目标分支：`feat/a24-desktop-workspace-view`

依赖：A24-OD-05、A24-OD-06

要求：创建/销毁、bounds/resize、show/hide、导航、load failure、renderer crash、恢复与 degraded UI。renderer 只发送 opaque workspace command，不获得 sidecar URL/token。

### A24-OD-08 — Creative navigation 与 host IPC

状态：`PLANNED`

目标分支：`feat/a24-desktop-creative-navigation`

依赖：A24-OD-07

要求：Creative nav、loading/unavailable placeholder、`openCreativeWorkspace(workspaceId)`、status events。不得把 Open Design React tree 深度复制进 Agent24 renderer。

### A24-OD-09 — Electron smoke/E2E

状态：`PLANNED`

目标分支：随 A24-OD-05～08 分支交付测试，不单独拖到最后

当前 Vitest/jsdom 不覆盖 Electron main。必须补 sidecar crash、view crash、resize、navigation、shutdown、dev/packaged smoke。

## 5. Integration/Open Design fork 自己承担的能力

| ID | 能力 | 所属 |
| --- | --- | --- |
| OD-01 | 独立 fork、upstream remote、固定 tag/SHA、无修改构建基线 | Open Design fork |
| OD-02 | Agent24 `RuntimeAgentDef` 与 ACP capability probe | Open Design fork |
| OD-03 | orchestrator-scratch provenance、result/artifact manifest | integration + OD fork |
| OD-04 | Brand/Product adapter | Open Design fork |
| OD-05 | upstream sync PR、compatibility matrix | Open Design fork/integration CI |
| OD-06 | 第三方 bundled 内容 inventory/SBOM | release 流程 |

这些任务不得要求 Agent24 主干接收 Open Design 的 Studio、preview、export 或 design-system 内部实现。

## 6. 明确不是硬依赖的 Agent24 工作

### T8.5c-W-wire / ME-3d / T9

结论：**不是当前方案的硬依赖，可以并行。**

原因：Open Design 采用独立 fork + sidecar + `agent24 acp` + Agent24 workspace contract，不作为 Agent24 Domain OS 包挂载。只有未来改变产品边界、把 Open Design 改成 OOP Domain OS 时，才需要重新评估 T8.5c-W/T9 依赖。

当前 `origin/main@69baf50` 已包含 T8.5c-W-wire v5 冻结设计，但尚未包含其实现。若该实现并行进行：

- 必须从最新 `origin/main` 建新分支；
- 不使用旧的 `chore/t8.5c-w-wire-design-freeze` 或 `feat/t8.5c-w-mount-domain-wiring` 作为实现基线；
- Open Design 线不修改 OOP `domain.rs`/`os_memory*`，除非独立 review 证明必要；
- 任一方进入主干后，另一方先显式 merge 最新 `origin/main` 并重新测试。

### Cos72 workspace

`feat/me4-cos72-skeleton` 当前 parked。名称中同样有 workspace，但它是领域产品线，不是本计划需要的通用 per-run filesystem workspace contract；两者不得混为同一依赖。

## 7. 建议分支图与合并顺序

```text
origin/main (创建时取最新；审计时为 69baf50)
│
├─ A24-OD-00 feat/a24-capability-auth
│    └─ merge → main
│         └─ A24-OD-01 feat/a24-workspace-registry
│              └─ merge → main
│                   └─ A24-OD-02 feat/a24-run-workspace-binding
│                        └─ merge → main
│                             └─ A24-OD-03 feat/a24-acp-bridge
│                                  └─ merge → main
│                                       └─ A24-OD-04 feat/a24-desktop-cli-packaging
│
├─ A24-OD-05 feat/a24-desktop-sidecar-manager
│    └─ A24-OD-06 feat/a24-desktop-creative-session
│         └─ A24-OD-07 feat/a24-desktop-workspace-view
│              └─ A24-OD-08 feat/a24-desktop-creative-navigation
│
└─ OD-01 Open Design fork baseline
     └─ OD-02 Agent24 runtime adapter (契约依赖 A24-OD-03)
          └─ OD-03 orchestrator workspace/result flow

feat/open-design-workspace
└─ 只消费已 review 的主干依赖与 OD fork pin，完成跨仓集成和发布门禁
```

允许的并行：

- 用户要求 capability security layer 完成后再继续，因此第一波只实现并审查 A24-OD-00；
- A24-OD-00 合入后，A24-OD-01、A24-OD-05、OD-01 可并行；
- A24-OD-03 与 A24-OD-06/07 可在 workspace 契约稳定后并行；
- OD-02 可根据冻结 ACP contract 先写 fixture/adapter，但真实 E2E 等 A24-OD-03；
- SOL review 在每波 Luna 完成后进行，不与未稳定的同文件写入交错。

## 8. 高冲突文件所有权

后端 workspace/ACP 工作包需要串行或显式划分以下文件：

- `protocol/openapi.yaml`
- `rust/crates/agent24-protocol/src/types.rs`
- `rust/crates/agent24-store/migrations/*`
- `rust/crates/agent24-store/src/repo.rs`
- `rust/crates/agent24-tools/src/lib.rs`
- `rust/crates/agent24-tools/src/local.rs`
- `rust/crates/agent24-agent/src/lib.rs`
- `rust/apps/agent24d/src/server.rs`
- `rust/apps/agent24d/src/runs.rs`
- `rust/apps/agent24-cli/src/main.rs`
- `packages/api-client/src/openapi.d.ts`
- `packages/contract-tests/*`

桌面工作包需要串行或明确 ownership：

- `apps/desktop/src/main/main.ts`
- `apps/desktop/src/main/backend-manager.ts`
- `apps/desktop/src/main/ipc/index.ts`
- `apps/desktop/src/main/preload.ts`
- `apps/desktop/src/shared/ipc-types.ts`
- `apps/desktop/src/renderer/App.tsx`
- `apps/desktop/package.json`

任何 Luna agent 若发现必须越过分配的文件所有权，应停止并返回主线程重新排程，不自行扩大修改范围。

## 9. 状态更新规则

每个依赖使用以下状态之一：

- `PLANNED`
- `DESIGNING`
- `DESIGN_FROZEN`
- `IMPLEMENTING`
- `IN_REVIEW`
- `MERGED_MAIN`
- `CONSUMED_INTEGRATION`
- `BLOCKED`

状态更新必须附 branch、base commit、head commit/PR、测试结果和 blocker。不能只写“完成”。

## 10. 启动时的第一组动作

收到 [EXECUTION.md](EXECUTION.md) 中的完整启动口令后：

1. 刷新并审计 `origin/main`；
2. P0 冻结 capability/workspace/ACP/desktop host ADR；
3. 更新本台账状态；
4. 第一波只创建 A24-OD-00 独立 worktree/branch并完成安全层；
5. 每个 Luna 只领取一个有文件所有权的工作包；
6. 等待全部实现与测试结果；
7. 启动 5.6-SOL review；
8. 修复、复审、合并或报告阻塞。

在收到启动口令之前，不创建这些实现分支，也不创建远端 Open Design fork。
