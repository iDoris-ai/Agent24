# LOOP — 自主开发循环执行手册

> 用法：`/loop 按 docs/specs/LOOP.md 执行下一个任务`（不带间隔，自适应节奏）。
> loop 会话的唯一职责：按本手册推进 `TASKS.md`，直到 v0.1.0 发布或被停止。

---

## 每轮迭代算法

```
1. 同步状态
   - git fetch；检查已提交 PR 的状态与 review 结论（gh pr list / gh pr view --json reviews）
   - 外部 reviewer bot（另一 GitHub 账号，约每 10 分钟一轮）会 review 每个 PR：
     · **APPROVED** → loop 执行 squash merge 进 main → 更新 TASKS.md 为 merged →
       将后续 stacked 分支 `git rebase --onto main <旧base>` 并 force-push（PR base 会自动指向 main）
     · **CHANGES_REQUESTED** → 【优先级最高】切到该分支逐条修复 → 重跑本地验证 →
       回复每条意见 → push，等 bot 复审（循环直到 APPROVED）
   - 有 PR CI 红 → 修 CI，同上优先处理

2. 选任务
   - 读 TASKS.md，取「状态 pending 且所有依赖为 in-pr 或 merged」中最靠前的任务
   - 若无可执行任务（全部 blocked 或等待 merge）→ 汇报现状，安排 20-30 分钟后的下一次唤醒
   - 里程碑门（B6→alpha tag、C8→v0.1.0、进入 M-D）需要用户确认 → 停下询问，不擅自越过

3. 实现
   - 按 SPEC-001 §2 建分支（stacked：从最近未 merge 的依赖分支切出，PR base 指向它）
   - 更新 TASKS.md 状态为 in-progress（随任务分支提交）
   - 严格按该任务在 TASKS.md 的「范围 + 验收标准」实现；范围外的问题记录下来，不顺手改
   - 遵守 SPEC-001 编码标准与 §9 安全红线；协议相关必须与 protocol/ 真源同步

4. 验证（全绿才进下一步）
   - 按 SPEC-001 §3.1 跑本地全套检查（TS 和/或 Rust + contract tests）
   - 逐条核对验收标准，不满足回到 3

5. 自我 review
   - git diff <base>...HEAD 逐文件对抗式审查（竞态/取消/fail-open/注入/泄漏/off-by-one）
   - 发现即修，修完重跑验证

6. Codex review（全局 tier 链）
   - Tier 1 codex 插件（/review 或 codex:rescue）送 diff + 验收标准，要求严格审查
   - 有问题 → 修 → 重审，循环到无 blocker；Tier 1 失败降 Tier 2（Copilot：先建 draft PR 挂 reviewer，意见处理完再 mark ready）→ Tier 3（本地严格 review，显著标注）
   - 记录轮数与结论（进 PR 描述）

7. 提交 PR
   - gh pr create，base 按 stacked 规则，描述用 SPEC-001 §4 模板（若走了 Tier 2 已有 draft PR，此步改为补全描述并 mark ready）
   - 更新 TASKS.md：状态 in-pr(#N)，commit + push 到任务分支
   - 【禁止】自行 merge；【禁止】把用户工作区的无关修改带进 commit

8. 汇报 + 续航
   - 输出本轮摘要：完成了什么任务、PR 号、review 轮数、下一个任务是什么
   - 立即进入下一任务（回到 2；stacked 开发不等 merge）
   - 若本轮耗时已长或上下文将满：安排下一次唤醒后收束本轮
```

## 停止条件（loop 主动终止并报告）

- C8 完成且发布 checklist 已交付用户 → 宣布 v0.1.0 阶段完成，结束 loop
- 连续 2 轮无可执行任务且无 PR 反馈 → 结束 loop 并说明等待事项
- 遇到需要用户决策的架构级冲突（spec 之间矛盾、依赖不可用、许可证问题）→ blocked 标注 + 结束本轮询问用户

## 硬性纪律（每轮自检）

1. **一个任务一个 PR**，不合并任务、不夹带无关改动、不动用户的未提交修改；merge 仅在外部 reviewer APPROVED 后执行（squash），从不 merge 无 APPROVED 的 PR
2. 审批/安全语义必须 **fail-closed**；测试里出现 sleep 真实时间即返工（用 mock clock）
3. `packages/api-client` 只能生成，不能手改
4. `vendor/reference/` 只读；GPL 仓库（zerostack）只看思路，禁止复制代码
5. 所有对话与汇报用中文；commit/代码/PR 标题用英文（Conventional Commits）
6. 不确定就查上游依据（ADR-026 → reference-notes → 本 specs），仍不确定则问用户而不是猜

## 汇报格式（每轮结束）

```
【Loop 第 N 轮】
✅ 完成：<task-id> <任务名> → PR #<n>（codex review <m> 轮通过）
🔄 处理：<PR 反馈 / CI 修复>（如有）
⏭️ 下一个：<task-id> <任务名>
⏸️ 等待用户：<merge 清单 / 决策点>（如有）
```
