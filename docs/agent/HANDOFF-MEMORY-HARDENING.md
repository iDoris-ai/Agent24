# 记忆正确性/隐私修复线：回家暂停检查点

> 2026-10-12 12:02 UTC+7 保存回家暂停检查点；用户随后明确「你继续工作」，现已 **RESUMED**。下文暂停证据保留，当前动作见末尾恢复记录。

## 当前授权与主线事实

- 此前用户把审查发现的真实正确性/隐私 bug 排为第一优先级，授权规划、复现与分配 subagent；**没有授权整个 Hindsight 增强项目**。本次暂停不改变该范围，恢复后仍是第一任务。
- main `ab125de262827bd0f1c6680b427d9c241ccff785`。GitHub 已重新核对 M1 release #720 于 2026-10-07 合入，M2 M-E release #767 于 2026-10-08 合入，merge `73d87f0b75c80bd9b14fa98471fe6a704bbd3cc8` 是 main 的祖先。因此不是「M1 完成准备开始 M2」；两者均已合入。主线 M3 等第二个真实用户，不自动开工组织化；旧 roadmap 状态文字有历史滞后。
- 本地新 bugfix 集成分支 `ab/m1-memory-hardening` 从上述 main 建立，未推送；不是新的全局 enhance 里程碑。未来任务 PR base 为对应集成分支，main release 人开人审。
- D0/D1、David 的 K1/KB 和暂停 Workspace 不变；不接管别人 worker/工作区。

## Run 与子任务（已排空）

Run `run_d6484275c63a`，三个 worker 均收到暂停，保存检查点后结束；所有本线 agent 终端及三个确认仅空闲 shell 的附加终端已关闭，reclaimable=0。

1. **Sol 6.1 规划**：`ctx_70db94abe3d8`。已保存静态证据与未完成计划，未跑测试，正式计划/TRIAGE 尚未完成。Codex IPC 受限，未送 worker_done；父协调者据 final 明确 abandoned，不伪造 succeeded。干净规划 worktree 已安全删除。
2. **AGY 重复撤回**：`ctx_5b40e3ebee09`，task `task_fe509db70010`。已有 authentic RED：`cargo test -p agent24-memory --test retract_cycle`，退出 101，第二次撤回触发 `UNIQUE constraint failed: mem_events.id`，事务回滚使撤回失败。仅新增复现测试，生产改动 0；未做修复/GREEN/全工作区 gates。有效 worker_done outcome failed/PAUSED_BY_USER 已验证、ack 和 release。
3. **Luna 暂停恢复回归准备**：`ctx_8d07b1228810`。仅测试改动已保留，cargo 未运行，不能称 bug 已确认或隐私已修复。Codex IPC 受限，父协调者据 final 明确 abandoned 后关闭终端。

## 重要契约发现：不要盲修暂停恢复

Sol 指出**当前 OpenAPI 明确豁免已冻结输入快照的审批恢复**，因此不能直接把“pause 后恢复仍包含原快照 recall”当现有违约。恢复后先核对原文/代码，做行为探针并区分：
- 现有承诺的真 bug（可最小修复）；
- 既有豁免下的隐私设计取舍（需独立明确变更契约）；
- 新派生页/模型抽取未来风险（不属于本次 bugfix）。
原始聊天保留是 M1 no-loss 语义，不能为“通过暂停测试”删除历史或重写快照。不得以研究设计审查可接受替代正式产品契约批准。

## 保留资源与备份

两份工作区有未提交测试，不能自动删除：
- `/Users/jason/orca/workspaces/Agent24/ab-m1-memory-hardening-02-retract-cycle`，分支 `ab/m1-memory-hardening-02-retract-cycle`；新增 `rust/crates/agent24-memory/tests/retract_cycle.rs`。
- `/Users/jason/orca/workspaces/Agent24/ab-m1-memory-hardening-03-pause-regression`，分支 `ab/m1-memory-hardening-03-pause-regression`；修改 `rust/crates/agent24-agent/src/session_memory_tests.rs`。

原始回执、日志、worker checkpoints：`/tmp/a24-memory-hardening/`。
耐重启备份：`~/.pi/agent/travel-checkpoints/iDoris-ai-Agent24-memory-hardening-20261012/`，包含三个 checkpoint、重复撤回 RESULT/RED/patch 和暂停测试完整文件/patch。

没有 commit/push/PR。根目录原有无关未跟踪文件未改。既有其他线受保护工作区未手动删除。

## 恢复步骤（只在用户恢复后）

1. 核对 main、集成分支、两份脏 worktree、Orca Task/Dispatch、真实额度；不得重复创建相同工作或覆盖未提交文件。
2. 读 Sol checkpoint 与冻结 M1/OpenAPI 契约，完成最小修复计划；对已豁免行为不直接改政策。
3. 先修已红灯证实的重复撤回 audit ID/事务问题，增加三轮、幂等 no-op 和 cross-owner 反面对照；保留同 ID 重记兼容，不引入通用世代/缓存增强。
4. 再运行 Luna 行为探针（应先调整任何违背既有豁免的断言）；cargo gates 串行，共享主仓 target，ulimit 4096，NO_PROXY/no_proxy 含 localhost/127.0.0.1/::1。
5. 实现者≠评审者；小 PR 默认≤300生产行、原版机械检查；所有任务 PR 外部 PR-Daemon 精确 head APPROVE + required CI 全绿 + mergeable 才合。没有被恢复前，不提交推送/触发新评审。

## 共享后台与调度边界

未停止、暂停或限流共享 PR-Daemon/GitHub CI；本机只读进程查询未匹配到 PR-Daemon，但不足以判断外部进程是否运行，状态记为未独立确认，不宣称已停或正常。
旧 #906/#908/#909 等云端审查继续由外部系统处理，恢复时重新核对，不将旧 approval 挪用。Pi 的既有 LoopX D0/D1 goal 未绑定本会话自动续跑，本次不伪造 activation 或修改其他 goal。

暂停时本线已排空：**活动 worker 0，保留终端 0；保留脏测试工作区 2**。不声称整台机器/其他协调者任务均空闲。

## 用户恢复后的核对与启动

- 再次读取 home checkpoint、两个保留工作区、Orca 状态和真实额度；main 仍为上述 SHA，#720/#767 均 MERGED 且 merge commits 为 main 祖先，M1/M2 已按批准范围收口。**当前不是 M3 开发**，M3 第二真实用户门未解除。
- 同 Run、同 Task 以 retry-of 续跑，未覆盖脏测试、未重复创建同一业务任务：Sol `ctx_5b847a91dffd` 完成最小契约/修复计划；AGY `ctx_d42047b2e701` 继续已 red 的重复撤回最小修复。两者均 ready/input_accepted/observed。
- Sol 新规划 worktree `ab-m1-memory-hardening-04-plan-review`；AGY 复用原 02。Luna 原 03 保留但未重启；先按明确 OpenAPI 豁免修正行为探针，再由父协调者授予串行测试 lane。
- Sol 已完成 TRIAGE.json/.md 与新修复计划草稿；IPC 限制下依据 final 明确 abandoned，已关闭其 agent 与已核验空闲 shell。计划工作区因新文档未提交保留，另存 home 备份。
- AGY `ctx_d42047b2e701` 已交付 H01 初稿并正式 succeeded/released；但内部独立审查 `ctx_6836849fc84b` 给 REQUEST_CHANGES：COUNT(prefix) 后缀分配在存在 base/base-2 的合法预占用状态仍可能撞 ID；保存 diff/log/RESULT 未覆盖实际 writer 生产 hunk，真实生产 churn 31 而非报告15。独立审查源码 manifest 与原工作区哈希匹配，未改代码；IPC 限制下 final 后 abandoned，终端及空闲 shell 已关闭，dirty review snapshot保留。
- 父协调者对该精确初稿执行 native 串行完整门禁：fmt=1（writer query 排版），clippy workspace/all-targets=0，test workspace=0，3050通过/0失败/17忽略。测试全绿不能替代占用后缀安全反面对照，未提 PR。
- AGY返工 `ctx_278d8fafbd99` 已正式 succeeded：预占用/间隙ID真实red后，在事务内探测实际未占用ID；writer已恢复基线并把H02提案隔离。最终H01生产churn27，lib244+integration3绿，writer12是lib子集。
- Sol独立R1复审 `ctx_66d9d590c47d` APPROVE_RESEARCH_PATCH：F1/F2解决、manifest匹配、无repo改动；仅内部验收。父协调者对精确R1哈希native串行完整门禁fmt/clippy/workspace全部通过：3050通过/0失败/17忽略。原版机械自证/检查退出0，无findings，27生产行/2文件/单rust目录。
- 已依常规任务流程以 jhfnetboy 作者提交并推送：head `9e74882f0ddc8b45bb0bbd0eb390776a87de72d4`；PR **#910**，base `ab/m1-memory-hardening`，不是main。等待外部PR-Daemon精确head APPROVE/required CI全绿/MERGEABLE；不得在已批准head上再加commit。无main release/部署。
- R1资源release返回 retained/user_takeover，终端 `term_598ef15e-d2e9-4304-a22d-c2d5b67cf320` 由用户接管，不能自动强关；02源码工作区已clean且remote保存，但因该连通终端保留。R1 reviewer final 后IPC限制下abandoned/agent终端已关闭。
- Luna `ctx_6602fbbc47f2` 已交付 READY_FOR_RED_TEST：输入保真探针与按 OpenAPI 对齐的新run/冻结snapshot测试，生产0、Cargo未运行；IPC限制下 final 后 abandoned/关闭终端，03 dirty tests保留，测试执行等待串行lane。
- 之后父协调者native串行运行Luna准备的P01/P02：P01按既有OpenAPI豁免通过1项；P02真实mock provider收到messages=[]，完整marker形状的原始用户prompt被误删，exit101/1失败，证明第二项现有输入保真风险，日志见resume/probe-gates。
- Sol P02/H03分诊 `ctx_07b653e1e4f3` 已交付并收口：真实P02 bug成立，但cold recovery缺少可靠、与snapshot原子提交的host/run/message-instance provenance，结论BLOCKED_NEEDS_CONTRACT；不能用形状/位置/当前active IDs当权威或只修fresh一半。报告P02-MINIMAL-PLAN.json/.md保留，未派生产实现。
- Astra配置调用 `ctx_78351835772f` 已交付交叉报告与P02-PROVENANCE-CONTRACT-DRAFT.md，验证Sol对cold provenance阻塞判断，推荐B（原子保存有界host/run/message-instance来源和明确NoRecall）而非按文本/位置猜测；旧Unknown snapshot恢复策略必须单独决定。该worker因Orca launch model/effort=null将实际模型标UNVERIFIABLE；父调用CLI和TUI显示gpt-6-astra high，仅记录配置事实，不伪称provider-side证明或其指定验收通过。报告未改源码/运行测试，final后明确abandoned/终端关闭。
- **用户已明确同意**：仅对升级前/无可信来源/仍需继续执行的旧run暂停provider/审批恢复、提示显式新开run；原文/history/snapshot全部保留，不自动复制不明记忆或重放旧工具。该legacy行为门已解除，不再重复问用户；private schema与public兼容具体方案仍需正常独立/外部审查。
- Sol `ctx_288f75a84aca` 在新06工作树提交147行单一契约文档 `docs/agent/PLAN-M1-SNAPSHOT-PROVENANCE.md`，未commit/修改生产。结果 `/tmp/a24-memory-hardening/p02-contract/CONTRACT-RESULT.json`。Luna `ctx_761b16221d0e` 独立调用/审批边界矩阵已交付CONSUMERS.json/.md；AGY Gemini真实预算91%被闸门拒绝，3p模型override仍被同池拦截，停止尝试、不force，回退本地Luna。两任务final后honest abandoned/终端关闭，07干净审计工作树按自动收口处理。
- 独立契约审查 `ctx_7b6b844c6359` REQUEST_CHANGES（精确147行doc hash `69518c454da390d5b9617ea304fd408e066ce97c1c9179812076594836419ef1`）：R1保留Run.error原failed-only语义，新增可选只读continuation_error，删除FreshAdmission/GET缺行推断；R2在Unknown保护前用既有created_at/cutoff纯TTL检查，不能让过期审批被无限保留；R3漏掉ACP::run_prompt仅等待WS终态，应既有REST低频reconcile提示hold，不新增WS事件或取消daemon run。报告CONTRACT-REVIEW.json/.md，仅内部审查。reviewer final后明确abandoned/终端关闭，当场收口。
- 当前活动worker1：Sol契约R1新Task `ctx_b37653911950`（不是retry旧成功作者Task），ready/input_accepted/observed，复用06仅修三项明确阻塞，输出CONTRACT-R1-RESULT.json，不覆盖原report；用户Unknown策略不变，生产仍0/无docPR。完成后独立精确SHA复审→文档PR外部批准→便宜worker实现，不能把草案或用户行为批准当public schema已冻结。06未提交doc保留，05脏审查快照按保护判据保留。
- **#910已合并**：clestons对精确head `9e74882f0ddc8b45bb0bbd0eb390776a87de72d4` APPROVED，原head失败macOS job仅重跑一次后6项CI全部成功且MERGEABLE，立即合入ab/m1-memory-hardening；merge `86808d30f683262fcf7bc92d6d666fbede5850f6`，2026-10-13T06:12:45Z。首次失败是不被本PR改动的documents matcher墙钟断言（1.346s），没有追加commit/改documents。未main release/部署。
- 已当场orca-close，reclaimable_left0；02物理工作树检查已不存在，不重复删除/强release过去user_owned记录。所有提交由remote task/integration保存。06在Solfinal后ff同步新的方向分支base86808d3，doc SHA不变；内部审查仍以精确doc hash为准。
- 保留用户接管终端1；不启动完整增强或M3，不覆盖未提交测试。原03/04/05 dirty测试/计划/审查副本按保护判据保留，活动worker只读不会修改H01 PR。
- 顺手核对旧 D1 待审：#908、#909 均获 clestons 对精确当前 head APPROVE、6/6 CI 成功且 MERGEABLE，按用户常设授权即时合并至 ab/decide。merge commits 分别 `4f445c7412b3e258ef6528d5337cc36da1bfe8dd` 与 `97c46766fe84e28d89616bb7d016f6d92adeb36c`。这不是 main release 或 M3 开工，不改变记忆 bug 第一优先级。

