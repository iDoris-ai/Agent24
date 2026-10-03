# P0 风险登记

> 状态：P0 frozen

| ID | 风险 | 可能性/影响 | 预防与检测 | owner |
| --- | --- | --- | --- | --- |
| R-01 | 两个 daemon 都执行 Agent 工具，authority 分裂 | 中/致命 | ADR-001；ACP attach-only；approval fixture 验证 OD 自动 permission 不生效 | A24-OD-03 |
| R-02 | `cwd` 或裸路径绕过 workspace registry | 高/致命 | public API 只收 opaque ID；cwd 只能 resolve 已登记 canonical root；symlink/TOCTOU 测试 | A24-OD-01/02 |
| R-03 | workspace TTL/释放与 active run 竞态导致数据丢失 | 中/高 | lease；releasing 状态；active lease 清零后清理；restart recovery | A24-OD-01/02 |
| R-04 | 工具仍捕获 daemon 全局 root，跨 run 越界 | 高/高 | immutable per-run capability；双 workspace 并发隔离测试 | A24-OD-02 |
| R-05 | ACP WS 断线丢失 delta/tool update，终态不一致 | 中/中 | WS-before-run；run_id filter；REST reconcile；明确 degraded update | A24-OD-03 |
| R-06 | ACP stdout 混入日志破坏 JSON-RPC | 中/高 | stdout purity test；所有诊断强制 stderr | A24-OD-03 |
| R-07 | Open Design 自动接受 permission 被误当 Agent24 批准 | 高/致命 | bridge 不发布 ACP permission authority；审批只经 Agent24 REST/host | A24-OD-03 |
| R-08 | renderer 获得 URL/token/root 或任意导航能力 | 中/高 | opaque IPC；独立 session/CSP；动态精确 origin allowlist；security E2E | A24-OD-06~08 |
| R-09 | sidecar crash/restart 误杀无关进程或留下 orphan | 中/高 | ownership + generation + process group；禁用 `pkill -f`；quit smoke | A24-OD-05/09 |
| R-10 | packaged app 偶然使用 PATH 上错误 binary | 中/高 | resourcesPath-only；manifest/version/hash 校验；无隐式 fallback | A24-OD-04/05 |
| R-11 | 上游升级破坏 ACP/runtime/daemon contract | 高/中 | release+commit pin；人工 sync PR；跨仓矩阵和 fixture | OD-01/02/05 |
| R-12 | 深改 Open Design 导致长期 fork 漂移 | 中/高 | runtime/brand adapter 薄层；禁止修改通用 ACP engine，偏离需新门禁 | OD-02/04 |
| R-13 | Apache-2.0 NOTICE、bundled third-party 或商标处理遗漏 | 中/高 | license/NOTICE/SBOM inventory；使用 Agent24 Creative 品牌 | OD-01/06 |
| R-14 | macOS 可用但 Windows/Linux 打包资源或信号模型失败 | 中/中 | macOS 首发完整 smoke；三平台资源/启动/关闭 CI；平台 native executable | A24-OD-04/09 |
| R-15 | Agent24 并行主干变更造成 schema/高冲突文件漂移 | 高/中 | 每波开始 fetch；从最新 main 建分支；integration merge main；生成物漂移检查 | integration owner |
| R-16 | scratch artifact 被误认为已写回或已部署 | 中/高 | `writeback=external`；结果 manifest 显示 pending/retained/discarded；无自动 deploy | OD-03/P5 |
| R-17 | 同用户 Open Design sidecar 读取全权 discovery bearer，直接批准/授权/关停 | 高/致命 | ADR-005 capability auth；host credential 不落 discovery；creative token 绑定 workspace/generation/TTL；负向 route/event tests | A24 auth + A24-OD-01~05 |
| R-18 | bundled sidecar 被完全攻陷后直接使用 OS 权限绕过 Agent24 application policy | 低/致命 | MVP 明确 OD 属于 pin/hash 验证 TCB；若威胁模型要求抵御则升级 per-workspace OS sandbox | product/security |

## 升级为阻塞的条件

- 任何测试证明裸路径或 OD permission 可以提升权限；
- workspace 无法在不改公开 authority 边界的情况下恢复；
- Open Design 的核心流程无法通过通用 ACP engine 接入，必须深改上游 engine；
- packaged 模式需要第二个 Electron main；
- 许可证或 bundled 内容不允许当前再分发方式；
- 主干变更与 `a24.workspace.v1` 的关键不变量冲突。

以上任一情况必须停止自动推进并重新做设计裁决。
