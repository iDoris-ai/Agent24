# Agent24 × Open Design 执行状态

> 更新：2026-09-23（Asia/Bangkok）

## 阶段门禁

| 阶段 | 状态 | 证据 |
| --- | --- | --- |
| P0 设计冻结 | PASS | ADR-001～005、依赖台账、风险与兼容矩阵已冻结并通过 SOL review |
| A24-OD-00 capability 安全前置 | PASS（逐层合并中） | #226 已合入 main；#227 retarget 后等待重新审批，其余保持堆叠顺序 |
| P1 Open Design 原样基线 | PASS | `open-design-v0.22.2@73953213a`，fork PR #1，SOL exact-head 复核通过 |
| P2 workspace contract | IN PROGRESS | DB registry/lifecycle/renew 与 allocation journal schema/约束通过 SOL；安全 root allocator/host lease/run binding 未完成 |
| A24-OD-05 sidecar foundation | PASS（未接线） | manager、protocol、platform owner、codec、frame reader 与 actor state policy 通过 SOL；owned pipes/I/O 未完成 |

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
  #373/#377/#380 的拆分证明门槛已有真实协调成本。弹性规则已提议但尚未获用户确认。

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
- 所有实现继续按功能拆分，PR 默认不超过约 200 changed lines。

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

- sidecar manager：#253/#254/#255/#259/#260/#261，分别为
  97/174/141/173/89/151 changed lines；
- foundation final：`a3b83fb22e8559319fe8ed677e9cbc40a64258ac`，SOL 无 blocker/high/medium；
- 110/110 desktop tests、完整 desktop typecheck 与 exact-head diff check 通过；
- private host protocol：#275/#278/#279/#282/#283/#284/#285/#290，分别为 159/129/80/84/141/108/145/75 changed lines；final `21ecfe7759e3e6d90daf1132095bbff337eb7f23`，SOL `PASS`；
- POSIX generation owner：#291/#292/#302/#303/#306/#312/#315/#313，全部不超过 200 changed lines；final `247b355e0d9c693d6f580f9372c7f16eaab2fe2e`，9 tests，SOL `PASS`；
- Windows Job owner/validation：#299/#300/#307/#308/#316，全部不超过 200 changed lines；final `b8cfc3ddc2054953688aa94dc096e1eab08f8f28`，Windows Server 2025 上 5 owner + 6 protocol tests，SOL `PASS`；
- common owner base：#322/#323/#324/#325，分别为 84/65/164/60 changed lines；final `df84c16c9d0ac19b7ee91e87b3060fcb70a54fd3`，macOS 与 Windows 双平台 SOL `PASS`；
- bounded actor codec：#328/#340，分别为 105/36 changed lines；final `c5b83201a06cd394ac22f2fb323bbb1be5326479`，17 tests，SOL `PASS`；编码帧（含换行）与实际 allocation capacity 均受 65,536-byte 上限约束，分配失败不返回部分输出且不推进 request sequence；
- foundation 仍未接入 renderer、IPC 或产品路由；
- 下一门禁是 allocation-bounded stdio actor、ready/exit supervision 与 bounded graceful→forced shutdown；通过前仍不得产品接线。
