# Hyphae 健康、兼容晋升与可逆回滚契约

状态：2026-10-11，DEP-HN0 冻结策略提案，**待跨仓配套 follow-up 评审，非 DONE**。本文所有阈值、状态和 manifest 均为规范目标，不是当前能力；双方互链 PR 合并后才完成契约冻结。具体候选版本的证据在 DEP-HN1/C8 晋升时填入，不推迟本文策略决定。全部实现仍等待 Hyphae 当前 M1 发布门关闭；本任务不发布 Release/OCI。

依据：[网络交付契约](HYPHAE-NETWORK-DELIVERY.md)、[唯一任务台账](TASKS.md#hyphae-delivery)、[COMM 历史设计](../design/COMM-HYPHAE.md)。LAN/公网访问控制、TLS 和服务端暴露是独立安全设计门；本文通过不能解除该门。保持 exact-hash、zero-run、私钥仅本地、显式监听授权、无 UPnP/防火墙修改/TLS 绕过。

## 1. 可复核的当前事实

Agent24 实查 `ab125de262827bd0f1c6680b427d9c241ccff785`。Hyphae 在 `/Users/jason/Dev/iDoris/Hyphae` 只执行 fetch 并读取 `origin/main` 对象，实查 SHA `1df9c6707c4df956bfc8868509bf33c1925bac2d`（含已合并 #172/#173）；没有切换或修改该 checkout。以下 Hyphae 相对源码路径均相对此仓，评审须按该 SHA 读取，不能拿移动中的 main 证明候选兼容。

- [binary.rs](../../rust/crates/agent24-comm/src/binary.rs) 的 `HyphaeLock::embedded/expected_for` 每平台只有一个 client hash；`VerifiedBinary::install` 全量 hash 后 O_EXCL 写最终副本。旧文件存在、版本号相同或曾运行成功均不授权旧 hash。
- [daemon.rs](../../rust/crates/agent24-comm/src/daemon.rs) 的 `READY_AFTER_DEFAULT=3s` 只将存活进程标为 running；[restart_policy.rs](../../rust/crates/agent24-comm/src/restart_policy.rs) 为 0.5/1/2/4s 退避、第 5 次失败熔断、存活 60s 重置。它们都不是协议或数据健康证据。stop 使用受管进程组、身份检查、TERM/KILL 与退出确认，宽限来自 daemon 生命周期配置。
- [comm_routes.rs](../../rust/apps/agent24d/src/comm_routes.rs) 仍按 embedded lock 选择 client；[router.rs](../../rust/crates/agent24-comm/src/router.rs) 手动 relay probe 调 `relay info --timeout 5`。`connected` 是一次 WebSocket 连接结果，不是投递、解密或补收完成；`catch_up` 保留 unknown/incomplete。
- Hyphae `cmd/hyphae-relay/main.go` 默认 `127.0.0.1:3334`，`--data-dir` 默认 `build/relay-data`；“listening”打印发生在 `ListenAndServe` 之前，不能作为绑定成功证据。`internal/relay/config.go` 的 `--public` 许可不等于 TLS/访问控制方案；没有本文要求的结构化 readiness/maintenance 接口。

| 持久对象 / 实际源码 | 实查格式与风险（不认证版本间兼容） |
|---|---|
| `.hyphae/keystore.json`；`pkg/types/types.go`、`internal/identity/keystore.go` | `default_identity/identities/contacts/encrypted/salt/verification/password_hint`，identity 内含 nsec；**没有持久 schema/version 字段**。`StoreVersion` 是文件字节 SHA 的内存 CAS 标记，`json:"-"`，不是 schema。当前上游有 `keystore.json.lock` 和 CAS；COMM 历史“无锁”不应当作此次上游事实。Agent24 必须仍要求加密。 |
| `.hyphae/relays.json`；`internal/relayconfig/config.go` | `{version:1,relays:[...]}`，读路径拒绝非 1；缺文件会走上游公共默认值，Agent24 必须阻止隐式回落。此 version 仅属于 relay 配置。 |
| `.hyphae/outbox.json`；`pkg/types/types.go`、`internal/messaging/outbox*.go` | `{entries:[...]}`，**无顶层版本号**；含 queue_id、route、id、已签名 event_json、relays、重试计数/时间/status；DM 与 group 路由/状态不同，legacy 字段可缺失。兄弟 `.lock` 串行化读改写；不能仅因 JSON 能反序列化就认定旧版不会误发群消息或丢字段。 |
| `.hyphae/messages.db`；`internal/storage/db.go` | SQLite WAL；messages 的 id 主键、event_id 唯一，含密文/明文、双方 npub、时间、方向/加密标记、relay/kind 及索引。InitDB 会建表；messages.json 导入后 rename 为 `.backup`。实查未发现由 Hyphae 管理的统一 `user_version/schema_version` 迁移序列，不把 SQLite 自身 schema cookie 当产品版本。 |
| 同一 SQLite；`internal/groupchat/store.go`、`fanout.go`、`fanout_queue.go` | groupchat_groups/members/invites/messages/fanout，包含 roster、邀请状态、logical_id/event_id/queue_id、签名 fanout 事件与重试状态。`migrateFanoutAccounting` 通过 table_info/ALTER 增列，并回填 legacy accounting；retry_count=NULL 表示未知，不能变为 0。与 outbox 跨文件关联，必须一致备份。 |
| 同一 SQLite 的其他表；`internal/group/db.go`、`internal/profile/db.go`、`internal/audit/audit.go` | 旧 groups/group_members/group_messages（含 role 增列）、agent_profiles、audit_log 也可能存在；不能只备份 messages 或只数聊天条数。保留完整 DB、审计链和未知对象。 |
| relay `<data-dir>/events.db`；`internal/relay/server.go`、`go.mod/go.sum` | Khatru + `fiatjaf.com/nostr/eventstore/boltdb`，不是 SQLite。固定依赖 `v0.0.0-20260928115942-58e4c715304e` 的本地模块源码 `boltdb/lib.go,migration.go` 有 settingsStore/rawEventStore/索引桶，内部 migration target=1，settingsStore 的 version 可缺省，注释无 rollback。**这是依赖内部标记，不是 Hyphae relay schema v1 或兼容保证**；必须绑定依赖和实际桶/编码证据。 |

上述是静态源码观察；未运行候选迁移或兼容测试，未读取用户真实私钥/消息库。该观察时 Agent24 生产 lock 的 Hyphae source 为 `671c584f9e9eb807a15968e2aa42fd7507e178b8`（历史值），与实查上游不同；二者兼容性为 **unknown**。2026-10-24 生产 lock 已升到 Hyphae v0.27.0 `9126147e8eb5e497e3055f9eda27473fefbb91b2`，见 [LOCK-UPGRADE-20261024](../comm/LOCK-UPGRADE-20261024.md)；上述静态观察没有对新 source 重做，兼容性结论仍为 **unknown**。

## 2. 健康分层与固定时限

所有秒数使用单调时钟、从阶段进入起算，包括子命令/重试/退避；父期限更早时取最小值，超时不能重新计时。每份结果绑定 operation、generation、component、完整 binary hash、配置摘要、时间、耗时和脱敏 reason。重启/改配置使旧证据失效；没有实现探针就报 unknown，不以日志猜测成功。

| 层次 / 对象 | 必须满足的谓词 | 冻结上限与失败归类 |
|---|---|---|
| process_alive（client/relay） | 被验证的 PID/启动身份/进程组仍存在；只说明存活 | 每次读取 2s；不可访问报 unknown，不能据此标 ready |
| client local_ready | 精确 hash/授权 tuple；HOME 与配置正确；仅 stdin 解锁成功；生命周期锁持有；持久对象可读且匹配晋升证据；事件循环响应本地 challenge；无意外监听 | spawn 后 15s；单次 challenge 5s。缺凭据为 locked；格式不符为 compatibility_blocked；本地崩溃/死锁为 local_fault |
| relay local_ready | 精确 hash/tuple；只有已授权地址/端口的监听且属于该进程；指定数据目录独占打开；存储完整性与已知事件读取成立；本地协议请求可响应 | spawn 后 20s；单次请求 5s。仅端口打开/HTTP 200/打印 listening 均不充分；冲突为 blocked，不杀外部服务 |
| relay_reachable（每个配置端点） | 对精确端点完成 WS（跨机 WSS 完整 TLS 校验）及协议查询，记录端点结果 | 每端点 5s，一轮并行总计 10s；失败为 network_degraded/TLS_error，不证明本地 binary 坏 |
| synthetic_encrypted_exchange | 隔离 A/B 合成身份 A→B 解密校验 nonce/event ID，再 B→A 加密回复并解密校验；对应存储记录恰一次、无未知 queue 状态；relay ACK 仅是中间证据 | 每次完整双向 20s，最多 2 次，间隔 5s，总计 45s；重试复用原签名事件/ID，不能生成新消息掩盖失败 |
| 启动/更新综合健康 | 先 relay（如受管），再 client local_ready，再合成往返；各层独立输出，不以一项覆盖另一项 | 总计 90s；远端不可达结束为 installed_degraded、exchange=unknown；未配置获授权探测对端时必须在 prepare 前告知并阻断更新，不能切换后才发现缺少探测条件 |
| 正常运行健康 | 本地响应与 relay 可达每 30s 一轮；三轮连续本地失败才报 local_unhealthy，成功清该计数；进程退出立即报告 | 每轮至多 10s，最迟首次失败后 70s 可见；外网失败只更新网络维度，不计本地重启失败数。合成往返仅首启/更新/用户触发，不暗发周期消息 |
| stop（client/relay，各自） | 冻结新请求，停所有受管写入进程；验证身份后 TERM 5s，未退出再 KILL；确认整个组消失、回收和释放锁 | 总计 10s/组件，组合顺序停 client 再 relay ≤20s；未确认则 stop_unconfirmed，禁止替换/恢复数据或启动另一代 |

探测 identity、HOME、消息记录须与用户会话隔离，使用真实已验证组件；不泄露任何私钥。本机 relay 探测走已授权监听，测试事件专门隔离；客户端加入外部网络只用管理员允许的合成对端，无授权就 unknown，**不得为探测下载/拉起 relay 或新增监听**。运行时可暂缺外部往返，但 C8/HN6 必须在受控测试环境证明真实加密往返与 relay 重启后的持久读取；不得把本地自测当作网络往返。

只有可归因于候选的本地故障，或健康受控对端已独立确认且重复证明候选协议/解密错误，才进入本次更新的自动回滚判定。外部 DNS/断网/relay 停机/TLS 错误、缺口令、外部端口占用、未知故障均不允许破坏性数据恢复；保留本地健康候选、原备份和原因，必要时停在 blocked。稳定运行后不因周期健康失败自动恢复旧快照；已有业务写入时恢复会丢数据，必须另作恢复计划。

## 3. 受信任兼容清单与准入矩阵

Agent24 管理的组件必须由可信 Agent24 发行内嵌独立 client/relay pins 及组合准入清单；标准、外部 binary 路径和离线包同规则。独立服务器 relay 不依赖 Agent24 build：运营者须通过预先信任的签名根或独立固定的预期 hash 验证资产，并使用绑定 relay/source/存储依赖及迁移证据的服务器兼容清单，不能把同包 checksum 当信任根。本文 Agent24 tuple 与自动回滚权限仅适用于 Agent24 受管组件；服务器流程采用相同数据安全规则但由运营者独立授权。源 SHA/版本号/同包 checksum/本地 marker 均不能授权执行。签名根目前未建立：冻结采用独立 pin 路径；未来若声明签名，须预先经评审固定 root identity 并额外验签，不接受随资产下发的新根。

tuple 精确绑定 Agent24 build、平台、client binary/lock、relay binary/lock（client-only 明确 null）与配置约束；client/relay 各自升级不意味着任意笛卡尔积可运行。当前 Agent24 build 必须同时授权 candidate/current/previous 的**完整 tuple 和有向转换边**，旧 Agent24 发行曾接受过不够。每次 spawn 均重验 hash，不能绕开 VerifiedBinary 或开生产 override；单一 embedded lock 在 HN4 完成此能力前禁止自动旧 hash 回滚。

下面为可机器解析的**目标兼容 manifest 阻断示例**，不是现有文件格式/有效授权。null、空证据或 unknown 一律拒绝晋升与迁移；`manifest_schema` 只版本化本清单，不给数据杜撰版本。实际 pins 必须为完整 SHA-256：256 位，编码为恰好 64 个十六进制字符。source/Agent24 commit 为完整 Git SHA。填入真值也须经过评审，不自动变成 trusted。

```json
{
  "manifest_schema": "agent24.hyphae.compatibility/1",
  "status": "blocked_example",
  "agent24_commit": "ab125de262827bd0f1c6680b427d9c241ccff785",
  "platform": "darwin-arm64",
  "trust": {"mode": "embedded_independent_pins", "signing_root": null},
  "pins": {
    "client_current": {"source_sha": "9126147e8eb5e497e3055f9eda27473fefbb91b2", "lock_sha256": null, "archive_sha256": null, "binary_sha256": null},
    "client_candidate": {"source_sha": "1df9c6707c4df956bfc8868509bf33c1925bac2d", "lock_sha256": null, "archive_sha256": null, "binary_sha256": null},
    "client_previous": {"source_sha": null, "lock_sha256": null, "archive_sha256": null, "binary_sha256": null},
    "relay_current": {"source_sha": null, "lock_sha256": null, "archive_sha256": null, "binary_sha256": null},
    "relay_candidate": {"source_sha": null, "lock_sha256": null, "archive_sha256": null, "binary_sha256": null},
    "relay_previous": {"source_sha": null, "lock_sha256": null, "archive_sha256": null, "binary_sha256": null}
  },
  "tuples": {
    "current": {"client": "client_current", "relay": "relay_current"},
    "candidate": {"client": "client_candidate", "relay": "relay_candidate"},
    "previous": {"client": "client_previous", "relay": "relay_previous"}
  },
  "accepted_tuples": [],
  "transitions": [{"from": "current", "to": "candidate", "rollback_to": "current", "decision": "blocked", "migration_id": null, "evidence_sha256": null}],
  "stores": [
    {"id": "keystore", "observed_version": null, "compatibility": "unknown"},
    {"id": "relays", "observed_version": 1, "compatibility": "unknown"},
    {"id": "outbox", "observed_version": null, "compatibility": "unknown"},
    {"id": "messages_sqlite_all_tables", "observed_version": null, "compatibility": "unknown"},
    {"id": "relay_boltdb", "observed_version": null, "compatibility": "unknown"}
  ],
  "migration": {"id": null, "implementation_sha256": null, "input_fingerprint_sha256": null, "output_fingerprint_sha256": null, "reversible": "unknown", "backup_required": true},
  "backup_policy": {"scope": "all_affected_stores_consistent", "restore_test_required": true, "retain_known_good": 1},
  "transaction_binding_required": ["manifest_sha256", "operation_id", "generation", "candidate_tuple", "current_tuple", "previous_tuple", "migration_id", "consent_sha256", "backup_id", "backup_inventory_sha256", "backup_verified", "restore_verified", "phase"],
  "evidence": {"forward": null, "backward": null, "unchanged_schema": null, "backup_restore": null, "encrypted_exchange": null, "zero_run": null}
}
```

有效清单还须引用网络契约 §4 的每平台制品元数据（完整摘要绑定 URL、大小、配方、工具链、最低系统版本、许可证/来源）；只知源码 SHA 不足以生成 pin。每对象兼容证据记录起止布局/语义指纹、reader/writer 方向、迁移实现摘要、测试 fixture 与命令/退出码/CI 证据摘要，依赖版本也计入。没有迁移也须给出绑定 unchanged-schema 证据摘要的 `migration_id=none:<sha256>`，不能以 null 表示“已证明无需迁移”。runtime 事务把清单摘要与实际备份清单、恢复演练结果绑定；不能把 `backup_verified:true` 自填字段当验证。

| 转换（C=client，R=relay，A=当前 Agent24 build） | 允许条件 / 否则阻断 |
|---|---|
| 同 tuple 重装/重启 | 完整 pins 重验、数据布局与该 tuple 的证据匹配、无未知写入者；这不是跨版本兼容认证 |
| `(C0,R0)→(C1,R0)` 或 `(C0,R1)` | A 明确接受目标 tuple/有向边，更新组件及未变组件的协议/存储交互测试通过；单个新 pin 不够 |
| `(C0,R0)→(C1,R1)` | 联合晋升与一致迁移/恢复演练通过；中间混合 tuple 未获授权就不得启动 |
| 新 client-only / 从无 relay 到有 relay | 清单明确 null/目标 relay 与配置边；新增安装、启动、监听另获同意；不能从 client pin 推导 relay pin |
| 宣称 unchanged-schema | 必须有两 SHA 的 serializer/DDL/迁移/依赖差异审查、布局指纹与双向读写后重启测试，覆盖未知字段、群 fanout、去重与重试语义；DDL 相同或打开成功不够；仍备份 |
| 已知可逆迁移 | 精确 migration id/实现摘要、输入/输出指纹和失败中断演练通过，备份恢复已验证；在副本执行，拒绝隐式未登记迁移 |
| 不可逆迁移 | 仅在旧 tuple 对迁移前一致备份的恢复演练通过、隔离无业务写入、额外明确同意时允许；“不可逆”不免除可恢复性；否则阻断 |
| unknown/缺证据/未知布局或依赖 | 阻断迁移与候选打开真实数据；同意也不绕过；保留 current，不自动初始化空库 |
| candidate→current 或 current→previous | 当前 A 明确接受回滚 tuple/有向边且完整 pins 匹配；旧读写器通过兼容测试可用当前数据，否则仅恢复绑定的迁移前快照且符合 §4 无新业务写入条件 |
| 任意旧 hash、旧清单重放、previous 不受 A 信任 | 拒绝；不得退回旧锁文件或放宽 exact-hash；组件无法回滚时保留数据并停机诊断 |

本次不把任何不同版本的转换填成已通过。上述固定决策规则现在冻结；精确 candidate hash、每平台真实资产、输入布局指纹及迁移测试结果属于发布晋升门，不是留空策略的理由。

## 4. 有界、可逆的更新事务

一次 operation 持跨进程锁，拒绝并发升级/改配置/导入/写入；同意绑定完整 tuple、数据范围、停机、migration id、自动回滚范围与监听约束。持久日志每次转移 fsync，记录 §3 的绑定字段和阶段结果，目录 0700、文件 0600；秘密不得入日志。计划读取不执行候选。磁盘必须容纳完整备份、迁移副本、恢复副本及两代 binary，否则在停机前拒绝。

1. **prepare**：下载至多 600s、校验/安装 staging 至多 60s，总计 660s；先验包再验 binary，安全解包，同文件系统 fsync/不覆盖提交。失败/取消保留 active。此时只安装，不切 active、不迁移。
2. **quiesce**：停止 client、relay（仅受影响者），组件各 10s、组合 20s；阻止所有会写入受影响本地存储的 daemon/CLI/UI/outbox retry/群 fanout/外部本机服务。仅受管 relay 才冻结其写入；client-only 更新不得要求停止或恢复不归 Agent24 管理的外部 relay。拿到 outbox.lock 不证明 SQLite 或其他写入者已停；锁持有关系须由 Hyphae 支持。停不净就停止事务，不备份、不切换。
3. **backup_verified**：至多 120s。全量 client HOME 持久数据与受影响 relay data-dir 一致快照；包括 keystore、relays、outbox、messages.db 及 WAL、legacy messages.json/.backup、其他持久配置与未知文件。SQLite 先一致 checkpoint/关闭，不能活拷 DB 漏 WAL；relay 正常关库后完整复制 events.db/配置，不套 SQLite 恢复命令。锁/PID 是运行元数据，记录但不当活锁/PID 恢复。发现未知文件先保全，再阻断未说明的迁移。
4. **restore rehearsal / migrate**：backup 阶段的 120s 内，在另一受保护目录核对所有文件大小/完整 hash/权限，恢复并用受信任旧组合做不联网的读校验与 SQLite integrity_check/外键、relay 后端完整性/事件读取，形成 restore_verified。随后迁移阶段至多 120s，仅在副本执行已登记 migration；成功后验证输出指纹、语义不变量并 fsync。大库超时保留 current，走另行评审的离线维护计划，不静默延长期限。
5. **switch / check**：原子切换 tuple+数据代际指针至多 10s；跨目录不能假称一个 rename 原子完成，日志加唯一提交记录决定恢复方向，完成前禁止启动混合代际。按 §2 至多 90s 检查，期间禁止受影响本地组件接受/发送真实业务，含后台补收、重试以及受管 relay 的外部写入；只允许隔离合成流量。外部 relay 可持续接收其他节点事件：在本机事务内持久保存补收边界和已签名待发 event ID，commit/rollback 后用原 ID 去重恢复补收与重试；不恢复外部 relay 快照、不撤销已发事件。relay 无法证明保存完整历史时明确报告潜在补收缺口，不能声称消息零丢失。缺 maintenance/隔离能力则本升级路径 blocked，不以当前 daemon 直接试跑用户库。
6. **commit**：本地健康、合成往返通过才记 healthy；仅网络失败可提交 installed_degraded 并解封业务，保留 exchange=unknown 与旧备份，不能把它晋升为新的 known-good。一旦解封真实业务，旧快照自动恢复窗口关闭；后续降级须新计划，不能抹掉已接收事件/已发送记账。
7. **rollback**：只在该 operation 的预授权范围且无新业务写入时执行一次。先停候选并确认退出，再核对旧 tuple 仍被当前清单允许，隔离失败数据，恢复绑定快照到新目录、验证后切指针，再以旧 tuple 检查；有可验证双向读写证据的 unchanged-schema 可复用数据。恢复缺失/损坏、权限变化、布局不匹配或停止未确认均 fail-closed；不覆盖唯一备份、不重建空库、不循环切换。

从 quiesce 开始正常路径总期限 **420s**（包含上述子阶段和记账）；超时仅在符合回滚条件时进入 rollback。rollback 独立总期限 **300s**：停止 ≤20s、恢复及验证 ≤120s、切换 ≤10s、健康 ≤90s，余量只用于日志/清理；一次 operation 最长 **1380s=660+420+300**。期限耗尽报告 failed/stop_unconfirmed 并保留两代数据和日志，不能宣称恢复成功；无法证明写入者已停时绝不强行切数据。重启读取原阶段/剩余预算，无法重建安全期限则进入受限恢复，不重置自动重试次数。

一致性验收须同时比对身份/默认身份/联系人、relay 配置、消息 ID/内容摘要/方向/去重、群 roster/邀请/logical_id、fanout event/queue ID/状态/NULL 重试语义、outbox 签名原文与状态、审计链、relay 事件集合；不仅比较文件数或聊天条数。恢复后绝不重新签名重发“可能已发出”的事件。私钥与备份始终本地受保护；口令仍来自 OS 凭据库并只经 stdin，备份验证不得导出明文私钥或口令。

崩溃后按日志识别 prepared/backup_verified/migrated/switched/committed：未提交就保留原数据并恢复获授权组合；已解封或有未知写入则阻断快照回退，保全双方数据等待人工恢复计划。无论正常/降级提交，至少保留一份已验证 known-good tuple、其独立 pins 与一致备份；只有用户确认稳定且存在另一个已验证恢复点才可清理，不能因“满 60s 存活”清理。

## 5. 每 task 的负例与可观察出口

各实现 task 先记录反面对照再修复。以下是未来必须执行的测试，本文静态核查不冒充运行通过；证据绑定精确两仓 SHA、平台、完整资产/清单 hash、命令/退出码、时间线及脱敏结果。

| Task / 责任 | 必须注入的负例 | 可观察验收 |
|---|---|---|
| HN0 / 双仓 | 把三秒存活/握手当健康；把未知 schema 当兼容；把目标 JSON 当有效授权 | 双仓互链逐条审查，状态保持待配套评审至双方合并；阈值/矩阵无“实现时再定”；目标示例可解析且阻断 |
| HN1 / Hyphae | relay 打印 listening 但绑定失败；库锁/坏库；假 ACK 无持久事件；探针卡死 | 结构化本地就绪/维护隔离/停机能力满足 15/20/10s 界限；重启读回同事件；四平台候选与存储/依赖证据齐全，否则不交给 C8 |
| C8 / Agent24 晋升 | unknown 输入；仅 DDL 相同但丢 route/queue_id；旧版改写 group_pending；NULL retry_count 变 0；混搭 tuple | 双向读写/重启与迁移/恢复 fixture 覆盖全部持久对象；零隐式公共回落；拒绝无证据的边，保留生产锁；真实加密往返与 zero-run 正反对照 |
| HN2 / 安装器 | 坏 pin、半文件、归档逃逸、满盘、fsync/rename 中断、并发操作 | 不执行未验证文件、不覆盖异常目标、active hash 不变；每个提交边界故障注入可恢复 |
| C9 / 标准包装 | 错平台/null pin、PATH 注入、打入未授权 relay | 干净四平台仅执行内置精确 client；无源码/Go/全局 binary 依赖，无新增监听 |
| HN3 / 管理 API | 无 token/同意、过期 generation、候选 hash 偷换、旧清单重放、备份 marker 伪造 | 拒绝且数据/监听不变；operation 幂等，阶段、tuple/hash、deadline、reason 可查询且无秘密 |
| HN4 / 监管事务 | 活着但死锁；断网/TLS 失败；候选解密错误；SIGTERM 不退出/PID 复用；坏备份、WAL 未落盘、群 outbox 与 DB 不一致；迁移每阶段崩溃 | 本地故障与网络降级分开；纯断网不恢复旧数据；stop 未确认无切换；只回滚受信任完整组合一次；420/300/1380s 界限可测；恢复不丢身份/历史/待发，解封后拒绝自动快照回退 |
| HN5 / UI/CLI | running/ACK 冒充端到端成功；unknown 探测结果；未获 LAN/TLS 许可 | 独立显示本地/网络/往返状态、最后探测时间与原因；degraded 不显示全绿；无合成对端不悄开本机节点；扩大暴露保持禁用 |
| HN6 / 双仓验收 | 离线升级、无旧 hash 授权、relay 持久卷损坏、回滚再次失败、稳定提交后有人写入 | 真实 Release 四平台 smoke、指定干净机器全流程和服务器卷演练；失败保留诊断/数据；runs/model/module Δ=0 且同量具正对照有效；未经独立安全门不宣称 LAN/公网交付 |

## 6. Hyphae 配套文档的精简补丁指引

协调者在 Hyphae `docs/agent/hyphae-agent24-distribution-install-update.md` 开配套文档 PR；本工作树不修改上游，不推送或开 PR。

1. §2/§6 标明当前仅三秒进程存活/手动 WS 探测和单一 client lock；§6.1/6.2 用本文 §2/§4 的有界事务取代“保留数据、换回旧 binary”概述，强调外网失败不触发破坏性回滚，未确认停机不能换数据。
2. §10.5 引用本文 §1/§3 的真实存储观察、独立 pins、完整 tuple/有向边、unknown 阻断与机器清单；明确 Bolt 依赖内部 version 不构成 Hyphae schema 认证，禁止虚构统一 schema 版本。
3. §10.6 互链本次 Agent24 follow-up PR 与本文稳定路径；§10.7/10.9 的 HN0 状态为“待跨仓配套 follow-up 评审”，双方最终 head 审批与合并后才 DONE。精确候选兼容证据归 HN1/C8 晋升门，策略现在固定。
4. §10.7/10.8 同步本文 §5 责任/负例/可观察时限和一致恢复验收；保留 M1 阻塞全部实现、LAN/公网 ACL/TLS 独立安全门、签名根未建立不得声称验签，以及人控 Release/OCI 出口。

未关闭的安全门：双仓配套审查、M1、HN1/C8 精确版本证据、HN4 受信任旧 tuple/结构化健康/隔离迁移与恢复实现、LAN/服务器访问控制与 TLS 设计。它们不允许通过文档状态、示例清单或用户确认跳过。
