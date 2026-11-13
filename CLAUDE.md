# CLAUDE.md

## 最高原则：只推主干（jason 2026-11-13）

这一条优先于下文所有约定。

- **只做主干流程和核心功能。** 边缘案例、低概率事件一律先不做：并发竞态的极端交错、有人篡改数据库或文件、恶意本机进程、磁盘或网络的极端故障、罕见输入组合等。需要记的写成一行 FU，以后再说。
- **评审只修影响主流程的问题。** 只修会让主流程错、不可用或越过安全底线的问题（Critical / High，以及直接打断主流程的 Medium）。Low 和边缘 Medium 不修，挂账。
- **不反复磨。** 设计文档一轮评审即冻结；每个分片默认一轮评审、一轮修复就合，不为边缘问题开第二、三轮。
- **分工。** 写码派 Grok（订阅额度）；主会话负责分析、出方案、分派任务和验收，少开 Claude 子代理。

## Mycelium Protocol 生态上下文
> 本 repo 属于 auraai 组织，参与 Mycelium Protocol 生态建设。
> 上下文来源: github.com/AAStarCommunity/Brood — 更新时自动同步

@/Users/jason/Dev/Brood/protocol/MISSION.md
@/Users/jason/Dev/Brood/orgs/auraai/PROFILE.md
@/Users/jason/Dev/Brood/orgs/auraai/INTERFACES.md

## Project Overview

> TODO: 描述本 repo 的用途、架构和开发指引
