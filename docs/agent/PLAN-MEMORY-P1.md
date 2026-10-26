# C3 P1：沿 Agent24 核心交互完成个人记忆可控

> **DRAFT / 待外审，2026-10-23。** 本轮授权是 P1-01 设计与计划，不是生产开工、迁移、清除用户数据或发布批准。固定基线 `f16e65a95f9732f4e00cb288f967e40e0978eb7b`；该 SHA 是设计源码快照，入库任务已吸收 `ab/memory-p1@c19771dc`。本文是待审规划，不自称已批准生产任务源；入库即使获准也不解除下面的高影响设计决策门。

## 1. 输入、权威与交付边界

- 权威约束：[MEMORY-STRATEGY](../research/MEMORY-STRATEGY.md) 的 C3 P1/F7/F11 方向、[OpenAPI](../../protocol/openapi.yaml)、[MEMORY](../laws/MEMORY.md)/[APPROVAL](../laws/APPROVAL.md) 法律与当前实现；草案与现状冲突不覆盖现行契约，需在相应 PR 说明变更。
- 输入为用户提供的 10/22 模块台账、记忆余项核销和 P1 建议稿；旧基线 `2a67f891` 的“待红证”等状态按下表更新，不当作当前批准。
- 持久证据：[模块核销](MODULE-PROGRESS-EVIDENCE-20261023.md) 已由 #1146 入库；探针报告见 [PR #1148](https://github.com/iDoris-ai/Agent24/pull/1148)，含命令/exit/源指纹与各轮覆盖边界，合并状态以 PR 为准；Superfluid 研究由 #1147 入库。独立复核只接受指定范围与启动设计，不等于批准新 purge 方案。
- 本次产物：[生命周期与清除 ADR](../adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md) 与本文；只改这两份新文档。未改生产、迁移、已批准法律，未运行 Rust/TS 门禁、提交、推送、开 PR、派子 agent 或执行 Mini 任务。

用户目标是 Agent24 核心闭环：输入 → 记忆/上下文 → 模型 → 审批与工具 → 实际提交 → 系统回执 → 查看/暂停/撤回/更正/导出/清除/恢复。外围模块仅按接口接入协同，不能让其未上线或新研究候选阻塞个人可控，也不能另起权限系统。已核对现有策略、M1 hardening 与 snapshot provenance 草案；本文承接差量，不重复发明已交付能力。

## 2. P1 前余项核销（证据与未完成分开）

> 协调者补充：本设计完成后，又补得 P02 同进程/冷恢复 marker history 红证，P03 真实 HTTP 撤回/pause/fallback/qualification/no-session 共 5 项通过，协调者独立复跑亦通过。P03 结论是已构造请求按现行边界继续发送，不据此新增 H04 bug。下表与 Todo 中涉及这些已列场景的“补测”应转为维护回归与设计输入；同秒固定时钟审计 ID 碰撞已由 #1152（`7dd6f08f`，2026-10-23 合并）修复；H03-b 发送副本按来源 kind 整形已由 #1250（`727fad28`）合入；冷恢复矩阵与新发送屏障设计仍未完成。最新证据见探针报告 §8.1/§8.5/§9，不能只读前轮快照。

| 项目 | 当前结论 / 证据级别 | 下一动作与关闭条件 |
|---|---|---|
| M1/P0 #720 | 已验收发布：jason 2026-10-06 在 `c503bf1` 真机链；#720 入 main | 保持关闭，空白验收模板不能推翻事实；T12–T14 聊天记忆/回执已有，只做差量 |
| M2 #767 / H01 #1024 | M2 已发布；H01 重复撤回事件 ID 修复已交付 | 不重做；F7 是移交 P1 的新阶段工作，H01 现有测试作回归 |
| H02 / R01（当前红证） | 真 WriteGate 新 evidence 重申后重试 Conflict；真实 `chat_remember_turn` 再次 remember 返回 Failed；尚非 HTTP E2E | 优先最小修复当前审计匹配，保留缺失/伪造 audit、跨 owner 拒绝；固定时钟同秒同 evidence 的第二重申 SQLite 2067 ID 碰撞已由 #1152（`7dd6f08f`）修复，故障注入回滚控制保留；新 evidence 重申与存量重申已由 #1153（`4b3ace00`）、#1154（`a09561c2`）修复，红证已转常规回归 |
| P02/H03 / R02（当前红证） | 完整 marker prompt、普通历史在 provider 收到的副本中丢失；持久原文未删；已有 UserInput tag 未阻止误裁 | 优先来源契约重审与修复；补 host/prompt/history 完全同文、同进程/冷恢复、旧/坏标签和读取失败；不能关闭撤回校验来保输入。进展：来源表 #1168/#1181/#1195/#1251，拒发门 #1197，按 kind 整形 #1250；冷恢复矩阵尚缺，见第 4 节 |
| P01 / R03（限定绿） | 新 run pause、同进程审批、独立进程退出后的冷恢复符合 OpenAPI；撤回裁请求副本而不改 snapshot | 已运行场景核销为契约回归通过；daemon HTTP/启动 orphan sweep、并发 pause 另列补测，不要求改变暂停契约 |
| P03/H04 / R04（HTTP 观察已补） | 真实 Router 撤回/pause/fallback/qualification/no-session 共 5 项通过，协调者独立复跑 exit 0；首 provider barrier 发生在收到请求后 | 已列场景转为回归；尚未证明首次发送前的线性化屏障。先冻结旧承诺与新要求界线，确有旧契约 red 才建 H04；已发送不可追回，网络 await 不持 SQLite 写锁 |
| R4 Held/System | 未证进入当前个人产品链 | 记录消费者核查；自动抽取接线时再设计，不改全库信任政策 |
| R5 FTS/vector/双时态 | 当前资格一致性与 as-of 缺口未整体证实为生产越权 | 并入更正/F11 设计；向量上线属 P2，未启用标 N/A 但残留数据仍纳清除清单 |
| R6 派生页/自动抽取/generation 通用框架 | 未来增强，不是已发布 M1 待修 | 退出本轮修复范围；purge 必需的范围 fence 不扩成全量 Hindsight |

第一轮探针默认合计 7 passed / 5 ignored；补充轮更新为 8 passed / 7 ignored，另有真实 HTTP 5 passed。ignored 包含 opt-in 红测与冷恢复子夹具，不能当作全部场景通过；定向 clippy/fmt 不是完整提交门禁。本轮只读引用。独立复核接受探针已声明范围、支持 P1-01 设计启动，**不支持全 readiness 关闭或 purge 开工**。

## 3. 关键设计选择与前置门

拟采用“生命周期内 no-loss，用户显式 purge 例外”；容量满不静默清历史，暂停/撤回/更正不删除原文。purge 用可信身份与独立确认，先 durable intent/fence，后逐库事务与异步副本清理；恢复先验证独立删除水位/tombstone，无法证明则隔离。无痕从入口限制所有持久落点，不靠会话结束删 EventLog 冒充全链无痕。详见 ADR，均待外审。

优先外审高影响决策：①会话来源闭包/全个人域的首版粒度；②独立恢复控制存储的信任锚、回滚检测及 tombstone 退休；③默认加密恢复包与密钥丢失行为；④在途 provider/工具与失联外部副本的承诺；⑤无痕首版允许的工具/provider。推荐先做可证明的最小范围；这些未定时继续探针/非破坏设计，不进入对应生产实现。不要求用户决定普通函数名或排版。

P02 优先检查复用/扩展 `agent24-store/migrations/0016_run_source_tags.sql` 与消费者：K1 标签控制 LocalOnly/路由，不自动提供 host recall 实例证明。旧 provenance `0015` 已被 `module_consents` 占用，store 当前到 `0018`；另建表与复用的取舍、旧 snapshot Unknown 处置、消息/来源原子绑定、执行前验证点必须在新来源契约中外审。归档 #1094 不是批准，不照抄其 SQL 或把历史缺标签补为 NoRecall。

## 4. 可派发 Todo、依赖和测试门

以下 ID 延续 10/22 跟踪别名；子片是建议拆分，**全部未派生产**。每个子片独立验证、不得以整项数千行实现一次交付。`P1-01` 本轮为“草稿已产出，外审待办”，不是完成批准。

> 2026-10-26 按合并证据核销。勾 `[x]` 须同时满足：有合并提交实现，且退出测试门已由合并测试覆盖；证据写作「#PR merge短SHA」。部分完成的保留 `[ ]` 并写明已做/未做。推迟的门不勾，触发条件见 [R05-4](R05-4-H03-CLOSEOUT.md) §3。「设计已接受」不等于「实现已完成」。

| ID / 顺序 | Todo 与最小分片 | 依赖 / 退出测试门 |
|---|---|---|
| L01 | [x] 模块/研究入库 #1146/#1147；[ ] 报告 #1148 与全部探针分片入库。已入库：报告 #1148 `146dae39`，H02/容量夹具 #1151 `91158847`，时钟接缝 #1152 `7dd6f08f`，容量探针 #1164 `09771b6d`。未核到入库：#1148 所述 897 行 agent 夹具与 354 行 HTTP 夹具；P02 场景已由 #1250、#1197 的回归测试覆盖，但原夹具文件本身不勾 | 固定 SHA/hash、完整命令/退出码；文档合并不等于修复，未覆盖不关闭 |
| L02 | [x] #1145 merge commit 吸收固定 main，核所有权与精确 ruleset；[ ] 每片开工重核 | 同步 merge `8535154f`，已保留 main 祖先，未改 ruleset；477/0 是同步前测量，不永久沿用 |
| R01/R06-H02 | [x] a 固定当前红测与控制（#1151 `91158847`）；[x] b 最小审计幂等修复（#1153 `4b3ace00` 新重申，#1154 `a09561c2` 存量重申）；[x] c 同秒风险独立核销（#1152 `7dd6f08f`，原「未修复」已过期，五项 `h02_` 常规测试与回滚控制） | L01/L02；当前 chat 红→绿且 audit 伪造/缺失、owner 反例仍红；H01 三轮撤回回归；同秒须可控时钟，不靠运行速度；数据写入语义需外审 |
| R02/R05 | [x] a 新来源设计（设计已接受，不是实现完成：R05-1 #1170 `4fbc3728`、R05-2 #1173 `9ba7d5ee`、R05-3 #1185 `2207cdc6`、R05-5 #1248 `4d2d2100`，R05 ADR 修订作为接受记录）；[ ] b fresh/history/审批冷恢复矩阵（部分。已做：fresh 与同进程审批恢复，#1250 `727fad28` 的 `send_trim_tests.rs` 契约测试，含同文三来源、同形 UserPrompt/SessionHistory 逐字、真 HostRecall 撤回被裁、snapshot 字节不变；冷恢复仅 `resume_after_restart_drops_revoked_recall_line` 一例，重开文件库后撤回行被裁、其余逐字、库内 recall 正文保留。未做：冷恢复下 `memory=None`、`active_ids` 读失败、同文三来源，以及恢复后来源行与 digest 不变的断言；`send_copy_drops_host_recall_when_memory_is_none` 与 `resume_after_append_binds_kind_by_store_seq` 用内存库，不算冷恢复）；[x] c 旧/坏/缺来源策略（整次拒发 `source_unproven`、不回填：#1197 `9df7d670`，测试 `null_host_recall_does_not_call_the_provider`、`unreadable_assistant_recall_ref_fails_before_the_next_send`、`origin_read_failure_does_not_call_the_provider`、`legacy_resume_approve_does_not_run_shell_or_call_the_provider`、`legacy_resume_deny_does_not_run_shell_or_call_the_provider`；坏 digest 读成 Unreadable 见 `tampered_instance_digest_is_unreadable_and_not_no_recall`；无来源旧暂停通知 `legacy_unsourced_pause_notice_still_refused`，#1251 `3ec207ec`） | L01、K1 当前表/调用点；同文三来源各自正确，用户输入逐字到 provider、真 host 撤回被裁、snapshot 不改；新契约精确版本 APPROVE 才派 H03。该前置门已满足：R05-1 #1170 于 2026-10-24T09:52Z APPROVE、10:08Z 合并，H03 首片 #1197 于 21:04Z 合并；R05-5 #1248 于 2026-10-26T05:19Z APPROVE 并合并。推迟，不勾：G1、G2、G4、G5、K1 同事务，触发条件见 R05-4 §3；冷恢复矩阵见同节 |
| R06-H03 | [x] a 持久来源绑定（#1168 `c9a19532` 迁移 0021、#1181 `631ff507` 快照与来源同事务、#1195 `b680cb0a` 迁移 0022、#1251 `3ec207ec` 迁移 0023 HostSystem；K1 标签仍是 `append_snapshot_run_messages` 之后独立 `tag_run_source`，K1 同事务推迟，不在此勾内）；[x] b fresh 发送副本（#1197 `9df7d670` 拒发门、#1250 `727fad28` 按 kind 整形、#1205 `b9e4eb5f` G3 角色门；provider spy 先红后绿见 #1250 PR 正文）；[ ] c 同进程/冷恢复及诊断消费者（部分。已做：同进程审批恢复，#1197 的 `drive_resume` 恢复门，#1250 的 `resume_after_restart_drops_revoked_recall_line`。未做：冷恢复缺口同 R02/R05 b；诊断消费者无合并提交；G2 推迟） | R05 批准（已于 2026-10-26 满足，#1248）、L02；每片可用或受控不启用，迁移/wire 分片独立门；provider spy 与真实审批路径 red→green，旧 run 不被错误继续执行 |
| R03/R04 | [ ] P01 剩余 daemon 入口回归；[x] P03 已列 HTTP/资格/no-session 观察；[ ] 新发送屏障契约表，仅确有违约再开 H04 | 不阻塞 ADR 起草；阻塞对应 readiness 声明/发送保证。fixture 控制顺序、首 provider/fallback 收包分别记录，不将 pause 既有恢复例外判 bug |
| P1-01 | [ ] 外审本文与生命周期 ADR，冻结 no-loss/purge 例外、scope/身份/恢复控制与审计。状态：生命周期 ADR 第 9 节五项拍板的外审接受记录已形成（#1247 `59d2130d`，clestons 2026-10-26T04:54Z APPROVE，即 adr-s9 的接受记录，只接受设计与首版范围，不批准迁移或生产实现）；R05 ADR 修订的接受记录见 #1248 `4d2d2100`。本计划文档本身仍是 DRAFT、未经外审接受，故不勾 | L01；逐项处置 §3 数据落点、失败恢复/取消/幂等/副本承诺；外审意见有接受/修订记录才完成设计门 |
| P1-02 / F7 | [ ] a 合成数据容量探针；b 行数/payload/磁盘/附件口径；c 保留/压实与配额失败设计 | a/b 只读源码和隔离合成库测量可与 ADR 并行；c 及生产行为按相关 ADR 批准门；核 per-owner override、personal 全路径、满额后聊天/原文/回执、审计空间与恢复。默认 200000/256 MiB 仅为 migration 0014 静态配置，不是磁盘占用或机器测量 |
| P1-03 | [ ] a 受权用量 API；b 记忆页占用/会话排行与告警；c 满额回执与恢复入口 | P1-02；阈值用量纲/测量选定，触发与解除可测；配额/磁盘满不能报“已记住”，原文未保存必须显式显示；聊天继续不等于持久化成功 |
| P1-04a | [ ] 大输出 owner 内附件格式/引用；[ ] 原子写入与校验；[ ] 回放/回收 | P1-02 + 格式外审；原文逐字可还原、摘要不代替原文；写 blob/提交引用/回收各点 crash，不产生悬挂引用或误删其他引用 |
| P1-04b / F7 | [ ] 冷归档 manifest；[ ] 归档切换与导回；[ ] 热副本安全压实 | 04a、P1-05 格式；冷包校验且回放等价后才切引用/回收；损坏包/断电不丢原文；用量与磁盘分别核，自动归档不自动永久删除 |
| P1-05 | [ ] a JSON/Markdown+加密/世代格式；b 导出；c 隔离导入/授权映射；d 幂等与恢复门 | ADR + 格式外审，04a 引用契约；干净数据根恢复数量/原文/来源/修订/撤回状态；拒越权、zip 路径穿越、未知格式/缺附件/旧水位；导出并发 purge 不泄露已 fence 内容 |
| P1-06 / F11 | [ ] a 双时态/来源/冲突设计；b 修订账本；c 用户更正 API；d 查询与 as-of | H02/H03 核销、04/05 格式可承载版本；`asserted_by` 由宿主身份注入，模型不能自证；当前仅新有效值、历史 as-of 正确、未知日期不造；高分失效项不挤走有效项 |
| P1-07 | [ ] 偏好与事实分域；[ ] 显式设置/修改/撤销及回执 | P1-06；语言/称呼偏好不能变权限指令，来源可信且可查看；导出/清除覆盖偏好历史，不自动从聊天抽取 |
| P1-08a | [ ] purge 预览/确认/意图；[ ] 恢复控制存储与 fence；[ ] 可取消边界 | ADR 外部设计批准 + 04/05/06 数据格式；双 owner、陈旧确认、满盘/满配额、同 key 不同范围、取消/提交竞争；在删除代码启用前先证明隔离与恢复 |
| P1-08b | [ ] 单库正文/索引清理；[ ] Run/附件/归档；[ ] 托管备份/模块回执 | 08a；每落点 crash 重试、部分失败保持 fence；所有版本/摘要/工具正文清除；多引用不误删；不受控副本如实列出，不能伪装完成 |
| P1-08c | [ ] rebuild/恢复防复活；[ ] 重启作业续跑与最终验证 | 08b、可信最新删除水位；旧/缺清单、旧 snapshot/旧 worker 写回、整包重新导入均不可复活；明确逻辑清除与 SQLite/文件物理回收不同 |
| P1-09 | [ ] a 全落点流向与能力白名单；b 临时会话；c 审批/工具/provider/崩溃路径 | ADR 批准、P1-08 受控范围门；从首字节不写 EventLog/Run/trace/附件/日志正文，正常结束和异常退出/重开均验证；不可冷恢复的回执真实，外发副作用逐项说明 |
| P1-10 | [ ] 随各片接管理页/操作回执；[ ] 最终统一状态与无障碍/错误恢复路径 | 各 API 真实结果；复用 M1：成功、重复、失败、Held、暂停、撤回、更正、清除 partial 分明；模型口头“完成”不能生成成功回执 |
| P1-11 | [ ] 自动联合回归；[ ] jason 真机链；[ ] 人工 release | R01–R06 按范围核销、所有 P1 必选片/高影响决定落地；记录精确 head、CI/外审/真机证据，任何延期项须改交付范围并称“部分交付” |

顺序：**H02 最小修复与 P02 来源契约优先 → ADR 外审 → 容量告警/F7 → 附件归档与导出导入 → 更正双时态 F11/偏好 → purge/无痕 → UI 回执收口 → 联合验收。** 设计与剩余探针可先推进；每个操作的 UI 回执随 API 交付，最后只做一致性收口。没有批准的契约不借“提前打底”写破坏性生产路径。

波次 A 出口：L01/L02、有证 bug 的修复/设计及剩余探针都有明确结论；当前仅设计可开始，不标全绿。波次 B 出口：容量可见、原文可恢复、受权导出导入可验证。波次 C 出口：更正/偏好/清除/无痕的读写、恢复与回执一致；原 M1 真机记录不替代 P1 新验收。不承诺未经测量的日程或性能提升。

## 5. 验证、分支与发布规则

实现前在获准笔记本核所有权、同步 `ab/memory-p1` 并确认 ruleset 精确覆盖，再从该集成基线建 `ab/memory-p1-NN-短名` task worktree；传统任务 worktree 位置遵守 AGENTS，已派发 Orca worktree 不在此设计任务搬动。task PR base 只能是 `ab/memory-p1`，禁止直接 main、不碰他人/受禁方向分支，不 force-push。集成每周 merge main，不能 rebase 共享历史；本轮不执行这些操作。

每 task **≤300 生产行，并且 PR 总变更默认 ≤300 行（只排除锁/生成文件，测试与文档仍计入）**。10/22 稿“测试文档另计”与 AGENTS 冲突，本计划收紧按后者。不可分片的中间版本须说明原因走仓库既有例外；优先设计/测试/实现按可验证单元拆开，不用忽略测试行隐藏大 PR。author、中文标题/正文及 pre-pr-check 逐类回应遵守 AGENTS。

先写验收测试并在修复前取得确定 red，再实现和 green，保留阴性控制；新能力用失败注入/变异证明门能抓住缺实现，不写仅复刻实现的测试。合成数据、双 owner、真实 API/审批/provider spy 与独立进程重开优先；无真实用户 DB/模型请求。现有 opt-in 红测试在对应修复后转为常规回归，不能靠 ignored 宣称修复。

提交前 Rust 在 `rust/` 执行：`export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`、`ulimit -n 4096`，再 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。只有 AGENTS 列的四个既有沙箱失败可按白名单记通过，并记录总计/名称；其他失败阻塞。不得创建 worktree 私有 target，不能把定向探针冒充完整门禁。

改 `apps/`/`packages/` 才追加 `pnpm install --frozen-lockfile`、`pnpm typecheck`、`pnpm lint`、`pnpm test`；改 `protocol/` 追加 `pnpm lint:openapi`、`pnpm gen:api` 并确认无生成 diff。本片纯文档，源码静态快照不冒充新运行；同一生产树已验证的门禁可明确标注复用，不以重复 cargo 占用正在运行的构建通道。

执行用户 2026-10-10 已生效的全局硬门：**所有 Agent24 PR 均须外部 PR-Daemon（clestons）对精确 head APPROVE、required CI 全绿且 mergeable**，并满足 pre-pr-check 逐类回应与适用本地门禁。仓库较旧的普通 task 可先合口径不用于本线；这不是待采用的建议。三条件满足立即合入获准集成分支，不向已批准 head 追加修改。purge 等破坏性行为另须先获设计批准，不能只凭实现 CI。

release `ab/memory-p1 → main` 永远由人开、人审，需 PR-Daemon APPROVE 与 jason 真机验收；worker 不开 release。每次验收区分“源码/定向测试/HTTP 接线/真机/发布”，记录精确 SHA、命令、退出码、证据路径和剩余项。本设计 worker 未创建调度器；协调者已另行取得 Pi 原生激活回执和首次 continuation。后续频率以宿主 LoopX quota/continuation 实际调度为准，遵守全局两分钟内安排下一检查的要求；时间戳不冒充计时器，无变化不重复跑测试。不得选 Fast/priority，不解除任务审批或暂停门。

## 6. 外部模块的协同边界

| 接缝 | P1 只要求的协同 / 保持的限制 |
|---|---|
| iDoris / 模型 | Agent24 的 run、授权、记忆权威不外移；当前通用 provider 不等于 iDoris 隐私/预算整链已验。登记请求、缓存、WAL/日志及远端保留边界；loopback 不代表无外发/无副本。Superfluid/QoS/SHM 研究不纳 P1 依赖。 |
| Documenting / Knowledge | 文档/检索资料与个人事实分域；来源引用、索引副本与删除接口协调。DOC-1 r5 #1068 仅其明确范围已验；Knowledge #1085 v2 工具广告决策仍待确认、宿主 binding 未接，不能借 P1 放行。 |
| Media / AgentEar | 附件只处理核心引用与所有权；MediaBot 不能按名字等同 MediaBase。AgentEar A3 附着验收不等于完整语音链；后续通过统一 WriteGate/回执消费，不扩大流式项目。 |
| 核心工具与 D0/D1 | #794 四片已合但显式根确认/撤销、根内免问接线、根外恢复与审计仍逐项核；不改已批准审批法律。D0 24GB 复测、D1 集成分支进度不阻塞 P1。 |
| OA / Workspace / M3 | Office Assistant、OD-M11/12、TPI-W/OpenCreator 继续暂停；M10 已落地不解锁后续。M3 仍等第二个真实用户，P1 完成不会自动解除此门。 |

外部模块未接入的副本标 N/A，已发送但不受控的副本标不可撤回，已登记且离线的受控副本标待处理；这些状态不能互换。模块停用/卸载不等于其历史副本已清，恢复接入先核删除世代。未来协作实现另派给对应 owner，此任务不触碰其他仓库或派 worker。

## 7. 本轮交付状态与远期展望

已完成的只是两份 DRAFT 文档与固定基线静态核对：明确生命周期差异、容量真实计量口径、副本处置、恢复/取消/失败状态与按片测试门。设计外审仍待；L01 文档部分已入库、探针仍分片推进，L02 固定基线同步/ruleset 已核；剩余探针、所有新行为实现与 P1 真机验收未完成；不得据本文勾掉 P1-01 的批准门或宣称 P1 已启动生产。

入库文档核验须检查相对目标、表格、DRAFT 和公开证据链接；本片只新增这两份文件。检查不是设计批准或产品测试通过；本 ADR 的待决项只能经明确设计决策关闭，不能凭文档 PR 的 APPROVE 自动标成已实现。

P2 在 P1 控制边界之上做本地向量/RRF、候选抽取和可重复评测，先确认质量与资源成本，再接用户确认写入。P3 再做共享空间与社区协同，明确成员、来源 ACL、发布与撤回权，不默认从个人域迁移。P4 叠加身份、组织保留/法律保全与治理审计；P5 再评估图、巩固、经验和高级语音记忆，均不回头削弱个人生命周期语义，也不自动解锁 M3 或当前暂停线。
