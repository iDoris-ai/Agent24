# ADR-006：Legacy Recovery Holds

> 状态：Proposed / G4 implementation blocked until accepted
>
> 覆盖：A24-OD-02 历史 Run 迁移、恢复与 serial admission
>
> 修订目标：ADR-002 §5/§9 中“每个非终态 Run 创建 active lease”的冲突语义

## 1. 决策问题

升级前 Agent24 可以同时存在多个 queued、running 或 awaiting-approval Run。ADR-002
要求把全部非终态 Run 原子绑定到 singleton `legacy_compat`，又要求 serial workspace
只有一个 active Run lease。当前 schema 的 partial unique index 也确实只允许一个。

不能通过取消、合并、丢弃历史 Run，或创建多个 workspace ID 指向同一旧 root 来规避
冲突。当前恢复能力只安全支持满足 parked tool turn、payload/tool 一致且未超过 72 小时
的 awaiting-approval Run；queued/running 没有可证明安全的 checkpoint，自动重放可能重复
外部副作用。

此外现有启动流程会在 approval restore 后 sweep orphan Runs，直接取消 queued/running
以及没有 pending approval 的 awaiting-approval。只加内存恢复队列无法跨第二次重启保护
任务；“审批 decision 已提交、resume 尚未执行”也已有 crash window。

## 2. 考虑方案

| 方案 | 结果 | 结论 |
| --- | --- | --- |
| 升级前排空到最多一个 Run | 实现最少，但 parked approval 可长期阻止无人值守升级 | 安全回退 |
| **durable holds + 唯一 execution lease** | 保留全部历史任务，串行恢复，新 Run 仍严格 serial | **推荐** |
| grandfather 历史并发 | 接近旧体验，但迁移期无法声称 workspace serial，双 admission 更复杂 | 拒绝 |

## 3. 推荐契约

每个迁移 cohort 中的历史非终态 Run 建立 durable recovery hold。Hold 只保护恢复归属、
证据和生命周期，**不授予工具执行权**。只有经串行 admission 取得现有 `kind=run`
唯一 lease 的任务可以执行。

```text
legacy_recovery_cohorts
  cohort_id, migration_version, legacy_workspace_id,
  root_generation, created_at, completed_at

legacy_recovery_holds
  run_id PRIMARY KEY, cohort_id, workspace_id, root_generation,
  original_status, recovery_state, approval_id,
  ready_at, reason_code, released_at
```

`recovery_state` 为 `awaiting_decision | ready | active | needs_attention | released`。
它是附加恢复状态，不修改或伪造原 `RunStatus`。

- cohort 是迁移事务读到的固定集合；普通 create/resume 不得新增成员；
- 每个 hold 必须绑定同一个 singleton legacy workspace 与 root generation；
- active Run 再次等待审批时仍持有唯一 lease；
- terminal transition、lease release 和 hold release 必须同事务；
- cohort 未排空时，新 legacy Run 返回 `409 workspace_busy`；scratch 不受影响；
- cohort 排空后，新 legacy Run 完全走普通 serial admission。

## 4. 原子迁移

结构 migration 与数据初始化分开。daemon 取得独占启动权并验证旧 root identity 后，在
一个 `BEGIN IMMEDIATE` 事务中：

1. 创建或确认 singleton `legacy_compat`；
2. 固定 cohort 并读取全部历史非终态 Run；
3. 回填 Run、其审批和已存在 tool call 的 workspace；
4. 把旧 persisted grants 仅绑定 legacy scope；
5. 为每个 Run 建立 recovery hold；
6. 写 marker、数量不变量与脱敏 migration audit。

事务必须满足：

```text
非终态历史 Run 数
= cohort 中已绑定 legacy 的 Run 数
= 未释放 recovery hold 数
```

任一 decode、identity、DB、audit 或数量验证失败都整体回滚；daemon 不开放执行接口，
也不退回全局 root。结构已升级但 marker 未完成时，下次启动重试同一初始化，不能创建
第二 cohort。迁移不改 prompt、messages、tool I/O、usage、approval decision 或旧 audit
hash chain，只追加迁移事件。

## 5. 恢复分类与执行

| 历史状态 | 恢复结果 |
| --- | --- |
| awaiting approval + pending 且 checkpoint 完整 | `awaiting_decision` |
| approval 已决定且 thread/payload 仍匹配 | 持久化 `ready` intent |
| queued/running | `needs_attention`；不自动重放、不自动取消 |
| tool/payload/72h/root identity 校验失败 | `needs_attention`；零工具调用 |
| 用户明确 cancel/abort | 正常 terminal，并原子释放 lease/hold |

Ready Runs 按 `ready_at, run_id` 稳定排序。promotion 在同一事务重读 Run、hold、decision、
workspace/root，插入唯一 Run lease并把 hold 标为 active；并发 promotion 最多一个成功。

approval decision 与 ready intent 应同事务提交，通知和事件在 commit 后发出。对升级前已
落库的 resolved decision，启动 reconciler 验证 thread/payload 后补齐 intent。

重启遇到 active hold 时：完整 awaiting-approval checkpoint 恢复原 lease 归属；
queued/running 的执行结果不确定，转为 needs-attention 并保留 lease/hold，直到用户明确
处理。DB admission 是 authority；内存 cancel map 只能作本进程去重。

## 6. 公开兼容行为

Run projection 可 additive 暴露 `workspace_recovery.state/reason/retryable`，映射为
`awaiting_decision`、`waiting_workspace`、`active`、`needs_attention`，但不暴露 root/
lease identity，也不把“approval 已批准”表述为“工具已执行”。

历史 Run 保持可 list/get/cancel；approval 仍只允许一次 decision。等待导致超过 72h 时
进入 needs-attention，不延长旧许可。scheduler 遇到 recovery drain 的 busy 必须延后，
不能累计为执行失败、触发五次失败自动 disable、虚构 Run 或丢失 schedule 归属。

## 7. 安全与恢复测试门禁

- 两 running + 一 awaiting-approval 全部迁移：三个 holds、最多一个 active lease；
- pending/approved/denied/aborted、缺失 tool row/thread、terminal 混合；
- 每个事务写点、audit 与 commit-unknown 失败都无半回填；
- decision commit 后 crash，重启仍 ready，不被 orphan sweep 取消；
- 两个 decision/promotion 并发，最多一个 executor；
- active 再次 awaiting approval 时 lease 不释放；
- terminal/cancel/promotion 竞争保持 terminal + lease + hold 原子；
- queued/running 有潜在外部副作用时零自动重放；
- 72h/tool/payload/root generation mismatch 都 needs-attention；
- cohort 存在时新 legacy 409，排空后恢复；scratch 仍互相并行；
- legacy grant 不授权 scratch；scheduler busy 不 disable；旧 binary 不能并写升级 DB。

## 8. 实现与未来改变成本

按 `<=200 changed lines` 逐层交付：ADR/fixtures、nullable bindings、cohort/hold schema、
typed store、legacy root seam、cohort snapshot、approval/tool/grant 回填、marker/invariants、
classification、sweep protection、decision+intent、ready query、promotion、terminal release、
RunManager/restart reconcile、projection、scheduler、启动屏障与 E2E。

主要冲突文件必须单 owner 串行接线：store migrations/repo、agent resume/lib、approval
broker、daemon approvals/server、scheduler。

- 方案 B 到普通 serial：cohort 自然排空后约 1–2 个清理 PR；
- 增加 running checkpoint：需要副作用幂等、结果日志和 checkpoint protocol，属于独立波次；
- 改成 grandfather 并发：需新 ADR 和双 admission，安全风险与回退成本都更高。

接受后，ADR-002 应改为：历史非终态 Run 分别建立 durable recovery hold；恢复执行时
才原子取得唯一 Run lease。迁移不取消、合并或自动重放历史 Run；不可安全恢复者保持
可见的 needs-attention，所有新 Run 继续遵守 serial invariant。
