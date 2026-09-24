# ME-4 的 `/goal` 交付契约

> 在 `~/Dev/auraai/Agent24` 的 Claude Code 会话里整段粘贴下面的代码块。
> 规划来源：[`PLAN-ME4-OS-CAPABILITIES.md`](PLAN-ME4-OS-CAPABILITIES.md)（定义/规范/停机规则）+ [`tasks.md`](tasks.md)「ME-4 台账」（状态）。
> 收工：`/goal clear`。

```
/goal 按 Agent24 docs/agent/PLAN-ME4-OS-CAPABILITIES.md 把 ME-4 全部交付，终点是 v0.5.0 发布并在干净机器上验收。涉及三个仓库：~/Dev/auraai/Agent24（iDoris-ai/Agent24）、~/Dev/auraai/sin90-design（iDoris-ai/Sin90）、~/Dev/mycelium/Cos72（MushroomDAO/Cos72）。停止条件必须全部满足才算完成：① Agent24 tasks.md「ME-4 台账」每一行都是 DONE 并回填了 PR/commit；② Sin90 docs/agent/tasks.md 的 T0.1–TS.1.1 全部 DONE；③ Cos72 docs/agent/tasks.md 全部 DONE；④ 本轮开的 PR 在三个仓库里都已合并（包括最后的 ME4-6.1.4 台账收口 PR）；⑤ 按 PLAN-ME4 §四，没有未处理的 P0/P1（Critical/High）问题；本轮新登记的 Medium/Low 已在 followups 里标 ME4→next 并写明理由；⑥ ME4-6.1.3 完成：`gh release view v0.5.0 -R iDoris-ai/Agent24 --json assets` 的资产名与 docs/RELEASE-CHECKLIST-v0.5.0.md 逐项相等，并且在 Mac mini（ssh jason@100.107.243.106）上只用已发布的文件装好 daemon 和 Sin90、Cos72 两个包之后，`agent24 os list` 显示两者都是 mounted。

【顺序】按台账依赖推进，只挑依赖已满足的 READY task；每做完一个，就把下游依赖已满足的 BACKLOG 改成 READY。主线：ME4-0.x 清账 → ME4-1.x 调度回调 → Sin90 M3 → Sin90 M4 → ME4-4.x 推理回调（设计 4.1.1 在 0.2 之后就可以并行）→ Sin90 M5 → ME4-5.1 SDK → Sin90 迁 SDK → Cos72（先建它的 .pilot.yml 和 docs/agent 七件套）→ ME4-5.4.1 wire 文档 → ME4-6.x 发布。

【分工：贵模型规划与评审，便宜模型开发】我（主会话，Opus）只做统筹：挑 task、写和冻结设计文档、审子代理交回的结果、决定能不能开 PR、维护台账。每个开发 task 交给一个 Agent 子代理：subagent_type=general-purpose，model=sonnet，不用 isolation 参数。遵守 pilot「一个 Feature 一个 worktree」：由我先建（或复用）该 Feature 的 worktree，在里面建好这个 task 的分支，再把 worktree 路径和分支名交给子代理，要求它只在那里工作、不 push、不开 PR、只用显式路径 git add；给它的 prompt 必须包含该 task 在 tasks.md 里的全文（范围、明确不做、依赖、验收命令）、相关的 PLAN-ME4 §二 规范条目、已冻结的设计文档路径，并要求它交回 diff 摘要、每条验收命令的原始输出、变异验证结果。子代理不开 PR、不改台账。我收到结果后亲自复跑验收命令，再做对抗评审：Codex（codex:codex-rescue）可用就用它；在 2026-09-29 19:28 额度恢复之前，改由一个全新上下文的 Opus 子代理（model=opus，只给 diff、task 定义和规范）加 security-review skill 来审，PR body 标「本地模型评审（Codex 未评审）」，并记进 followups 的 ME4-CODEX-DEBT。评审提的问题交回同一个子代理修（用 SendMessage 续用它的上下文），修完再审，直到干净，然后由我开 PR。设计冻结和 Codex 挑战不交给 Sonnet。

【怎么用文档】每个 task 开工前，先从它所在仓库的 docs/agent/tasks.md 读出范围、明确不做、验收命令。实现严格对照 PLAN-ME4 §二 的 S1–S6（这些是下限：设计可以更严，不可以更松）和各仓库的 architecture.md、spec.md；Sin90 的数据模型以 docs/DESIGN-LIFEOS.md 为准，改模型要先改它的 §2 表。只做当前 task。凡是带状态机或新线协议的 task（ME4-1.1.1、4.1.1、5.1.1、6.0.1，Sin90 T5.0.1），都先写设计文档，送对抗评审（Codex，额度耗尽期间为 Opus 子代理）到 approve 并冻结，然后才写代码。设计文档照 docs/design/T8.5c-W-wire.md 的格式，文档里的 Rust 片段要先 cargo check 过。冻结后如果切法变了，先改 PLAN-ME4 §三 和台账，再开工。

【怎么开发】一个 task 一个分支一个 PR；一个 Feature 一个专属 worktree，不在主 checkout 里写代码；只用 git-guard.sh add 加显式路径。改动超过 300 行，就按 transport/存储/单个 handler/REST 或 wire/黑盒拆成 stacked PR，合并父 PR 前先用 gh api -X PATCH 把子 PR 的 base 改成 main。新增的回归测试一律做变异验证（把修复改回去必须变红），每条判据都要带正对照。每交付一刀，就在 me3-status.sh 追加 4a/4b/4c 探针行。

【怎么验证】逐条跑该 task 的验收命令。凡是 `cargo test <过滤词>` 形式的，先加 `-- --list` 确认匹配到的测试数大于 0；需要 node 或 python3 的测试，工具缺失就算失败，不许 skip。然后跑全局前置（Agent24：cd rust && cargo fmt --all --check && cargo +1.98.0 clippy --workspace --all-targets -- -D warnings && cargo +1.98.0 test --workspace；Sin90 和 Cos72 用各自 tasks.md 顶部那条）。碰到内核交互的 Sin90/Cos72 task，先把 ../Agent24 ff 到 origin/main，再跑真实挂载黑盒。全绿才能提 PR，跑不过就修到过，不许标 DONE。

【提 PR 前】顺序是：自审 → Codex 挑战（用 codex:codex-rescue 子代理，给完整 diff，要求严格找 correctness、race、安全、错误处理、空测试）→ 中立裁决 → 修 → 再挑战，直到干净 → 把 ~/Dev/tools/PR-daemon ff 到最新，跑通 --selftest，再在 worktree 里跑 pre-pr-check.sh --base main，PR body 按编号逐条回应命中项。Codex 不可用时，按 CLAUDE.md Tier 2 做本地严格评审，并在 PR body 写明「Codex 未评审」。

【怎么等评审、怎么合并】PR 由外部 clestons（PR-Daemon，在 Mac mini 上）裁决：不自己 approve，不用 --admin。推送或开 PR 后，先 ListAgents 现查 pr-daemon-mini，SendMessage 发「OWNER/REPO#N @<40位sha> — 请复审」；发不出去不重试。用 Monitor 或后台 until 循环等，挂后台前先在前台跑一次判定，确认输出非空、没报错。能合并的判据只有一个：存在一条 clestons 的 APPROVED review，它的 commit_id 等于 PR 当前的 headRefOid，并且所有 check 都是 SUCCESS。submittedAt 晚于推送时间只说明有新 review，不能证明审批覆盖了当前 head。满足判据就立刻合并：Agent24 用 git-guard.sh merge-pr <n> --integration main --allow-trunk；Sin90/Cos72 在 main 开保护之前用 gh pr merge <n> --squash。合并后更新本地 main，用 safe-cleanup 清理。approve 之后，绝不再往该分支推任何 commit：台账的 DONE 和证据，由下一个 PR 顺带回填，或留给 ME4-6.1.4 的最终台账收口 PR。收到 CHANGES_REQUESTED，就按 pilot 的 review-triage 中立裁决（该改的改；不该改的在评论里讲清业务理由；不阻塞的记进 followups），修完推上去，再发复审请求。单次等待上限 60 分钟，到点就在 progress.md 记下来，回主循环；同一 PR 连续 3 次超时，就标 BLOCKED。

【等待期间做什么】等回执不算停工：回主循环，挑下一个依赖已满足的 READY task 继续做，三个仓库可以交错推进。

【状态即文档】每次状态变化都要写回对应仓库的 docs/agent/tasks.md 和 progress.md（通过上面的回填规则），跨仓库的门同时回填到 Agent24 台账。task 完成时写清「实际发生了什么」。

【什么时候可以停】只有停止条件全部满足，才能报交付。碰到需要我拍板的事（产品方向、验收口径、架构取舍、花钱、计划外的对外发布），就把相关 task 标 BLOCKED，在 progress.md 写下待决问题，继续做不受影响的 task；等所有 READY task 都做完、剩下的全被 BLOCKED 挡住时，立即停下来，带着问题清单一次性问我，不要空转。v0.5.0 的发布（包括 Sin90/Cos72 两个仓库的 Release 资产）已经授权，按冻结后的 docs/RELEASE-CHECKLIST-v0.5.0.md 执行即可，不用再问；旧的 docs/RELEASE-CHECKLIST.md 不要用。
```

## 为什么这么写

- 三个仓库各自有 `docs/agent/tasks.md`，**跨仓库的门只在 Agent24 台账里**：这样 goal 只需要盯一张总表，就知道哪里卡住了。
- **合并判据用 exact-head**（review 的 `commit_id` == `headRefOid`），不用「推送之后出现了新 review」。Sin90 和 Cos72 的 main 没开保护，服务端不会兜底，所以判据必须在客户端写死（Codex 评审第 1 轮 #16）。
- 停止条件 ⑥ 要求在干净机器上**只用发布物**真的装上去，因为 v0.5.0 的定义是「独立可装」（Codex #3）。
- 停机规则写成可以判定的条件：什么情况阻塞发布、什么时候该停下来问，都有明确标准（Codex #20）。
- 等待上限 60 分钟（pilot 模板写的是 30 分钟），因为 PR-Daemon 每小时兜底扫描一次。
