# CORE-5：双库迁移保护与停靠恢复（设计说明）

> 2026-10-27。代码基线 `b72fdd33949476a146388ebf490dcb80b65537d6`。
> 只写设计，不改代码，不批准实现或发布。代码事实及文件行号见 [CORE-5 事实清单](../research/CORE-5-FACTS-20261027.md)，下文 F* 均指该清单。
> 未注明“现状”的路径、字段、状态与 Operation 均为设计。CORE-2 以§10 修订为准；本稿不重做在场证明。

## 1. 背景与现状

| # | 现状 | 目标 | 证据 |
|---|---|---|---|
| V1 | 主库先迁移，memory 失败会降级继续 | 双库作为一次升级，任何失败都整体恢复 | F7、F8、F13–F18；ADR-037 F1 |
| V2 | sqlx 每个脚本有事务，整串和双库没有共同事务 | 保护已提交的前序迁移及 FTS rebuild | F14–F19 |
| V3 | daemon 单例不能拦住 CLI 直接开库 | 所有合作入口停写后再做双库快照 | F6、F10、F12 |
| V4 | health 恒 ok；桌面与 CLI 只判 HTTP 成功 | 停靠可发现，不能复用，也不能自动另起实例 | F22–F24 |
| V5 | LaunchAgent 失败退出重启；systemd 未有模板 | 停靠存活，不进入迁移重启循环 | F25、F26 |
| V6 | CORE-1 数据目录已落地；本基线未含 CORE-4 水位 | 共用目录解析器，等待 CORE-4 契约对齐 | F22、F27 |

ADR 已指定 checkpoint 加 SQLite backup API。下文保留该选择，并补齐跨库停写、恢复日志与掉电边界。0015–0017 按库名区分，不编辑已发布脚本（F19–F21）。

## 2. 决定

### D1 先封住所有开库入口，再开始迁移

非 ephemeral 启动先解析 CORE-1 data_dir、校验目录属主与非符号链接，再持 daemon.singleton.lock。锁持续到进程退出，包括停靠期。先查标记与未完成事务日志，再打开任何业务池。generation、授权管理和诊断服务可以初始化，不启动模型、模块、日程、恢复 sweep 或业务写入。

将现有 open 的“建池并迁移”拆成内部阶段，由一个迁移协调者拥有全部启动连接。先完成双库保护，再逐库迁移。memory 迁移、打开及 FTS rebuild 都属于失败门，不能沿用 F8 的 warn 后继续。两库成功、业务初始化成功后才发业务 ready；发现写入失败不报 ready。

所有合作的离线数据库入口必须遵守同一锁和标记门。F12 的 consent 失效及 decide export/delete 先改为在线请求；离线退路只有拿到单例锁且无标记、无未完成日志时才能运行。离线入口不得自行自动迁移，schema 不兼容就要求启动 daemon。拿不到锁就返回 busy，不先打开库。此项是快照验收的前置，不声称现有锁已经覆盖它。

一致性定义为“同一个无写入区间内两库的已提交状态”。backup API 不提供两个独立库的共同事务，本稿用全局停写补足。非合作进程直接写 SQLite 不在保证范围，沿用 CORE-2 同 UID 威胁边界；检测到锁竞争就停靠，不忽略 busy。

### D2 快照方案与空间预检

SQLite 行为依据 F28。比较表中的取舍是本稿设计理由。

| 方案 | 取舍 | 本稿选择 |
|---|---|---|
| VACUUM INTO | 会重建数据库布局，不能用原文件字节证明物理恢复；仍需跨库停写 | 不选，避免把布局重写带进升级保护 |
| SQLite backup API | 可生成独立可打开的库；仍须停写与检查 backup 完成。与 ADR 指定步骤一致 | 选用。实现片需选定有维护的 Rust 绑定并验证 SQLx 连接生命周期 |
| checkpoint 后关库复制 | 复制可以保留 checkpoint 后主文件字节；必须确认全部连接关闭与 WAL 已合并 | 留作备选。若 backup 绑定不能可靠接入，先裁决修订 ADR，不静默换方案 |

先用不运行迁移的维护连接探测已存在的两个库。先查空间，再对两库依次执行 `PRAGMA wal_checkpoint(TRUNCATE)`，要求 busy=0 且无未完成帧。任一失败不迁移。不存在的库记录 `existed=false`，不为快照创建空库；首次安装也走同一状态机。

空间按 data_dir 所在文件系统的可用空间算，不能用整盘容量。设 S 为两库主文件与 WAL 的总大小上界，G 为本次迁移清单声明的额外数据库、WAL 与 FTS 临时空间上界，R 为 max(64 MiB, S 的 10%)。要求空闲空间至少 `2*S + G + R`，含快照与恢复 staging 各一份。checkpoint 前先按含 WAL 的 S 预检，之后重算，不因 WAL 变小而放宽已预留的恢复空间。

G 必须由迁移作者给出保守上界和大库夹具证据；缺失就停靠 `space_budget_unknown`，不把倍率当作空间保证。按文件系统实际分配保留恢复预算，不只创建稀疏文件。只读库或无法测量空间也停靠。执行期间遇 ENOSPC 仍按失败处理，预检不能消灭并发磁盘消耗。

快照目录固定 `<data_dir>/migration-snapshots/<UTC紧凑时间>-<随机attempt_id>/`，0700；文件 `agent24.db`、`memory.db`、`manifest.json` 为 0600。名称不直接拼接版本或客户端路径。manifest 记录格式版本、attempt_id、时间、来源成功二进制版本及 SHA-256、目标二进制版本及 SHA-256、两库是否存在、长度、SHA-256、迁移清单摘要、迁移前水位。首次安装来源版本为 null。来源身份由新增 `migration-last-success.json` 提供，正常就绪后原子记录二进制身份与已验证水位；升级前复核它与现库相符。旧安装没有该记录时写 null，不拿当前升级二进制冒充前一版本。

逐库 backup 到 `.partial`，要求 step 返回 SQLITE_DONE 且 finish 返回 SQLITE_OK，再校验完整性与迁移表，再关闭目标连接、fsync 文件、rename 为正式文件、fsync 目录。两个文件都完成后才原子发布 manifest。新建的快照目录及 `migration-snapshots/` 都要逐级 fsync 父目录，一直到 data_dir；全部完成后才允许把 phase 写成 migrating（§8 R1）。源维护连接关闭后才建迁移连接。快照不能在任一库已迁移后补拍。半成品不授权迁移。

### D3 持久状态机与成功清理

`<data_dir>/migration-guard.json` 是内部恢复日志，不是用户可删除的解锁开关。字段含 format_version、attempt_id、snapshot_id、目标二进制身份、phase、两库恢复进度与最后错误。phase 为 preparing、snapshotted、migrating、restoring、restored、committed。所有更新均用同目录独占创建临时文件、0600、写完 fsync、rename、父目录 fsync。目录操作失败就不前进。

preparing 先落盘，随后 checkpoint 和快照。snapshotted 必须对应完整 manifest。migrating 必须在第一个迁移前落盘。两库迁移、FTS rebuild 与校验成功后标 committed，再装配业务并发布发现和 ready。业务装配失败不把已成功迁移误称迁移失败，不自动回退已 committed 的库。

重启见 preparing 或 snapshotted 且尚未 migrating，原库未做 schema 变更，直接停靠并说明中断。见 migrating 或 restoring，则在不开业务的前提下幂等恢复完整快照，再停靠。恢复不是再次迁移。日志损坏、快照缺失或校验不符一律停靠，不能猜测已完成。

成功快照保留到后续一次正常启动验证后，且至少 7 天；满足这两个条件的旧快照可清理，最近一套成功快照继续保留。失败、未完成或标记引用的快照一直保留，空间不足不能自动删它们。清理只在持锁、无活动升级时进行。按 manifest 删除整套，不按单库年龄删除。未发布的 `.partial` 仅在确认没有迁移发生且不被日志引用后清理。保留快照不表示可在业务写入后无损降级。

### D4 失败时整体恢复，双文件不假装一次原子替换

1. 停止迁移任务，等待取消完成。协调者显式 close 并 await 全部池、独立连接及工作线程，包括打开失败路径持有的连接。禁止只 drop 某个 Store clone 后开始换文件。
2. 将 restoring 日志与 migration_failed 标记落盘，校验两份不可变快照。把快照复制到各正式库同目录的 `.restore.<attempt_id>`，逐文件校验长度与 SHA-256，fsync。恢复不得消耗唯一快照。
3. 关闭所有连接后，将当前失败主库 rename 到本次隔离目录。将原 `-wal`、`-shm` 一并移入隔离目录，不覆盖已隔离证据，确认正式路径已无这两个文件，再 rename staging 到正式库名，fsync 父目录。缺失库则移走本次新建库和 sidecar，不创建替代库。
4. 每替换一个库持久更新进度。两个库都恢复后核对正式文件 SHA-256 与快照完全一致。只在临时副本上跑完整性与逻辑数据检查，避免检查连接改写正式库头。标 restored，保持 migration_failed 并进入停靠。隔离的旧 sidecar 在 restored 持久化后删除，快照继续保留。

rename 只保证一个文件的可见性切换。双库整体原子性由单例锁、不开业务、恢复日志与启动重放保证，不声称文件系统提供双文件事务。每一步掉电后都按 manifest 重建 staging，并恢复全部库；不信任仅内存中的进度。恢复期间不 checkpoint 失败库，不把失败 WAL 应用到快照。

字节验收比较“恢复后的正式库”与“已关闭的不可变快照”，不比较 checkpoint 前的 db+WAL 文件布局。另用迁移前后逻辑导出对比证明已提交数据、schema 与迁移水位恢复；缺失库证明仍缺失。

任一步失败设置 `rollback_failed`，保留快照、隔离文件、进度和错误。两库均不开，不尝试带一个恢复库启动。标记写失败时，未完成 guard 日志仍阻止下次迁移；两者都无法持久化时当前进程继续只读停靠并报告持久化故障，修复磁盘前不可发布或自动启动业务。重试迁移端点拒绝 rollback_failed，需安装器或人工恢复流程先证明 restored，不能靠删标记解除。

### D5 停靠标记与 health 契约

`<data_dir>/migration_failed` 是 0600 JSON。字段如下，路径均为根目录内相对路径，错误去敏，不含 SQL 参数、令牌或用户内容。

| 字段 | 内容 |
|---|---|
| format_version、attempt_id、snapshot_id、created_at | 格式版本 1、失败尝试、快照引用与 UTC 时间；预检失败 snapshot_id 可为 null |
| failed_binary、previous_binary | 各含 semver 与 sha256；previous 可为 null，不凭文件名判断版本 |
| database、migration_version、phase | 失败库名、确知的脚本号或 null、preflight/migrating/fts_rebuild/restoring |
| error_code、error_summary、rollback | 稳定原因码、有界摘要、not_needed/in_progress/restored/failed |
| before_migrations、observed_migrations | 两库水位、存在性与验证状态；恢复失败时 observed 可为 unknown |

标记使用 D3 的原子写入与目录 fsync。格式未知或损坏也停靠 `marker_invalid`。预检失败也写标记，必须明确数据库未改、rollback=not_needed。审计用独立 owner-only 文件，不依赖业务库；写入失败不得清标记。

实际路径为 `GET /api/v1/health`。停靠返回 HTTP 503，JSON `status="migration_failed"`，保留 version/backend，加 `reason={code,phase,rollback,retry_allowed}`。公开响应只给稳定码，不给文件路径与原始错误。正常返回 200、status=ok；unknown status 一律不可复用。503 可使旧的 F24 客户端不误判健康，但还必须修掉它们的自动 spawn 退路。

与 CORE-4 共用 `protocol_version` 与 `migrations`，不另设另一套水位。目标语义是分别报告 agent24.db 与 memory.db 的实际已验证水位，另在诊断中给目标水位。restored 报恢复后值，failed 报 unknown，绝不把编译内嵌的最高迁移号冒充实际值。本基线没有这两个字段（F22），合并 CORE-4 后才定字段形状与兼容规则，见 Q1。

实现片同时改 OpenAPI 的 Health.status const、503 响应、reason schema、诊断路由和 repair 路由，并同步 Rust 类型、生成客户端与夹具。路由仍受 G-API-3 双向覆盖检查。CORE-1 决定所有路径的根，不另引 XDG 或 HOME 路径。

### D6 最小诊断路由与发现

停靠 router 独立构建，不调用业务 router 再逐条禁用。只挂下表、CORE-2 登记与证明控制通道，以及 D8 唯一恢复写入口。不挂 events、chat、模块、数据库查询、任意日志下载或 SQL 执行。所有读取来自标记、manifest 和已验证缓存，不调用自动迁移的 open。

| 方法与路径（共同前缀 /api/v1） | 内容 | 最低权限 |
|---|---|---|
| GET /health | D5 去敏状态及 CORE-4 协商字段 | 显式公开 HealthRead |
| GET /diagnostics/migration | 失败版本、库名、脚本、阶段、去敏错误、是否可 retry | 新增 DiagnosticRead，已登记一方令牌日常级 |
| GET /diagnostics/migration/snapshots | 本次快照的 id、大小、哈希、存在性、恢复进度；不返回文件内容 | DiagnosticRead 日常级 |
| GET /diagnostics/migration/space | 本次探测的可用量、预算与失败项 | DiagnosticRead 日常级 |

Creative、开发者令牌均无诊断与 repair 权限。CORE-2 2b 未完成前只开公开 health，其余返回可识别的 `diagnostics_unavailable`，不借 legacy bearer 放行。HEAD 继承 GET 权限；未知路径 404，已知路径错误方法 405。单独测试路由集合与缺省拒绝。

停靠也需发现。建议 daemon.json 加 `startup_state="docked"` 与 enroll_endpoint，绑定本次 generation，不含秘密；只在诊断监听绑定成功后发布。ready 管道发独立 `type="docked"` 通知，绝不发 `type="ready"`。正常实例在双库成功后按原就绪顺序发布。发布失败保持停靠，外壳可报告发现故障，不能把旧记录当成当前代。

此处对 CORE-2 D5“完成迁移才发布”的规则申请窄例外，仅允许发布明确 docked 的诊断实例，见 Q3。清理仍验证本实例所有权，不删其他 generation。

### D7 外壳与服务管理器

桌面 tryReuseExternal 改为解析有界 health JSON，返回 ready、docked、incompatible、absent 四种结果。docked 显示失败码与恢复入口，禁止业务请求，不 spawn 子进程，不累计为普通掉线重启。incompatible 提示升级。absent 才走已有启动决策，启动前检查标记与锁。启动得到 docked 通知时也停住自动重启。

CLI connect、attach_only、daemon start 和 TUI 共用同一探测结果。docked 在 capability 的“授权不可用”分支之前处理，业务命令返回明确 migration_failed，repair 转入诊断接入。不得回落 ephemeral，也不得用已有持久数据目录另起实例。显式 ephemeral 测试不读写持久标记，且不宣传为恢复成功。

停靠进程继续持锁和监听，等用户处理，不返回迁移错误到 main 使进程退出。保留 SIGTERM、SIGINT 的干净退出；子进程模式父 EOF 仍按 CORE-2 关闭，桌面识别 docked 后不自动重拉。LaunchAgent 保留 F25 的 SuccessfulExit=false。未来 systemd --user 用 Restart=on-failure，不用 watchdog 或 readiness 超时杀死停靠进程；若用 Type=notify，就发布“诊断监听可用”的管理器 READY 与 STATUS，不能等同业务 ready。模板尚未存在，不宣称已落地。

### D8 只通过已授权恢复路径清标记

启动发现标记且二进制 semver 与 SHA-256 都未变，直接停靠，迁移次数必须为零。未完成的恢复仍先按 D3 收尾，不能把“不重试迁移”解释为允许留下混合库启动业务。相同 semver 换构建也不自动放行。目标更新版本或不可比较版本同样停靠，不能把“换了文件”当用户同意重试。

安装器换回旧二进制是独立路径。先停止停靠 daemon 并取得同一单例锁，校验已安装二进制签名或可信发布清单及 hash。要求 semver 严格小于 failed_binary，且身份精确等于 manifest 的 previous_binary；并核对 restored 双库 hash、水位及旧二进制可支持的迁移清单。不接受字符串字典序，不只比较版本号。无 previous、版本相同、恢复失败或旧版本也不兼容时拒绝清标记，见 Q4。

安装器先落盘回退审计和状态，再原子归档标记并 fsync，最后释放锁启动旧版本。清除中途崩溃时由 guard 日志保持阻断；旧二进制不认识 guard 时，安装器必须先完成验证和恢复，不能直接交给旧程序猜。预检失败没有快照时，只能证明原库未变且有可信来源身份后走此路径。它不允许恢复成功运行后产生的新业务数据，亦不允许任意 CLI 离线删标记。

`agent24 daemon repair --retry-migration` 只调用 `POST /api/v1/diagnostics/migration/retry`。新增 MigrationRepair，全局提升级。只接受 FirstPartyShell 与 CORE-2 D4 的当次授权，绑定 generation、attempt_id、snapshot_id、当前二进制身份及请求摘要。按 CORE-2 D7 验证明、原子消费 nonce 后才进入处理器；§10 的签发限制继续适用，不能委托给开发者令牌。

建议 2b、2d 和停靠态登记通道未完成前完全不开放 retry，包括 ProductHost 子进程模式。不以父管道全权代替在场证明。公开 health 返回 retry_allowed=false，CLI 给出依赖未就绪提示，安装器回旧版本路径保留。这是 Q2 的建议裁决。

只有 rollback=restored 或 not_needed、证据完整且可重新满足空间预算时接受 retry。并发请求仅一个进入。先写带本次授权审计引用的 retry 事务日志，再归档清除旧标记，轮换 generation 与全部授权，重走 D1–D4。返回 202 只表示接受，不表示健康。失败写新标记并停靠；成功后才发布业务 ready。retry 不能跳过快照，也不准按旧 nonce 自动再试。未完成 retry 日志在重启时停靠，用户重新证明。

## 3. 测试门（实现片先 red，再 green，再做变异）

本次只提交文档。下表是未来实现验收，不声称测试已运行。每片保留反面对照与变异失败证据。

| 门 | 可证伪断言 | 对应变异 |
|---|---|---|
| T1 中途失败 | 在两个库分别注入第 N 个迁移执行前、SQL 中途、commit 后及 FTS rebuild 失败，均整体恢复且无 ready；N 遍历待执行集合 | 仅撤销当前事务；忽略 memory 错误；跳过 rebuild 门 |
| T2 字节与数据 | 双库恢复主文件逐字节等于关闭后的快照，逻辑数据/schema/水位等于迁移前；原本缺失库仍缺失 | 仅恢复主库；漏迁移表；恢复空库；恢复后自动 open |
| T3 WAL/SHM | 用真实未 checkpoint 数据及失败库 sidecar 夹具，恢复后无旧 sidecar，重开副本无失败迁移数据 | 先换库再保留旧 WAL；有连接未关；漏删 shm |
| T4 路由隔离 | 停靠只出现 D6 控制面；chat、events、模块、日程不可达，后台业务任务未启动 | 复用完整业务 router；先启动 scheduler 再停靠 |
| T5 外壳 | 200+failed、503+failed、未知状态均不可复用；桌面、CLI、TUI 不 spawn、不回 ephemeral；capability 报迁移失败 | 只判 200；将 docked 当 absent；先报授权不可用 |
| T6 重复启动 | 同一版本与 hash 的标记重复启动无 checkpoint、backup、迁移调用；hash 变化亦不自动迁移 | 启动忽略标记；仅比较 semver；更换 hash 即放行 |
| T7 空间 | 低于预算一字节、空间不可测、G 缺失均在迁移前停靠；快照阶段 ENOSPC 原 schema 不变 | 不计 WAL/staging；用稀疏预留；忽略写失败 |
| T8 回滚失败 | 注入 close、copy、rename、unlink、fsync 失败，保留证据、两库禁用、retry 拒绝，进程持续监听 | 忽略文件错误；仅凭一个库恢复即 ready；自动删快照 |
| T9 掉电重放 | 用文件操作层故障注入模拟掉电：丢弃所有未 fsync 的写、rename 和目录项，然后在 D3、D4 的每个持久边界重启。快照完好且没有外部故障时，**必须**恢复成完整的旧状态，不能以停靠报错了事；只有快照或日志确实损坏时才允许停靠。任何情况下业务都看不到混合的两个库 | 提前标 committed；去掉任一级父目录 fsync；信任内存中的进度 |
| T10 停写 | CLI 两个直接开库入口与 daemon 并发时不写不迁移；busy checkpoint 不生成完整 manifest | 绕过单例锁；未完成快照即迁移 |
| T11 清标记权限 | 无证明、错代、错 attempt、过期、重放、开发者令牌、ProductHost、TTY 均无副作用；2b/2d 未完拒绝 | legacy bearer 放行；把布尔 authenticated 当证明 |
| T12 版本回退 | 可信精确前一版本且 restored 才清除；同版本不同 hash、伪版本、未知来源、水位不兼容都拒绝 | 字符串比较；只比小版本；忽略库 hash |
| T13 服务与发现 | LaunchAgent 真机停靠不重启；systemd 模板交付后同验，SIGTERM 干净退出；docked 无业务 ready，第二实例取锁失败 | 返回迁移 Err；设 Restart=always；docked 发 ready |
| T14 契约与保留 | OpenAPI/Rust/生成客户端一致；restored 水位真实、失败 unknown；引用快照不被清理，诊断不泄密 | 报编译最高号；按年龄删失败快照；公开原始错误 |
| T15 预算超支恢复 | 迁移实际用量 G 超过声明值、磁盘被写满时，先释放保留文件，再从快照完整恢复；含 FTS rebuild 的迁移未声明 G 时，CI 报错 | 不设保留文件；保留文件是稀疏的；恢复前不释放保留；漏掉声明检查 |

## 4. 实现分片（每片目标 ≤300 行）

| 顺序 | 分片 | 前置与交付 |
|---|---|---|
| 1 | 5a 停写入口 | CORE-1；收口 F12 两个 CLI 开库点、锁与禁止离线迁移，T10。在线路由与离线退路可再拆 |
| 2 | 5b 连接生命周期 | 5a；拆分无迁移维护连接与迁移阶段，显式 close 失败路径，memory 失败升级为整体门。先交测试夹具，不单独切生产启动 |
| 3 | 5c 空间与 manifest | 5b、Q5；空间预算、文件状态与原子日志，每个可分别一片，T7/T9 |
| 4 | 5d 双库快照 | 5c；backup 绑定、checkpoint 与 manifest 发布，T2/T3/T10；无保护时不可启用新迁移 |
| 5 | 5e 整体恢复 | 5d；staging、隔离 sidecar、重放与 rollback_failed，T1/T2/T3/T8/T9。按恢复器、启动恢复两片交付 |
| 6 | 5f 停靠与协议 | 5e、CORE-4、Q1/Q3；Health、诊断 router、发现与服务生命周期，T4/T6/T13/T14。类型契约、router、启动接线分片 |
| 7 | 5g 外壳识别 | 5f；桌面、CLI/TUI 分片完成 T5，之后才能一起启用生产迁移保护 |
| 8 | 5h 安装器回退 | 5g、可信发布清单、Q4；版本身份、锁、审计及标记清除，T12 |
| 9 | 5i 授权 retry | 5g、CORE-2 2b/2d及停靠登记、Q2；先授权映射再 retry 状态机，T11，再做真机证明验收 |

每片保持默认关闭的接线或可验收的独立内部接口，不交付能绕过保护的中间产品。若任一片超过 300 行，按表内边界继续拆。数据写入、恢复、授权与 wire 变更均等待 PR-Daemon APPROVE，文档接受不代替实现批准。

## 5. 不在本稿

- 在场证明提供者、签名流水线、远程设备配对与 Linux headless 证明的重新设计。
- 任意历史版本降级、业务已写入后的无损回退、远端备份与跨设备迁移。
- 模块自有库的通用迁移框架。启动清单必须枚举 daemon 自动迁移库；发现新增库时另加保护与验收，不宣称双库覆盖所有模块。
- 改代码、修改已发布迁移、开 PR、合并、部署或 Mac mini 执行。

## 6. 疑问（交协调者裁决）

| # | 冲突或未闭合处 | 建议裁决 |
|---|---|---|
| Q1 | 任务说 CORE-4 已有 protocol_version/migrations；F22 证实本基线没有 | 给出 CORE-4 接受提交与最终字段。5f 依赖它，停靠水位报告实际恢复值或 unknown，不私建竞争契约 |
| Q2 | 2b/2d 未完成时没有可用的提升级证明；ProductHost 现状全权不能代替证明 | 完全关闭 retry，包括子进程模式。保留公开 health 与安装器可信回旧版本 |
| Q3 | CORE-2 D5 要迁移完成后才发布登记与发现；停靠却必须可发现且可登记 repair | 允许明确 docked 的诊断发现与最小登记通道，无业务 ready；为 CORE-2 建后续实现协调项，不改冻结稿 |
| Q4 | ADR 只说换回旧二进制，没有可靠来源版本记录或相同 semver 修复规则 | 采用严格 semver 降级且 hash 精确匹配前一成功版本。未知来源不自动清标记；安装器补可信发布身份与水位清单 |
| Q5 | F12 的 CLI 会绕过锁自动迁移；G 上界与 backup Rust 绑定尚无实现证据 | 5a 为硬前置；各迁移声明 G 并保留恢复空间；5d 前选绑定并证明关闭语义，不能先上线保护宣称 |
| Q6 | ADR 标提议；任务把 repair 归决策 4，而 F3 显示条文在决策 2 | 本稿按指定条文设计，不改 ADR 状态与编号。协调者确认接受范围并由 DOCS 线订正 |
| Q7 | 双库 rename 不能一次原子提交；磁盘彻底不可写时标记也可能失败 | 接受 guard 日志加不开业务的恢复边界；要求迁移前日志持久化、恢复预算和逐边界掉电验收，不声称任意硬件故障都可自动恢复 |

## 7. 协调者裁决（2026-10-27，Opus）

| # | 裁决 |
|---|---|
| Q1 | 沿用 CORE-4（PR #1301，分支 `ab/core-04-health-version`）的字段：`protocol_version`，以及 `migrations.{store,memory}`。这两个字段取 `_sqlx_migrations` 中 `success=1` 的 `MAX(version)`，查询失败时报 0 并打 warn。5f 以 #1301 合入为前置，停靠时 `migrations` 报恢复后的实际值。恢复失败时，在 `reason` 里标 `unknown`，`migrations` 字段本身保持 CORE-4 的形状不变，不另起一套契约。 |
| Q2 | 采纳。2b、2d 和停靠态登记通道全部完成之前，retry 完全不开放，ProductHost 子进程模式也不例外。`health.retry_allowed=false`。安装器换回旧版本的路径保留。 |
| Q3 | 采纳窄例外：只允许发布 `startup_state="docked"` 的诊断发现，以及最小登记通道；不发业务 ready。CORE-2 冻结稿不改，在 5f 的 PR 里登记这一协调项。 |
| Q4 | 采纳：semver 必须严格降级，并且 hash 与 `previous_binary` 完全一致。来源未知时，不自动清除标记。 |
| Q5 | 5a 是硬前置。G 的取值见 §8 R2（替代本格原先的「G=S」简化）。backup 的 Rust 绑定在 5d 之前选定，并验证关闭语义。 |
| Q6 | ADR-037 已于 #1249 接受，状态行已由 DOCS-2（#1304）订正。repair 条款按 ADR 原文设计。条款编号的出入交给 DOCS 线核对，本稿不改动。 |
| Q7 | 接受：以 guard 日志加「不开业务」作为恢复边界；迁移前日志必须落盘；按每个落盘边界做掉电验收。不承诺任意硬件故障都能自动恢复。 |

本稿评审至多一轮，之后冻结。评审意见中的真问题就地订正，其余登记到对应实现片。

## 8. 外审修订（2026-10-27，Mac mini Codex 一轮；本节优先于前文，修订后冻结）

| # | 来源 | 问题 | 修订 |
|---|---|---|---|
| R1 | Codex High | 新建快照目录后只 fsync 了目录内的文件和目录自身，没有 fsync 父目录。如果迁移已经提交部分脚本时掉电，可能出现 migrating 日志已落盘、整套快照目录却丢失的情况，无法恢复 | D2 改为：`<attempt>/`、`migration-snapshots/` 直到 data_dir，逐级 fsync 父目录，全部完成后才写 phase=migrating。guard 日志、标记和 staging 的创建都按同一规则执行。T9 的变异项「去掉任一级父目录 fsync」必须变红 |
| R2 | Codex Medium | §7 Q5 的「G=S」没有依据。中文 bigram 的 FTS rebuild 会扩充内容和索引，整批在一个事务里产生 WAL，可能超出预算并触发 ENOSPC | 撤回 G=S 的简化。迁移没有声明 G 时，默认 **G = 2·S**；含 FTS rebuild 或整表重写的迁移**必须**声明 G，并附上大库夹具测得的峰值，缺失时在 CI 中报错，不在运行时停靠。另外，恢复预算（staging 所需的 S）在迁移前用真实分配的保留文件占住，迁移期间即使遇到 ENOSPC，也先删除保留文件再执行恢复。新增 T15：G 实际大于声明值、磁盘被写满时，仍能用保留的预算恢复成功 |
| R3 | Codex Medium | T9 用强杀进程测试，内核缓存不会丢，测不出缺少 fsync 的问题；「或停靠报错」的写法也会放过恢复总是失败的实现 | T9 改为文件操作层故障注入，模拟掉电时丢弃所有未持久化的写入。快照完好时必须恢复成功（见 §3 T9 新文本）|

本稿到此冻结，后续意见登记到实现片。
