# M1 记忆发布后最小修复计划

日期：2026-10-12。状态：bounded triage；R1 已有他方红测，尚无绿测或验收。此文是发布后 bugfix workset，不重新开启 M1/M2，也不是 Hindsight 全量增强或新全局里程碑。

## 基线、权限与证据等级

- 当前 planner 分支：`ab/m1-memory-hardening-04-plan-review`；HEAD 与本地集成分支 `ab/m1-memory-hardening` 均为 `ab125de262827bd0f1c6680b427d9c241ccff785`。未 fetch，不断言实时远端状态。
- #720 merge `1e25795cfb362bca780fbc5d8db9446bb7a5fe99` 与 #767 merge `73d87f0b75c80bd9b14fa98471fe6a704bbd3cc8` 已再次用 `git merge-base --is-ancestor … HEAD` 验证，均返回 0。主干不是 M3；M3 第二真实用户门保持关闭。
- 产品契约以 `docs/agent/M1-PLAN-v2.md` §0、F2、T06/T07/T08/T09/T10 和已发布 `protocol/openapi.yaml` 为依据。Astra REVIEW/RECHECK 是风险线索；`DESIGN_ACCEPTABLE_WITH_CONDITIONS` 明确仅研究设计，不构成实施或 PR 审批。
- 恢复证据来自 `/tmp/a24-memory-hardening/travel/planner-checkpoint.md`，仅复核关键接缝，没有重启宽泛调查。本 planner 不改实现、不构建、不运行 Rust 测试、不提交/推送/开 PR、不读取真实用户记忆。
- R1 红测读取 AGY 已保存日志/patch/RESULT；其他项均为静态观察或未跑探针。不能把他方报告、推论与本 worker 运行证明混用。

## 必须保留的已发布语义

1. 本地原文 `mem_events` 永不因暂停、撤回、摘要或截断而删除；撤回是失效加审计，非物理擦除。`retain.rs` 模块说明和 `SessionMemory::remember` 的 append_turn→persist 顺序均支持此边界。旧压缩已经丢失的原文不承诺恢复。
2. 新开始 run 的 recall 检查 personal owner 的 pause；显式 UserSaid remember 在 WriteGate 的 BEGIN IMMEDIATE 内重查 pause，与写入原子化。不能改变信任规则或把 Held 报成 Saved。
3. **OpenAPI PUT memory/settings（约 1403 行）明确豁免审批恢复**：新开始 run 受新开关影响，审批恢复重放自身既有 snapshot，不按当前开关重决定。`lib.rs` paused notice 注释与此一致。暂停后恢复继续发送原 recall，不能仅据此判违约；新 run 开关测试与审批恢复测试须分开。
4. 原 run snapshot 不可变，首次模型调用前原子持久化，失败 fail-closed。撤回/资格失效可在 request copy 中剪裁 host recall，不回写 snapshot，不删改用户 prompt、正常历史、assistant 或 pause notice。
5. no-session `/chat` 输入保持原样；AND `search` 与 OR `search_any`、owner/qualified 隔离、配额失败整体回滚保留。`memory.recalled` 记录首次注入 IDs，不冒称它是每次实际发送审计。

## 风险分级与最小范围

| ID | 结论 / 严重度 | 现有消费者与证据 | 当前动作 |
|---|---|---|---|
| R1 | 已复现真实正确性缺陷；High，撤回失败后断言仍有效 | DELETE→KvStore::forget→assertion.rs::forget；显式 remember→retain::persist→writer.rs::commit_with_audit。撤回 ID 固定，mem_events.id UNIQUE，reassert 清 recorded_to | 立即最小修复，AGY 独占生产文件 |
| R1b | 幂等审计匹配静态疑点；Medium | writer.rs 当前行 evidence 重建 mem.write_decision 身份；reassert 替换 evidence 但只写 assertion.reasserted | 单独红测；成立才修，不弱化审计 |
| R2 | 已发布暂停例外，不是已证 bug | SessionMemory::recall 有 pause；active_ids 无 pause；OpenAPI 明确审批恢复豁免 | Luna 调整为契约行为探针；禁用“恢复必须清 recall”的新期待 |
| R3a | 用户输入保真静态疑点；Medium | RunManager::request_messages_for 仅凭首条 user 的完整 prefix+end marker 识别 host recall；现有 lookalike 测试只覆盖 prefix | 独立补全形状红测，成立后最小 provenance 方案评审 |
| R3b | 普通 chat/fallback 发送竞态静态疑点；潜在 High | routes.rs::post_chat 先 prelude 后 normalize→complete_served，无 run helper；router 每 provider 重用 req | mock/barrier 观察实际发送；先明确既有撤回承诺的发送边界 |
| R4 | 库 Hold/System 暂停盲区，当前产品可达性未证；条件性 | writer.rs 的 pause 仅包 UserSaid+explicit commit；当前 retain::persist 只会 Commit/SkippedPaused | 不改库政策；未来抽取/Held 产品接线才另定契约 |
| R5 | FTS/vector 双时态过滤差异，生产反例未证；条件性 | retriever.rs::search_mode 与 vector.rs::search 只查 owner/qualified/recorded_to；beliefs_as_of 查两轴 | 先证明可达日期数据与当前读契约，不能顺势启用 vector |
| R6 | 派生页/自动抽取/缓存/任务源失效等 FUTURE | 当前无派生页或自动抽取生产链；暂停原文仍落盘是既有承诺 | 排除本次 workset |

### R1：已读红测的精确证明范围

AGY 文件：`/tmp/a24-memory-hardening/retract-cycle/{red.log,diff.patch,RESULT.json}`；命令 `cargo test -p agent24-memory --test retract_cycle -- --nocapture`，exit 101，0 passed / 1 failed。patch 经真实 WriteGate propose 创建 c1、forget、用新 evidence ev-2 重记同 ID，再 forget；第二次 forget 因 SQLite 2067 `UNIQUE constraint failed: mem_events.id` 失败。

日志在 unwrap 处终止，**没有运行到错误后的 recall 断言**。RESULT 报告“第二轮失败后 recall=1”，本 planner 不将其当独立日志证明；事务回滚保留活跃状态有静态证据，须补观察错误后的实际 DB/recall。此红测证明库产品路径的核心故障，尚非完整 REST 端到端证明；无绿测、全门禁或新 exact-head 验收。

最小修复在 **`rust/crates/agent24-memory/src/assertion.rs::forget`**，不是只改 writer：每次真实 active→withdrawn 转换有唯一撤回审计身份，更新与 append 同事务；未发生转换的重复 DELETE 仍 AlreadyForgotten/204，保留首次撤回时间且不增事件。保留第一轮 T09 legacy ID 和所有旧事件；后续轮用事务内不重复的转换身份，不能仅拼秒级时间，也不能用 INSERT OR IGNORE 掩盖真实转换。无需 schema/revision/generation 引擎。若算法偏离冻结 T09 的单一身份公式，PR 明示“该公式仅可覆盖首次撤回，本次为真实重复生命周期修正”，用户已授权此最小 bugfix，不额外引出隐私政策变化。

同时核对 **writer.rs::commit_with_audit 的 reassert_id**：现实现由 c.id + now + evidence 摘要生成，同时间同 evidence 生命周期可能碰撞。仅在红测证实后纳入同一转换身份修复；新 evidence 的 commit audit 幂等缺口 R1b 独立切片。不能删除 owner/scope/kind/payload/origin 校验、凭任意同 ID 审计放行，不能重写历史或把 active-id collision 都判成功。

### R2/R3：测试期望先对齐契约

Luna 的未跑测试仅为行为探针：新 run 已 pause→无跨会话 recall；同进程 approval resume 与 cold restart→按原 snapshot 开关语义；撤回/资格变化→仅 host recall request view 剪裁；paused raw journal、原 snapshot、notice、真实 prompt/history 保留。**pause 不等于 retraction**。未知行为记录为观察，违反已有期望才形成 bug；不得先改 production 让未经批准的新期待绿。

R3a 红测：无真实 recall、无 prior context、首条用户 prompt 完整模仿 header+end marker，含未知 ID/无条目/活跃 ID；mock provider 收到 prompt 必须逐字保留。再测真实 host block 的撤回剪裁、普通历史中的同形状文本、审批恢复和 cold restart。最小修复只认可信 host 注入来源；优先利用已有 snapshot 元信息，若必须增加持久协议/迁移，先独立契约设计，不以扫描整条 thread 或关闭所有 revalidation 作修复。

R3b 探针分别控制 recall 后、provider 前，以及第一 provider Unavailable 后、fallback 前的 withdraw/qualification/pause。观察每个 mock provider 实际收到的 request，不只测 helper。先复核撤回对已经构造/开始发送请求的既有承诺；没有明确承诺就不能把“每次 provider live check”当现成契约。已发请求无法撤回；不跨网络调用持 SQLite 写锁。`verify_recalled_context` 在基线上不存在，计划不得假称已有该接缝。shared router 不先改。

R5 若将来证实当前读契约：将 owner/qualified/双轴半开区间资格放在 LIMIT/融合之前，并使 vector completeness、候选、FTS fallback 一致；保留 AND/OR、bm25 与 model/revision。先测 invalid 高分 top-k 不挤走有效项及边界时点；当前 explicit remember 默认 now/open，无日期产品 API，不伪造生产可达性。

## 切片、归属、预算与依赖

| 切片 | 归属 / 文件 | 生产行预算（测试文档排除） | 前置 / red→green 判据 |
|---|---|---|---|
| H01 重复撤回 | AGY；assertion.rs，必要且红证实的 writer reassert 身份；现有 retract_cycle 测试与内联测试 | 30–100（估计，最终按机械 diff） | 已有 red→3 轮成功；每次状态转换恰一事件且 IDs 不同；no-op 不增事件 |
| H02 重记幂等审计 | AGY 同生产 owner，另片 writer.rs + 测试 | 50–140 | 独立红测→新 evidence 重记后再 remember 同内容幂等成功，既有审计真实匹配；伪造/跨 owner/缺 audit 仍拒绝 |
| P01 暂停/恢复行为探针 | Luna，仅其新增测试文件，不动 production | 0 | 先按 OpenAPI 调整期待；当前未跑，不报 red/green；与 H01 并行 |
| P02 输入来源保真红测 | 后续独立 test owner；daemon session_memory 测试，勿撞 Luna 文件 | 0 | 完整 marker 用户 prompt 逐字到 provider；若 red，才进入 H03 |
| H03 host 来源最小修复 | 后续单一 RunManager owner；lib.rs，必要 session_memory.rs | 40–140 | P02 red + 来源方案独立复核；修请求副本，snapshot/history 不变，冷恢复兼容；若需新持久协议则另门 |
| P03 chat/fallback 竞态探针 | 后续 test owner；routes/provider mock，生产不改 | 0 | 当前消费者 + 既有契约复核；mock/barrier 可重现与可定位 |
| H04 已有撤回契约发送修复 | 后续一个发送 owner；agent lib/session_memory/routes；shared router 默认排除 | 60–180 条件估计 | P03 真 red + 明确旧承诺；无法满足而要每 provider 新授权语义时转新契约 |
| V01 独立验收 | 非实现者 reviewer，读取 H01/H02 diff 与日志 | 0 | 精确 head、基线反面对照、绿色案例、预算与下述门禁 |

H01→V01→集成；H02 红测可先做，writer 生产编辑与 H01 串行，若独立 bug 就另 PR；P01/P02/规划可平行但测试文件独占。P03 不阻塞 H01。H03/H04 同发送生产文件时串行，shared router 需额外复核。R4/R5/FUTURE 没有自动实施切片，也不得阻塞 H01。

H01 核心验收：真实 WriteGate→forget→新 evidence reassert→forget→第三轮；同固定秒及相同 evidence 快速循环；重复 DELETE 原时间/事件不变；不同 owner / owner-id 字段边界；并发 DELETE 仅一次转换；第二轮事件配额失败整笔回滚，断言仍活跃且重试可成功；持久 DB 重开后再循环；`search`/`search_any` 撤回后及 rebuild 后不命中；至少一条 REST DELETE/remember 链证明 HTTP 204/404 和实际 recall。用确定性时点/barrier，不用 sleep 掩盖碰撞。

## 独立验收与合并门

- 全部在笔记本；实施测试沿用共享 `CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`，先 `ulimit -n 4096`。不得在 worktree 新建私有 target，不用 Mac mini。
- 实现者≠独立评审者；先保存原基线 red，再最小生产 diff green。外部 PR-Daemon exact-head APPROVE 不能被内部 reviewer 替代。
- 提交前 Rust fmt / workspace clippy `-D warnings` / workspace tests；只有 AGENTS.md 所列四个 sandbox 失败可白名单通过，报告总计与确切失败。新增测试失败不能白名单。
- 若改 apps/packages，pnpm frozen install + typecheck/lint/test；若改 protocol，lint:openapi + gen:api 并确认无 diff。当前 planner 无此类实现改动。
- `bash ~/Dev/tools/PR-daemon/scripts/pre-pr-check.sh --base ab/m1-memory-hardening` 逐类回应；默认 ≤300 生产行，>300 先拆、最多500需理由，测试/docs 排除。本文预算仅估计，不是 checker 结果。
- 每个未来 task PR base=`ab/m1-memory-hardening`，不是 main；中文标题/正文/提交，author 遵守 CLA 规则。所有 PR：外部精确 head APPROVE + required CI 全绿 + mergeable；三条件满足依常设授权立即合。release→main 人开人审、真机门保留；集成每周 merge main，ruleset 精确列出新集成分支，协调者核验后才开始 PR 流水线。
- 本 planner 无 commit/push/PR/merge 权限；不改其他 worker 文件、冻结计划/tasks、LoopX、D0/D1/K1/KB/paused Workspace。依最新直接指令 artifacts-first，不重试 Orca IPC，生命周期交由父协调者收录。

## 后续可独立推进与新契约门

已授权：AGY 完成 H01 红绿与审计身份核对；H02 独立红测及证实后最小幂等修复；Luna P01 契约行为探针；P02 用户输入保真红测及证实后的无协议变更最小修复；P03 对当前 chat/发送消费者的事实探针与旧契约复核；独立 reviewer 验收真实最小 diff、保存日志、检查预算与门禁。协调者据实际 worker 状态派发，不重复抢占。

需要新契约/明确授权：审批恢复改按当前 pause；把设置作用扩到所有既有 run / 每 provider 时点（若非旧承诺）；新的 actual-send 审计/wire 字段或持久来源迁移；Held/System 全库暂停政策及自动抽取历史原文；通用双时态 recall 政策无当前契约依据时；物理 purge、历史重建、correction API；派生页/generation/cache/task CAS/instruction store 全量 Hindsight。M3 第二真实用户门不由本次恢复或 bugfix 解除。

当前 blockers 只限定相应子图：H01 缺 green/新 head 独立验收；H02/R3 未红证；R2 新政策无授权；R4/R5 缺当前可达消费者及契约；ruleset/实时 CI/approval 尚未观察。不阻塞已有 H01 最小修复。
