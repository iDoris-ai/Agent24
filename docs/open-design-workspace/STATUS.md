# Agent24 × Open Design 执行状态

> 更新：2026-09-20（Asia/Bangkok）

## 阶段门禁

| 阶段 | 状态 | 证据 |
| --- | --- | --- |
| P0 设计冻结 | PASS | ADR-001～005、依赖台账、风险与兼容矩阵已冻结并通过 SOL review |
| A24-OD-00 capability 安全前置 | PASS（待逐层合并） | SOL 无 blocker/high；全量 Agent24/协议/CLI 测试及私有 ready-pipe 烟测通过 |
| P1 Open Design 原样基线 | PASS | `open-design-v0.22.2@73953213a`，fork PR #1，SOL exact-head 复核通过 |
| P2 workspace contract | IN PROGRESS | A24-OD-01 types、schema、store foundation 与 registry models 已通过 SOL；CREATE 实现中 |
| A24-OD-05 sidecar foundation | PASS（未接线） | manager foundation `a3b83fb` + host protocol `21ecfe7` 均通过 SOL exact-head 复核；平台 owner 实现中 |

P1 的 upstream daemon suite 不是绿色：固定 pin 可重复出现一个
`outdated_cli / incompatible opencode args` 失败，随后停滞，需要 bounded SIGINT。
这是显式基线例外，后续不得把它写成 PASS，也不得让它掩盖新增失败。

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
- 下一切片只实现 registry CREATE；get/list 分批跟进，不提前加入 A24-OD-02 的 run/session 字段。

## A24-OD-05 已通过基础切片

- sidecar manager：#253/#254/#255/#259/#260/#261，分别为
  97/174/141/173/89/151 changed lines；
- foundation final：`a3b83fb22e8559319fe8ed677e9cbc40a64258ac`，SOL 无 blocker/high/medium；
- 110/110 desktop tests、完整 desktop typecheck 与 exact-head diff check 通过；
- private host protocol：#275/#278/#279/#282/#283/#284/#285/#290，分别为 159/129/80/84/141/108/145/75 changed lines；final `21ecfe7759e3e6d90daf1132095bbff337eb7f23`，SOL `PASS`；
- foundation 仍未接入 renderer、IPC 或产品路由；
- POSIX generation-pinned controller 与 Windows Job Object（或等价控制器）是产品接线前硬门禁，必须作为后续独立小 PR 实现。
