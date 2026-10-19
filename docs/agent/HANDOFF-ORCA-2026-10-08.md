# Agent24 交接文档 → Orca 总指挥

> 写于 2026-10-08 晚，作者 Claude（Opus 5.5），交给 Orca 作为 Agent24 的总指挥。
> 所有状态都是 **2026-10-08 晚的快照**。开工前先用文末「§7 现查命令」核实一遍，查到的结果和本文不一致时，**以现查为准**。

---

## 0. 一页结论

- **当前主线 = K1 内核（集成分支 `ab/kernel`）**：剩 4 片 + E2E + 发布到 main。
- **K1 发布后的下一条主线 = 决策服务**：开工前先在本机评估 LiquidAI d1 决策模型。
- **并行线 = COMM（`ab/comm`，Hyphae 通信配置）**：COMM-6b 继续。
- **其他线都暂停**：OD-M11/M12（Open Design）、移动端；M4 / 记忆 P1 已排期但未开工。
- **jason 的工作方式**：完成前别停，只汇报关键进展；已经拍板的事别再问；需要他决定的事集中起来问，问的时候给推荐项。

---

## 1. 人和机器

| 角色 | 是谁 / 在哪 | 要点 |
|---|---|---|
| 产品负责人 | jason（GitHub `jhfnetboy`） | 用中文沟通；回复要短：结论 + 需要他定的事 |
| 唯一评审 | **PR-Daemon**（GitHub 账号 `clestons`），常驻 Mac mini | 用 ListAgents 找 `pr-daemon-mini`；找不到就发到 `bridge:session_01C69XvZ7CXoypSnPt1k7Rwz`。**它只评审，不合并** |
| 协作者 | David（GitHub `stoptheworld-java`） | 负责 Documenting 场景，K1 的 E2E 由他用 DOC-1 联调验收（#708） |
| 开发机 | 笔记本（本机）`/Users/jason/Dev/auraai/Agent24` | **所有开发都在笔记本上做** |
| Mac mini | `ssh jason@<mac-mini-tailscale-ip>` | **没有 jason 允许，禁止使用**（子代理也不行）。给它发消息不算用 |

---

## 2. 硬规则（违反过，jason 都发过火）

### 2.1 PR 和合并
1. **评审过的 PR 不能再推 commit。** 只要收到任何一条 review（APPROVE 或 REQUEST_CHANGES），推新 commit 就会让评审作废（dismiss），而且不可逆，revert 或 force-push 都救不回来。
   - 例外：REQUEST_CHANGES 本来就要修，推修复是正常流程。
   - 已经 APPROVE 之后，所有 follow-up（包括 Low 级）都**另开新 PR**。
   - 派去修 PR 的代理，prompt 里必须写：「推送前跑 `gh pr view N --json reviews`，如果最新的是一条晚于你开工时间的 APPROVED，**不要推**，停下来汇报。」
2. **拿到 APPROVE 就直接合**（`gh pr merge N --merge`），不再问 jason。
   - 唯一例外：PR 改的是 `docs/laws/` 法律文本，或者评审明说「须 jason 确认」。这种先问 jason（#791 就是这样处理的）。
3. **合并约束**：
   - 仓库开了 `delete_branch_on_merge`：父 PR 一合，子 PR 的 base 分支就被删，子 PR 会被自动 close，而且**不能 reopen**。所以合父 PR 之前，先把子 PR 的 base 改走：`gh api -X PATCH repos/iDoris-ai/Agent24/pulls/N -f base=ab/kernel`。别用 `gh pr edit --base`，它不稳定。
   - 改 base 不会触发 CI，要再推一次才跑；**改 base 也会让已有的 approve 失效**。
   - 叠在别的 PR 上的子 PR 一律先开成 **draft**，等父 PR 合并、子 PR 同步之后再 `gh pr ready`。
4. **提交作者必须是 `jhfnetboy`。** Claude/Codex 只能出现在 `Co-Authored-By:` 尾行，否则 CLA 检查失败。子代理不准用 `--author` 或 `-c user.*`。
5. **开 PR 前先跑 pre-pr-check**：`bash /Users/jason/Dev/tools/PR-Daemon/scripts/pre-pr-check.sh --base origin/<base>`，然后在 PR body 里按编号逐类回应每个命中项（SZ-1、B1、C2……）。
6. **新开 PR 或推了修复之后，通知 PR-Daemon**，最多 3 行：`iDoris-ai/Agent24#N @<40位sha> — 新开 PR / 已推修复，请评审`。
   - 发不出去不用重试，mini 每小时会兜底扫一遍。
   - 判断有没有新评审，看 review 的 `submittedAt` 时间戳，**不要看 `reviewDecision`**。
7. **其他仓库 PR 的通知**（比如 Hyphae 的）静默忽略，不处理，也不汇报给 jason。

### 2.2 分支工作流（AGENTS.md「里程碑方向分支工作流」）
- 每个方向一条集成分支 `ab/<方向>`；task 分支命名为 `ab/<方向>-NN-<短名>`，PR 的 base 是集成分支。
- 攒到约 10 个 task，就开 release PR，把 `ab/<方向>` 合进 main。release PR 需要 APPROVE + 验收。
- 集成分支**每周至少吸收一次 main**，用 merge，不准 rebase。`ab/kernel` 现在落后 main 50 个提交，该吸收了。
- 安全、权限、数据写入、迁移、对外协议类的 PR **必须等 APPROVE 才能合**。普通 task PR 在 CI 绿、本地门禁过了之后可以先合。K1 的切片全部算高风险。

### 2.3 省额度分工（jason 10-07/10-08 定）
- **写代码交给本机 Codex**，用 `gpt-6-luna`；评审可以用 Codex sol。Claude 只负责设计、拆任务、验收和汇报。
- **并发上限**：Codex 最多同时 2 个任务，remote Claude 最多同时 3 个任务。
- 代理一个 task 一个，开完 PR **就退出**，不在会话里等 CI。等待一律用后台 shell 循环。
- 评审默认 1 轮；只有真 bug 才修，其余记成 followup。设计文档最多评审 1 轮就冻结。不为了行数硬拆 PR。
- **能自己验收的就自己验收**：CLI/API 类验收自己跑完，把结果贴进 PR；只把 UI 观感、真实体验之类需要人判断的部分留给 jason。

### 2.4 派 Codex 的方法（实测可用）
`codex:codex-rescue` 插件的沙箱写不了 worktree 的 `.git`，所以改用下面的做法：
```bash
cd /Users/jason/Dev/auraai/Agent24 && git fetch origin
git worktree add ../Agent24-wt-kernel-NN -b ab/kernel-NN-<短名> origin/ab/kernel
nohup codex exec -m gpt-6-luna -s danger-full-access -C ../Agent24-wt-kernel-NN \
  -o <result.md> "<prompt>" > <log> 2>&1 &
```
prompt 末尾附上「通用规则」：
- 只在当前 worktree 里工作；
- 先读 AGENTS.md；
- 作者保持 jhfnetboy，提交信息末尾加 `Co-Authored-By: Codex`；
- 测试先行，并且有反面对照（先证明测试在修复前会失败）；
- Rust 门禁：fmt、`clippy --all-targets -D warnings`、相关 crate 的测试；
- 共享 `CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`；
- 跑 pre-pr-check；`gh pr create --base ab/kernel`；高风险 PR 的 body 里写明「须 PR-Daemon APPROVE 后合入」；
- 最后一条消息只输出：PR 号、SHA、门禁结果、偏离规格之处；
- **禁止 ssh Mac mini**。

原来的模板放在 `/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/aa9e96c4-b22e-4fbe-b366-19c5b6b6e40d/scratchpad/codex/common.txt`。这个目录在 /tmp 下，可能已经被清掉。

**本机已知的环境性测试失败**：代理不要反复重跑，在 PR 里注明「以 CI 为准」。10-08 曾有三个代理各自为此空转 30–80 分钟。
- os-proto supervisor `a_module_that_never_handshakes…`
- agent24-comm `comm_zero_run` / `router` / `import` 超时
- agent24d `scheduler_token_bucket_survives_a_restart…`

---

## 3. 主线：K1 内核（`ab/kernel`）

**规格来源**（都在 `ab/kernel` 上）：
- `docs/agent/PLAN-KERNEL-K1.md`
- `docs/adr/ADR-K1-01..04`（10-08 已 Accepted）
- 切片规格：K1-5 看 ADR-K1-01 §5 反例清单；6a 看 ADR-K1-03 末节；6b 看 ADR-K1-02 §6；7.x 看 ADR-K1-04 §7。
- jason 的五问拍板见 #735 的评论。

### 3.1 已合入 `ab/kernel`
5.1、5.2、5.3a/b/c（#771 #773 #778 #781 #790）· 6a.1–6a.5（#769 #777 #779 #795 #791）· 6b.1、6b.2（#774 #780）· 7.1、7.1b（#770 #782）。

其中 #791（6a.5）修订了 `docs/laws/APPROVAL.md` 的 L-APPR-5：第一方模块写入私有存储的可逆草稿，在启用时授权一次之后，不再逐次审批。jason 10-08 已确认。

### 3.2 进行中（按顺序处理）

| PR | 切片 | 状态 | 下一步 |
|---|---|---|---|
| **#792** | 7.2 审计接线 | ready，head `cacd1ea`，已请 PR-Daemon 评审 | 拿到 APPROVE 就合。注意：7.2 必须已经修掉 #770 的 3 条（凭据类文本过滤、180 天保留常量的防漂移机制、ts 字段格式约束）。评审时核一下 |
| **#783** | 6b.3 统一出站判定 | CHANGES_REQUESTED，又和 `ab/kernel` 冲突 | **不是纯文本冲突，是语义冲突**，详见 #783 的最新评论。①`ModuleToolAdvertView` 在 #790 里改成了 async，`is_authorized` 也改名为 `has_current_consent`，所以本 PR 的 gate 6 `outbound_gate_ready` 要跟着改成 async。②`ModuleTool` 用的是 `requires_outbound_policy()=true` 的默认值，而 agent24d 的 `AgentModuleToolAdvertView` 没有实现 `outbound_gate_ready`（默认返回 false）。照原样合并，#790 的模块工具会全部不被通告、不能调用，agent24d 的 `module_tool_runtime` 有 4 条测试会失败。**正确做法**：把 5.3c 的模块工具调用路径接到宿主出口门上（调用前带上来源、用途、目标调 `authorize_egress`），然后让 agent24d 的 view 返回 true。不能简单把默认值改成放行。这一片派 Codex 做，worktree 是 `../Agent24-wt-kernel-19` |
| **#793** | 6b.4 Exec 与记忆后台任务的出口门 | draft，叠在 #783 上，CI 红 | 等 #783 合并：先把 base 改成 `ab/kernel`，再合并 `ab/kernel`。转 ready 之前必须修好 workspace clippy（聚合构建时 egress 公共导出解析失败）。PR 里要写明两项延后：shell_exec 在敏感资料下 fail-closed（没有 OS 网络沙箱）；模块后台投递、缓存/重试消费者还没接出口门 |

### 3.3 还没开工
- **K1-7.3**（审计查看/导出，按 ADR-K1-04 §7）
- **K1-6b.5**（6b 收尾），加上 **#708 E2E**：David 用 DOC-1 / Documenting 场景联调验收
- **合并 main 进 `ab/kernel`**：这一步要尽快做，现在已经落后 50 个提交
- **release PR `ab/kernel` → main**：需要 APPROVE + 验收（自测项自己跑）

### 3.4 K1 里已知要顺手处理的事
- #777 的三条 Low：`is_allowed()` 的命名/文档（已在 #790 改成 `has_consent`，核一下是否完整）；测试断言改成字面值；文档写明 risk 必须由宿主计算，不能用模块自报的值。
- #791 遗留：草稿例外没有单独的撤销开关（目前只能撤销整个工具的授权），作为 followup。
- 既有问题，需要单独开 issue：内置工具的 `approve_for_session` 对 WriteLocal/Exec 也生效，和 L-APPR-5 的文字不一致。
- #783 部署过渡期的窄边界：部署前已经 PARKED、部署后才 resume 的老 run，没有来源标记，shell/http 会被拒绝（fail-closed）。在发布说明里提一句。

---

## 4. K1 之后：决策服务（`ab/decide`）

- 计划在 `docs/agent/PLAN-DECIDE.md`；调研台账在 `docs/research/DECISION-MODELS.md`；Jev 资料在 `docs/research/jev-study/`。
- 已合入：D0 系列、D1 规划（#754）、D1-1 ONNX spike（#760）。`ab/decide` 落后 main 25 个提交。
- **jason 的顺序**：先完成 K1 的开发和发布，**再**开始决策服务。开始前先评估 **LiquidAI d1**（博客：blog.mushroom.cv/blog/liquidai-d1-3b-omni-600m-decision-model-zero-output-tokens-edge/）：
  - 用 `eval/decide` 的中英泰三语集在笔记本上跑，和方案 B（e5 / Qwen3-Embedding + 分类头）对比；
  - 许可证是 lfm1.0，限研究和非商业用途，**集成前必须核清条款**；
  - 600M 版本可能可以做语音路由，和 AgentEar 有关。
- 已定的约束：
  - 中、英、泰三语必须支持；
  - 8/16/24/32/64+ 五档，用同一系列模型（方案 B），并且能切换到方案 A（全 Qwen3-Embedding）；
  - 每个候选模型都要有许可证结论；
  - 决策日志保留 180 天；
  - 写记忆偏向精确：规则兜底 + 模型高置信才写入。

---

## 5. 并行与暂停的线

| 线 | 分支 | 状态 |
|---|---|---|
| COMM（Hyphae 通信） | `ab/comm` | COMM-6b 计划见 #741；6b.1（#768）和 6b.2（#776）已合。其余部分里，**涉及 profile 发布的要等 Hyphae #132 合并发版 + DEP-C8 升级 lock**；本地配置类可以先做。COMM7 等 Hyphae #27 |
| Documenting | `ab/documenting` | David 的场景线。#784 #796 #798 已开、#799 是 draft，都在等评审。按常规处理：APPROVE 就合 |
| M4（Cos72 Workspace 底座） | `ab/m4`（还没建） | 排在决策服务之后；记忆 P1（`ab/memory-p1`）和它并行 |
| DMG | #671（draft，base 是 main） | macOS arm64/x64 构建失败，待诊断。jason 要求即使没有签名也正常发布 DMG |
| Open Design OD-M11/M12 | — | **暂停**，不做 |
| 移动端 | — | 暂不做 |

记忆 P1 的待办：「原文永不删 vs 清除权」ADR；日志膨胀四项（占用可见、大输出外置、冷归档、purge），见 issue #721 和 `docs/research/MEMORY-STRATEGY.md`。

---

## 6. 汇报和提问的方式

- jason 的原话：「完成前不许停止工作，只汇报关键进展。」能自己定的就自己定。
- **真正需要 jason 拍板的**：法律文本（`docs/laws/`）、产品取舍、许可证或商业条款、动用 Mac mini、release 的主观验收。问的时候给出选项和推荐项。
- jason 拍板或验收的结论要**当场落文档**（tasks.md、相应的 PR/issue 评论）。对话一被清空，这些结论就丢了，之后再问会被骂。
- 权威状态来源：`docs/agent/tasks.md` + 各 PLAN 文档 + GitHub 实况。手写的进展摘要（包括这份文档）都可能过期。

---

## 7. 现查命令（开工第一步）

```bash
cd /Users/jason/Dev/auraai/Agent24 && git fetch origin
gh pr list --state open --json number,title,baseRefName,isDraft,reviewDecision \
  -q '.[]|"#\(.number) [\(.baseRefName)] draft=\(.isDraft) \(.reviewDecision) \(.title)"'
for n in 792 783 793; do gh pr view $n --json headRefOid,mergeable,reviews \
  -q "\"#$n \(.headRefOid[:8]) \(.mergeable) \([.reviews[]|.state+\"@\"+.submittedAt]|join(\" \"))\""; done
gh pr list --base ab/kernel --state merged --limit 30 --json number,title -q '.[]|"#\(.number) \(.title)"'
for b in kernel comm decide documenting; do echo "ab/$b 领先main $(git rev-list --count origin/main..origin/ab/$b) 落后 $(git rev-list --count origin/ab/$b..origin/main)"; done
git worktree list | grep kernel
pgrep -fl "codex exec"   # 确认没有遗留的 Codex 任务
```

现存的 K1 worktree：`../Agent24-wt-kernel-19`（#783）、`-22`（#793）、`-23`（#792）、`-20` / `-21` / `-24`（对应 PR 已合，可以清理）。清理规则：只删 tip 是 main 或 `ab/kernel` 祖先的分支；有独有提交的先用 bundle 存档。

## 8. Travel Drain 检查点（2026-10-08 本机 18:49）

本节是动态状态，**覆盖上文 3.2/3.3 的旧 PR 状态**；详细恢复记录见 `~/.pi/agent/travel-checkpoints/idoris-agent24.md`。

- 用户说「走了」后进入 DRAINING；Orca Run `run_8be06dd8425d` 的 4 个 dispatch 均已 settled/released，本机 `active=0`、`reclaimable=0`，cookit doctor 也报 `reclaimable=0`。没有新开任务、推送、合并或清理不确定 worktree。旧 Claude 锁 PID 无对应活进程。
- #783：远端仍是 `c973715`（clestons CHANGES_REQUESTED，CI 绿但冲突）；本地 `Agent24-wt-kernel-19` 已到 `95b4d5d`，干净但比远端 PR 分支 ahead 14，未推送。实现者报告 K1-6b.3 门禁与 603 个相关测试通过。必须本地复核并独立验收后再考虑推送。
- #792：远端 `cacd1ea`（clestons CHANGES_REQUESTED，旧 head CI 绿）；`Agent24-wt-kernel-23` 保留 4 个文件脏改。Sol 6.1 原始报告是 Orca `msg_3fe01ebca433`；Luna 修了两项中的主要路径，定向测试与 fmt/clippy 通过，但合法尾行删除检测仍未完成；workspace 全量测试存在 Hyphae/subprocess 超时，未得到全绿。不可合并。
- #793：draft、head `042451f`、base `ab/kernel-19-egress-gate`，CI 红且冲突；等 #783。
- GitHub 状态核验时三 PR 均未获 APPROVE、未合并。当前 clestons review 已到 CHANGES_REQUESTED 安全边界。用户确认 Travel Drain 只需暂停本机工作，不需要暂停 PR-Daemon；未访问 Mac mini，远端 PR-Daemon 可按原计划独立扫描，这不阻塞本机排空。
- 工作树均保留：#783 有未推本地提交，#792 有脏改，#793 是依赖草稿；根 checkout 原有 `docs/agent/HANDOFF-ORCA-2026-10-08.md` 与 `docs/demo/2026-09-30-v050-desktop-audit/` 未跟踪文件也保留。
- 恢复顺序：先核查 quota/PR/head/CI；先 review #783 的 `95b4d5d` 并安排 Sol/Astra 独立验收；再补 #792 的尾行删除审计回归并验证 fail-closed；#783 settle 后再处理 #793；之后 K1 继续 7.3、6b.5、#708 E2E。

## 9. 持续推进波次（2026-10-08 20:34 +07；覆盖 §8 的旧状态）

- **Run** `run_7c648f145993`，笔记本 Codex 并发上限 2；COMM-5b 本轮已失败收口并 release，当前活跃 1/2（仅 #792 修复 worker）。
  - `task_ab9947021b03` / `ctx_3e2c1b407a28`（gpt-6.1-sol）：继续 #792 的既有脏 worktree `/Users/jason/Dev/auraai/Agent24-wt-kernel-23`，只修 Mini Astra 独立验收指出的 1 P1 + 2 P2；测试与定向门禁后汇报，明确禁止 commit/push。#792 远端头仍 `cacd1ea`、OPEN/CHANGES_REQUESTED；旧头 GitHub CI 全绿，但新补丁不得视为已验收。
  - `task_f008d08d1f4b` / `ctx_245b71b4416b`（gpt-6.1-sol）：COMM-5b 实现保留在 `/Users/jason/orca/workspaces/Agent24/ab-comm-05b-zero-run-guard`，HEAD 仍为基线 `11a639c`，**工作树脏、未 commit/push、无 PR**。worker 失败报告 `docs/comm/COMM-5b-failure.md`：workspace 两轮分别 17/18 个非白名单超时，串行复跑 13 个失败；独立两行 `/bin/sh` 脚本也有 8 秒超时，T3 未全绿。结构门禁和部分 T2/反面对照已有通过证据；生产 Rust 改动 0 行。不得把该门禁结果算作通过，也不要在主机执行超时未恢复时重跑。旧参考工作树 `agent24-comm5b-20261005` 的 `2aafcfd`（150 commits 落后、无 PR）仍只作参考且保持原样。
  - 依赖任务 `task_c4f8f8bd2858`（本机 gpt-6-luna 独立复核）因上游实现 Task 失败仍 pending；只有在环境恢复、以 `--retry-of` 重派 COMM-5b 且最终门禁通过/PR exact head 可核对后才可启动。`luna` 角色已加入 `~/bin/orca-dispatch`，显式映射本机 Codex 池并受同一 90% 闸；语法检查与 dry-run 通过，最近确认本机 Codex 用量 39%。
- Mini 独立工作已收口且临时远端 review worktrees 已由 Orca 删除（未在 Mini 构建；数据卷仍 62 GiB 空闲）：
  - Mini Astra 对 #792 精确脏补丁发现：P1 `response_too_large` 审计判为未知但对调用方仍可 `retryable=true`；P2 终态审计失败返回裸字符串、不含 `result_unknown/retryable=false`；P2 “有效替换行”测试按 typed struct 序列化算 hash，与真实先转 `serde_json::Value` 后落库字节顺序不同，未证明 equality guard。三项均已派给当前 #792 修复任务，必须二次独立复核后才可考虑推送。
  - Mini Luna 对 #793 CI 只读定位：两平台各 590 pass / 1 fail，失败测试仍期待恢复后的 `shell_exec` 执行 `resume-ok`，但 #793 的新策略在 spawn 前 fail-closed；agent24d 合约测试仍期待无 host egress policy 时广告 `http_fetch`。这是旧语义/契约与 #783 下游策略不一致，保持 #793 draft、冲突、勿 rebase/retarget/push，待 #783 修稳后再更新测试与合约。
- 本机 Sol 对 #783 未推送精确提交 `95b4d5d` 的只读验收仍为 REQUEST_CHANGES：当前可达的 High 是 `UserInput` 的 `LocalOnly` 来源在 `agent24-agent/src/lib.rs:631` 被丢弃，空来源工具路径可放行并外发；另一 High 是获准云模型的 reqwest 默认重定向可能把 prompt 经 307/308 重放到未授权目的地。另有 DNS/审批等待期间撤权竞态与资源快照过期问题。#783 远端仍 head `c973715`、CHANGES_REQUESTED/CONFLICTING；本地 `95b4d5d` 工作树保持干净、未推送；不要因 #792 settle 就直接合入，之后要补负例、重新集成、跑门禁及 CI。
- **依赖 DAG**：`#792 修复 → Astra 再审 → 本地全量门禁/推送 → PR-Daemon 对 exact head 评审`；`#783 先修 LocalOnly/redirect/撤权边界并与当前 ab/kernel 集成 → #793 更新旧断言/合约并重跑 CI`；再继续 K1-7.3、6b.5、#708 E2E 与 `ab/kernel → main` release。COMM-5b 独立并行；COMM-6b.1/.2 已合并，6b.3/.4 等 Hyphae release + DEP-C8，COMM-7 仍独立待排。
- **额度**：本机 Codex 最近核对 39%（当前活跃 1/2）；Mini Codex 14%；AGY Gemini 5h 83.8%（超过 80% 硬闸，预计本地 21:47 重置）/ 周 36.9%；AGY 3P 周池 100%。不绕过硬闸；重置后优先将 AGY 派作 #792 新 exact diff 的交叉审查（若补丁已就绪），否则不制造 filler。`#792/#793` 的 Mini 审查终端已退出并 release；COMM-5b worker 已失败报告并 release，脏 worktree 保留；Run delivery 已处理/ack；当前活跃 `1`、reclaimable `0`。

## 11. LoopX / Pi / Orca pilot（2026-10-09 09:20 +07；覆盖前文 #792 活跃 worker 快照）

- 用户授予 Agent24 范围长期 LoopX 授权：可自主创建/续用 Goal、维护 Todo/检查点、绑定当前 Pi agent、执行 quota/continuation；可在无可执行工作时暂停 LoopX 调度器，并在阻塞解除后恢复，不逐步询问。权限边界已记录到 `~/.pi/agent/AGENTS.md`；不包含解除任务/Orca/PR 审批门、提交/推送/合并/发布或破坏性操作。
- Pi LoopX extension 原来未将 `--agent-id`、`--goal-id`、稳定 `--thread-id` 传给 `start-goal`，且项目与用户两份 extension 重复加载。已在 LoopX 1.3.1 Pi extension/template 加上显式身份参数与当前 session binding 复用，移除仅带 LoopX managed marker 的项目副本；`loopx slash-commands --inspect --surface pi` 现为 user-global `current`、无 duplicate warning。Pi `/reload`、`/loopx` 实际启动、`loopx resolve-agent-thread` readback 均确认绑定成功：Goal `agent24-goal` / Agent `codex-agent24-k1-792-pi-pilot` / 当前 Pi thread，Pi binding 中 activation token 与 task body 均已写入（不记录 token 值）。包内 pytest 因该 Python 环境未安装 pytest 未运行；以 Pi 实际激活与 LoopX readback 作验收。LoopX 升级时需保留/移植此本地 extension patch。
- LoopX 已写入一条 P0 Agent Todo：在摸 #792 脏补丁前先解决旧 Dispatch liveness。Orca 权威状态：Task `task_834051bb794d` / Dispatch `ctx_4149d420a50e` 为 failed/abandoned，但 liveness `unverifiable: stale_status`、资源 retained，终端仍报告 running；恢复契约明确禁止在 `unverifiable` 时 stop、release 或 retry。未派新 worker、未触碰补丁。原 issue-fix capability route 也因 PR #792 无 linked/eligible GitHub issue 被拒；下次应按 K1 权威计划使用通用工程任务路线，不得伪造 issue 或绕 admission。
- 因当前没有安全可执行的下一步，已按用户长期授权暂停 LoopX **Goal scheduler**（Goal lifecycle state `stopped`，readback verified）；不是解锁或修改任务审批状态。session binding 保留，阻塞解除后可不经逐步授权恢复。
- #792 worktree 仍 HEAD `cacd1ea275898e6a4be7ce01090e2a0ce66dfc18`，五个原有 Rust 文件未改，脏补丁 SHA-256 仍 `fd2418fa7195b434ef83a792cdac3bcd0d693e1690bc37e630c8d3e0dc8b59fb`；无 workspace tests、提交或推送。下一步：先取得 Orca 对旧 Dispatch 的正向 exited/settled 证据；再用相同 Task 的 `--retry-of ctx_4149d420a50e` 建立新鲜 attempt，补一次/并发复用回归测试，定向验证，交 Astra 精确补丁只读审查。

## 10. #792 修复后复审（2026-10-08 21:11 +07；覆盖 §9 对 #792 的旧状态）

- `task_ab9947021b03` / `ctx_3e2c1b407a28` 已 succeeded 并 release；#792 原 worktree 仍保留脏改，HEAD `cacd1ea` 不变，无 commit/push。Mini Astra 上轮指出的 1 P1 + 2 P2 均已修复并有红绿证据：response_too_large 统一为 `result_unknown=true/retryable=false`；终态审计失败返回 typed unknown JSON；真实落库序列化字节的有效替换行反面对照覆盖 pre-dispatch 与 interrupted terminal。
- 已通过：fmt、diff check、workspace clippy、store/tools（450+1 doc；38 pass/1 ignored）及 agent24d `module_tool_runtime::tests`（20 pass）。daemon focused 首次共享 artifact 编译异常，按规则仅以 `CARGO_INCREMENTAL=0` 重跑一次后通过。证据 `/tmp/kernel-23-review-fix-report.md`；最终完整补丁 SHA-256 `6109f0cb7e66af94e4e537dd20419c93e5d5005253db3674523f5cd26d0748f7`。**尚未跑 `cargo test --workspace`，因此不能提交/推送。**
- 第二轮 Mini Astra 只读复核 `task_913d2a797ef1` / `ctx_bb2cef4ff4a6` 已完成并 release，未构建/改动。准确确认相同 base 与补丁 SHA（`6109f0cb…d0748f7`），发现 **2 个 P2 验收阻断**：`agent24-tools/src/lib.rs:644` 的 `Pending` 持久化 `ResultUnknown` 却向 caller 返回 `InvalidResult`；`:730–734` 以 `Denied` 豁免未知降级，但 invoke 后模块自报 `PermissionDenied`（含 cancellation settle）在 terminal 写失败时仍可能返回裸 `module_tool_audit_unavailable`。报告已随 Orca delivery ack；临时 Mini worktree 已清理，无 Mini 构建。
- 本机 Luna 修复任务 `task_29d2b46ffeda` / `ctx_ef2f62108f47` 活跃（Codex 池 42%）：已以 `cargo test -p agent24d module_tool_runtime` 对两个问题做红证据并开始绿验证；修复保留真正 pre-dispatch host Denied，覆盖 Pending persisted/caller 一致性，以及 post-invoke `PermissionDenied` terminal audit failure 与 cancellation settle 的 typed unknown/no-redispatch。仅在现有脏 worktree 修改，不提交/推送。
- 已排好依赖 DAG：修复成功后启动 `task_7946201a99bc` Mini Astra 最终精确 diff 只读复审；其无 blocker 后启动 `task_38efd6152c36`，在笔记本无并行构建时跑一次 `cargo test --workspace`，白名单外失败阻断。PR-Daemon 对精确 head review 和 CI 全绿后才推送。当前原 patch SHA 仅对应 Luna 修改前版本，最终 SHA 需重算。COMM-5b 的最新本地验证见下方 §11。另：

## 11. Travel Drain Checkpoint（2026-10-09 17:00 +07；覆盖前文旧状态）

- Jason 说“我要回家”后进入 DRAINING：冻结新派工，不启动新 Run/Task/worktree/review，不推进 M2。当前 Run `run_7c648f145993` 的在途 worker 已全部 settled 并 release；无 Cargo 测试/构建进程。没有 commit/push/merge/tag/release，也未暂停 PR-Daemon。
- 旧 Dispatch `ctx_4149d420a50e` / `task_834051bb794d` 仍 `abandoned`、terminal retained、liveness `unverifiable: stale_status`。`worker-list --terminal-state reclaimable` 为 0；但不要 stop/release/retry。未执行 `orca-close`：脚本会按该行 `nextAction=release` 触碰 unverifiable terminal。不能宣称整个 Run/机器完全清空；待正向 execution-host 证据后再处理。
- #783 最新本地修复：`ctx_5d97150c1629` succeeded/released，`Agent24-wt-kernel-19` HEAD `95b4d5d`，5 文件脏改（241+/73-），`diff --check` clean。worker 报 tools 14/14、agent 108/108、recall 2/2、fmt clean；整仓 gate 与独立审查尚未做。远端 PR #783 仍 `c9737156…`、CHANGES_REQUESTED/DIRTY；未推送。
- COMM-5b：`ctx_388be64e6371` succeeded/released。Worktree `/Users/jason/orca/workspaces/Agent24/ab-comm-05b-zero-run-guard` 基于 `11a639c`，8 文件脏改，`diff --check` clean；报告 `docs/comm/COMM-5b-zero-run.md`。worker 最终自报 2,788 passed/0 failed，T2/T3 通过，但其过程出现 RPC、`daemon_supervise`、token-bucket 跨秒失败和多次整仓重跑；独立核验必须先看准确日志。`domain.rs` 只有测试区改动，需 review。曾创建本地 commit `01682bdd` 随后 reset 回 `11a639c`；未 push，reflog 保留。无 PR。
- #792：远端仍 `cacd1ea…`、CHANGES_REQUESTED/BLOCKED；本地 #792 worktree 5 个脏 Rust 文件（2,251 changed lines，含测试），无 push。#793 仍 draft `042451f…`、base `ab/kernel-19-egress-gate`、DIRTY；先等 #783。
- GitHub 状态快照：#671 draft/macOS x64 与 arm64 构建失败、review required；#829（Documenting，非本任务）一个 macOS CI job 在跑。最新 release `v0.5.1`；没有获批全绿候选、Apple signing/notarization 不可用。
- 本地 PR-Daemon `resident.sh status` 超时，review_queue Python 查询也超时；当时未见本地 process/launchctl 项。服务/队列/head 状态记为 unverified，未对服务作任何操作；云端 PR-Daemon/CI 继续独立运行。
- Doctor（2026-10-09 16:59）：Orca ready，reclaimable=0；本机 Codex 0.161.0 落后 0.162.0、阻止后续 Orca dispatch；AGY Gemini 5h 72.3%/周 58.7%，AGY 3P 周池耗尽；Claude 7d 62% over-pace；Mini Codex 周用 29%；DeepSeek ¥3.42/¥9；本机与 Mini 各 2 张 reset cards（10-30、11-07 到期）。Jason 2026-10-09 明确：本机与 Mini Codex 可积极用于有价值任务，不应仅为保留额度而推迟；需要时使用 reset cards。恢复时重查额度并尊重每周期 95% 派发闸门，不得强行绕过或制造 filler，也要确认真实服务状态。
- 主 checkout detached `c503bf1`；`origin/main=984d0f7`、`origin/ab/kernel=933d038`。主 checkout 原有两个 untracked 路径均保留。M1 ledger/PR #720/#721 已合并，但 `/tmp/agent24-M1-ACCEPTANCE-main.md` 仍未勾验，不能认定完整真机验收。
- 恢复顺序：先核查旧 dispatch 正向状态、GitHub/Orca/PR-Daemon/额度；review #783 精确脏 diff并补整仓 gate；核验 COMM-5b 完整测试日志、`domain.rs` test-only 改动及 pre-pr-check；继续 #792 hardening；#783 settle 后才更新 #793。所有工作树脏改均保留，不得提交/推送/合并/发布，直到各自门禁及外部 review 满足。

个人恢复 checkpoint：`~/.pi/agent/travel-checkpoints/idoris-agent24.md`。LoopX `ACTIVE_GOAL_STATE.md` 保持原有 blocked 状态，未覆盖或解锁。
