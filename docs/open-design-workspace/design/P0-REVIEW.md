# P0 门禁记录

> 审查模型：`gpt-5.6-sol` / xhigh
>
> 日期：2026-09-19
>
> 当前 Gate：`PASS`

## 已修复进冻结稿

- pin-specific ACP load：`resumesSessionViaAcpLoad`、`openCodeSessionId`、load `sessionId` 与 pinned cancel request；
- Agent24 denied → ACP `failed`，不再发送不存在的 `denied` ToolCallStatus；
- host lease 加 instance/generation/heartbeat/expiry/restart fencing；
- legacy `awaiting_approval` Run/approval/lease 原子 backfill；
- Creative 改为 per-workspace ephemeral partition 并在 detach 清理；
- 非 legacy workspace 强制使用 immutable workspace-bound Session；
- 冻结无第二 Electron main 的 `agent24-headless.cjs` Node-mode launcher；
- root 的平台 file identity 与 lease lazy-expiry CAS；
- compatibility matrix 不再给 ACP 发明 minor version。

## Authority 决策

用户已批准 [ADR-005](ADR-005-CAPABILITY-AUTHORITY.md) 的推荐路线：capability auth + pin/hash 验证的 Open Design TCB。当前全权 discovery bearer 必须拆分，Creative runtime 只能持有 workspace-scoped 短期 capability。

经过两轮修订，最终 contract 进一步冻结：capability discovery 无 token；legacy/Creative mode 互斥；desktop-owned daemon；project root 外 handoff；逐事件 revoke；durable attachment/principal ownership；token/sidecar/daemon rotation 只恢复原 conversation。5.6-SOL 最终复审无 blocking/high finding。

放行结果：

1. ADR-005 已并入 ADR-001/002/003/004 和风险台账；
2. A24-OD-00、分支 ownership 和 4–7 日增量已登记；
3. 最终 SOL 复审 `PASS`；
4. 先实现 capability security layer，再启动真实 Creative runtime。
