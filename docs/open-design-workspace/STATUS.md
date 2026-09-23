# Agent24 × Open Design 执行状态

> 更新：2026-09-23（Asia/Bangkok）

## 阶段门禁

| 阶段 | 状态 | 证据 |
| --- | --- | --- |
| P0 设计冻结 | PASS | ADR-001～005、依赖台账、风险与兼容矩阵已冻结并通过 SOL review |
| A24-OD-00 capability 安全前置 | PASS（逐层合并中） | #226/#227 已合入 main；#228 等待基线更新后的重新批准，其后代保持依赖顺序 |
| P1 Open Design 原样基线 | PASS | `open-design-v0.22.2@73953213a`，fork PR #1，SOL exact-head 复核通过 |
| P2 workspace contract | IN PROGRESS | DB registry/lifecycle/renew 与 allocation journal schema/约束通过 SOL；安全 root allocator/host lease/run binding 未完成 |
| A24-OD-05 sidecar foundation | IN PROGRESS（未接线） | #253 合入 desktop sidecar ownership/endpoint-handoff contract；manager 由 #254 承担，#429 controlled pipes、#431 graceful stdin close、#435 soft-stop seam、#436 dormant grace primitive 与 #438 fixed stdout worker 推进中；host `run()`/产品路由仍未接线 |

当前 PR 监控结论（2026-09-23 Wave 10）：3 小时完整 monitor 未发现 eligible Open Design
root。#253 已合入；其他已批准后代仍堆叠等待 #228/#254 与各自前沿，不得越序合并。#254 五项
CI 全绿但旧 change request 等待新的 approval；#228 等待基线后的重新批准，其后代继续 dependency-blocked。

P1 的 upstream daemon suite 不是绿色：固定 pin 可重复出现一个
`outdated_cli / incompatible opencode args` 失败，随后停滞，需要 bounded SIGINT。
这是显式基线例外，后续不得把它写成 PASS，也不得让它掩盖新增失败。

## 2026-09-22 Wave 2 门禁

- capability stack：#226 已 merge；#227 的旧审批因 base retarget 被 GitHub 正确作废，
  当前等待新 approval，#228 以后不得越过依赖顺序合并；
- renew：#349～#351 初审发现到期成功返回、时间 anchor 倒退和 CAS 缺口；#355 修复与
  #360 原子回滚/审计/lease-preservation 测试均通过 SOL；
- G1 root service：#359 只建立不可构造的 crate boundary；#363 allocation journal schema、
  #364/#366 SQLite 约束矩阵通过 SOL。它们仍不代表 filesystem allocator 已实现；
- sidecar：#352/#353 frame reader 修复/对抗测试、#362 one-generation actor state policy
  通过 SOL；真实 owned pipes、bounded decoder、launch/ready/control/exit loop 尚未接线；
- design：#354 G1/G2 scratch service plan、#358 durable legacy recovery holds、#361 G8
  actor plan、#365 bounded Launch decoder 均已文档化并通过内部 exact-head review；
- 上述新 PR 均已请求外部 review；除 #226 外尚无新的 external `APPROVED`，因此未合并。

## 2026-09-23 Wave 3 门禁

- #368 将 allocation 类型放入 service crate，违反单向依赖边界；#371 从其父提交重建，
  把 ID、phase 和 failure reason 单一定义在 store。#371 SOL `PASS` 后，#368 作为
  superseded PR 关闭，未改写历史；#373 的严格 row decoder 也已 SOL `PASS`。
- #372 的五处 nullable workspace binding 已通过真实 v8→v9、FK、旧数据、audit chain
  与原 insert path 的 SOL 门禁；#374 的 recovery cohort/hold schema 仍是 dormant
  persistence，不能解释为恢复执行已接通。
- #370 首轮 SOL 阻塞了可伪造的错误字符串分类；新 head 改用 per-decode typed flag，
  200 changed lines，复核 `PASS`。#375 的 env raw-entry 上限也已 SOL `PASS`。
- #374 的复核证明 nullable `TEXT PRIMARY KEY` 可绕过 hold 的 Run/approval FK；该 PR
  已用显式 `NOT NULL` 与 NULL 对抗测试修复并通过 SOL；这仍不等于运行时恢复已接通。
- 截至 00:35，#227 仍 `REVIEW_REQUIRED`；#228/#229 虽已批准但等待父依赖；
  #371～#375 尚无 external approval，本轮没有 merge。
- 进度注册表持续保存在 `.loopx/pr-program/agent24-open-design/`；它是本地控制面状态，
  不作为代码或外部 approval 的替代证据。

## 2026-09-23 Wave 4 门禁

- #377 getter core 与 #380 对抗证据分别通过 SOL：strict decoder、typed errors、合法
  Unix/Windows identity、完整 corruption matrix 与三张业务表只读证明均成立；下一步是
  journal 写事务，不是直接把调用方路径登记为 FS authority。
- #379 的五态 recovery model 与 60-case pure decision matrix 通过 SOL；它只表达持久化
  决策，不提供 approval authority、执行权或启动恢复。
- #378 bounded control reader、#381 resumable writer 与 #382 POSIX owned pipes 均通过
  SOL。host `run()` 仍 inert，G8 不能标为完成；下一切片是 Windows pipes/observe/Job empty。
- 01:48 监控：#227 仍 `REVIEW_REQUIRED`；#228/#229 的 approval 不能越过它；其余本计划
  PR 没有 external `APPROVED`/`REQUEST_CHANGES`，本轮无 merge。
- 200 行仍是硬门禁。#362/#370/#373/#378 均恰好 200 行，#374 为 199、#381 为 195；
  #373/#377/#380 的拆分证明门槛已有真实协调成本。此历史门禁随后已由下文的 500 行弹性规则取代。

## 2026-09-23 Wave 5 门禁

- #227 已在 exact head `f70c11a` 获 external `APPROVED` 且检查全绿后合入 main；#228
  自动 retarget 到 main 后旧审批被正确作废，已重新请求 review，未越过依赖合并。
- sidecar #421/#422 已 external `APPROVED` 且三平台 CI 全绿，但仍分别等待 #417 与 #421；
  #423 exact head `ea14072b` 的 Ubuntu/macOS/Windows 与 CLA 全绿，等待 external review。
- #423 的 pipe-transfer 失败路径保留同一 authoritative owner，并以纯内存 deterministic
  regression 取代会放大全局 reaper 竞态的额外真实进程测试；本地 SOL exact-diff review `PASS`。
- ADR-006 已由项目 owner 的 P0–P9 执行授权正式接受并冻结；G4 可以按小切片继续，但不得把
  Ready 队列读取器解释为 lease、admission 或恢复执行权。
- G1 materialization 初版达到 486 changed lines；审查要求先加入 legacy `create_workspace`
  跨表 ownership 前置保护，并把核心状态转换与对抗性证据拆成相邻 PR，避免突破 500 行或压缩测试。
- #424 以 442 changed lines 加固 `create_workspace` 对 Materialized/Committed/Retained journal
  ownership 的反向守卫，真实双连接 WAL 竞争与两次独立 SOL exact-head review 均 `PASS`；
  #425 以 331 changed lines加入只读 Ready 队列读取器、partial-index plan 证据与同级双审。
- 当前合法 merge 集仍为空：#228/#417 等前沿等待外部审批；监控不得把内部 SOL、绿色 CI
  或下游 approval 当成可越序合并的授权。
- 外部 reviewer 预计处理当前百余 PR 需要较长时间；按 owner 最新指令，完整 PR 状态轮询
  从每 15 分钟降为每 3 小时，期间优先继续实现与开小型、可审查 PR。

## 2026-09-23 Wave 6 收口

- #426 以 407 changed lines 完成 bounded request decode：文本与集合在 owned copy 前受限，
  duplicate/native-equivalent env key 与失败后的 sequence mutation 均被拒绝。macOS CI 两次命中
  已有 global reaper 的 2 秒测试窗口，测试专用窗口改为 10 秒后 Ubuntu/macOS/Windows 全绿，
  两份 SOL exact-head review 均 `PASS`。
- #427 以 499 changed lines 实现 dormant Reserved→Materialized 原子事务：写前 ownership 冲突
  保留 typed `Conflict`，任何写后异常静态化为 `Database` 并整体回滚；157 个 store 测试和两份
  独立 SOL exact-diff review 通过。它不操作文件系统，也不开放 runtime 调用。
- #428 以 273 changed lines 组合 `ControlReader + RequestSequence + ingress state`；首个 framing、
  I/O、protocol 或 sequence 错误只报告一次，之后永久 `Closed` 且不再读取。Windows 首轮 CI
  揭示测试夹具用了 Unix 路径；改为 native absolute path 后三平台 CI 与新 exact-head SOL 全绿。
- 下一批边界已经冻结但尚未开工：G1 先补 materialization 对抗测试再做 commit core；G4 只做
  crate-private Ready→Active DB admission，禁止 runtime wiring；G8 依次做 controlled pipe access、
  graceful stdin close、blocking I/O deadline 与最后的 actor wiring。
- 本轮在三个实现 PR、门禁证据和本地 PR-program 快照都收口后按 owner 指令暂停；恢复后外部
  review 仍按 3 小时 cadence 观察，并从这里继续，暂停期间不启动新的大事务切片。

## 2026-09-23 Wave 9 当前门禁

- #253 已于 2026-09-23 合入 main，merge commit 为
  `bb6f62bad5e75fdf64a375bb4acaec50f1230683`。最新 `origin/main` 与该提交完全一致；变更
  仅增加 desktop sidecar ownership/endpoint-handoff contract
  （`sidecar-contract.ts` 与对应测试），没有改动 store/host core。manager 由 #254 提供。
  integration candidate 的
  Cargo manifest/lock 冲突提示沿用此前审计结论，最终集成仍需保留独立依赖增量。
- #254 已 retarget `main`，head 为 `6fcae9e`；旧 `CHANGES_REQUESTED` 已修复，Typecheck + Test、
  CLA、Rust fmt/clippy/test、Contract Tests + Codegen Drift、Contract (agent24d) 五项 CI 全绿；
  rereview/approval pending。
- #228 已请求 rereview；其后代仍 dependency-blocked，不允许绕过父 PR 或前沿。
- G8 #429 controlled pipe access head `ce9adb0`：Ubuntu/macOS/Windows 三平台 CI 绿；
  #431 graceful stdin close head `65e79bf`：三平台 CI 绿。#435 soft-stop seam 通过
  一份内部 SOL review；external reviews pending。
- G8 #436（head `74587f1`，stack #435，136 changed lines）加入 dormant grace
  observe/force/reap primitive，无 run-loop wiring；Ubuntu/macOS/Windows/CLA green，一份
  SOL `PASS`，external review pending。下一步为 fixed stdout worker。
- G1 #430 是 materialization adversarial slice；#432 是 Materialized→Committed dormant
  core；#433 是其 adversarial test。G4 #434 是 dormant Ready→Active core。精确内部结论为：
  #432/#433/#434 各两份 SOL `PASS`，#435 一份 SOL `PASS`；external reviews 均 pending。
  #432、#433、#434 不接 runtime，也不代表 P2/P9 完成。
- PR 行数规则维持日常目标 200–300、硬上限 500；301–500 行原子例外须两次独立 SOL。
  #430（494，two SOL PASS）、#432（422）、#433（373）、#434（499）是已说明并审查的原子例外。
- 下一步为 G1 registration writer（#432/#433 commit adversarial matrix 已完成）、G4 promotion
  adversarial test 与 G8 fixed stdout worker；#436 grace primitive 正在审查中，再经后续门禁才能进入 runtime。
  不得声称 P2 或 P9 完成。
- complete monitor 请求下，本轮唯一满足依赖并可合并的 PR 是 #253，现已 merged；其他
  approved descendants 仍 stacked，受 #228/#254/frontiers 阻塞，不能合并。

## 2026-09-23 Wave 10 当前门禁

- #253 已 merge 为 `bb6f62b`；#228 等待基线更新后的重新批准。#254 head `6fcae9e` 的五项
  CI 仍全绿，但旧 change request 仍等待新 approval，不能据此合并或解锁后代。
- G4 #437 为 recovery promotion adversarial slice；#440 registration writer（head `0a0a373`，
  421 additions / 1 deletion）依赖 #433，已有两份 Astra `PASS`，但仍是 dormant persistence
  工作，未接 runtime。
- G8 #438 fixed stdout/output worker（head `25999b0`，427 additions / 6 deletions）依赖 #436；
  两份 Astra `PASS`，Ubuntu/macOS/Windows 与 CLA 全绿。它仍未把 host `run()` 或产品路由接线。
- G4 terminal 草稿（+698/-45）经 audit 不可提交；必须拆成 helper、core、adversarial 三个切片。
  Darwin `EPERM` retry 另列切片，不能混入该终态变更。
- 本次 3 小时完整 monitor 没有 eligible Open Design root；绿色 CI、内部审查或 stacked
  descendant approval 都不构成越序 merge 授权。P2 仍为 `IN PROGRESS`。

## A24-OD-00 小 PR 栈

安全实现拆成 15 个可独立 review 的堆叠 PR；每个 PR 的总 changed lines 均小于 200：

- #226 discovery schema
- #227 CLI fail-closed discovery
- #228 token / operation types
- #229 scoped claims
- #230 digest store / watch
- #231 mint authority
- #232 validate / revoke
- #233 authority / token tests
- #236 lifecycle tests
- #237 modular wiring
- #238 private host bootstrap transport
- #239 safe capability startup
- #240 default-deny route policy
- #241 host mint / revoke API
- #242 route isolation tests与保留 namespace

最终栈验证：

- `agent24d`：311 unit + 5 daemon integration + 2 trampoline，全部通过；
- capability authority：14 个测试通过；
- protocol state file：5 个测试通过；
- CLI：35 unit + 3 uninstall integration，全部通过；
- capability ready pipe：只返回一次 64 字节 host bearer；父 pipe EOF 后 bounded clean shutdown；
- capability-mode discovery schema 明确不序列化 token。

## P1 固定基线

- fork：`iDoris-ai/open-design-agent24`；
- integration branch：`integration/agent24-open-design-v0.22.2`；
- exact head：`17c4e86f2177260f5a930e398fa804a0e750a4fc`；
- PR：`https://github.com/iDoris-ai/open-design-agent24/pull/1`；
- 相对 pin 的唯一变更：`docs/agent24-baseline.md`（63 additions）；
- frozen install、daemon build、typecheck、guard、headless health/stop 均通过；
- 未启动 Creative runtime，未开放任何 Agent24 Creative route。

## 当前约束

- P2 期间 Creative 仍只能 `GET /api/v1/models`；
- workspace/session/run/events/approval 均保持 Creative default deny；
- A24-OD-01 与 A24-OD-02 必须共同消费 ADR-002，不能各自发明字段；
- 所有实现继续按功能拆分，PR 日常目标为 200–300 changed lines，硬上限 500；
  301–500 行仅用于不可合理拆分的原子切片，并强制两次独立 SOL exact-head review。

## A24-OD-01 已通过切片

- protocol types：#245（199 行）与 #246（104 行），final `814bb9c`，SOL `PASS`；
- SQLite schema：#248/#249/#250/#251/#252/#256/#257/#258，分别为
  163/172/151/95/103/167/81/165 changed lines；
- schema final：`1167301d67b4940c7a68db64a06ee94454583297`，SOL 无 blocker/high/medium；
- 已验证 canonical timestamp、精确 7 天/90 秒边界、typed counters、真实 v6→v7 upgrade、root identity 与 lease/state 约束；
- store foundation：#263～#274、#276～#277、#280～#281，全部不超过 200 changed lines；
- store final：`f2a6032378107c1e73b6553a6597b4d30f6b5fef`，58 tests，SOL 无 blocker/high/medium；
- 已验证 fail-closed workspace/lease decode、SQLite storage affinity、固定 UTC `Z`/闰秒拒绝、cleanup 状态与 `BEGIN IMMEDIATE`；
- registry models/projection：#286/#287/#288/#289，分别为 132/156/69/95 changed lines；final `6394b50b0b80a21701717ddf1fecaf163fe0d8c0`，63 tests，SOL `PASS`；
- registry CREATE：#293～#298、#304～#305，全部不超过 200 changed lines；final `b8a4621a00e9fce9e53f48d69792ed2ba1d83803`，81 tests，SOL 无 blocker/high/medium/low；
- registry GET：#309～#311、#314、#317，全部不超过 200 changed lines；final `33b2e018a098ce48bb299ff3a40bd17fec52aaaf`，88 tests，SOL 无 blocker/high/medium/low；
- registry LIST：#318～#321、#326～#327，全部不超过 200 changed lines；final `c5c7b9519dae8f1ff05fd1757aaa18091ce6f038`，103 tests，SOL 无 blocker/high/medium/low；
- DB-only expiry/release-request：#329/#330/#334～#339，分别为 35/181/99/167/67/30/174/137 changed lines；final `e85b0c539f4e12cb5f849039d5298f2b0c22d3c6`，114 tests，SOL 无 blocker/high/medium/low；
- 该切片只完成事务性状态与审计，不代表真实文件清理；quarantine/delete、lease drain、cleanup retry/completion、run/session 字段仍不在本波。下一切片设计 `renew_workspace`。

## A24-OD-05 已通过基础切片

- sidecar contract/manager 栈：#253 为 ownership/endpoint-handoff contract；manager 切片
  #254/#255/#259/#260/#261 的初始历史行数为 174/141/173/89/151；当前 #254 head
  `6fcae9e` 经修复后为 373 additions（live PR），不能把历史初始行数当作当前规模；
- foundation final：`a3b83fb22e8559319fe8ed677e9cbc40a64258ac`，SOL 无 blocker/high/medium；
- 110/110 desktop tests、完整 desktop typecheck 与 exact-head diff check 通过；
- private host protocol：#275/#278/#279/#282/#283/#284/#285/#290，分别为 159/129/80/84/141/108/145/75 changed lines；final `21ecfe7759e3e6d90daf1132095bbff337eb7f23`，SOL `PASS`；
- POSIX generation owner：#291/#292/#302/#303/#306/#312/#315/#313，全部不超过 200 changed lines；final `247b355e0d9c693d6f580f9372c7f16eaab2fe2e`，9 tests，SOL `PASS`；
- Windows Job owner/validation：#299/#300/#307/#308/#316，全部不超过 200 changed lines；final `b8cfc3ddc2054953688aa94dc096e1eab08f8f28`，Windows Server 2025 上 5 owner + 6 protocol tests，SOL `PASS`；
- common owner base：#322/#323/#324/#325，分别为 84/65/164/60 changed lines；final `df84c16c9d0ac19b7ee91e87b3060fcb70a54fd3`，macOS 与 Windows 双平台 SOL `PASS`；
- bounded actor codec：#328/#340，分别为 105/36 changed lines；final `c5b83201a06cd394ac22f2fb323bbb1be5326479`，17 tests，SOL `PASS`；编码帧（含换行）与实际 allocation capacity 均受 65,536-byte 上限约束，分配失败不返回部分输出且不推进 request sequence；
- foundation 仍未接入 renderer、IPC 或产品路由；
- 下一门禁是 allocation-bounded stdio actor、ready/exit supervision 与 bounded graceful→forced shutdown；通过前仍不得产品接线。
