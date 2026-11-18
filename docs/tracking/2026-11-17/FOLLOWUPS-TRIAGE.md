# Follow-ups 分诊（2026-11-17）

台账 `docs/agent/followups.md`。起点是 85 条 OPEN 的 `FU-*`。`ME4-CODEX-DEBT-*` 另有 9 条，不是这 85 条，见文末。

本文件只做分诊。不改产品代码。

## 计数

| 类 | 条数 | 台账 |
|---|---:|---|
| 已修 | 7 | 标 `[x] done=PR#n` |
| 作废 | 5 | 标 `[x] done=PR#n`，正文写明前提没了 |
| 作废，不改台账 | 1 | 没有 PR 号，规则不允许标 done |
| 修 | 6 | 保持 OPEN，另加新开的 FU-132 |
| 挂 | 66 | 保持 OPEN |

上表五类加起来是 85。加上 FU-132 之后，OPEN 的 FU 是 74（85 − 12 条标掉 + 1 条新开）。修的 6 条仍 OPEN，FU-132 使待修变成 7 条。

`gh pr view` 对 #716、#1380 返回 404。号来自合并提交第二父：`9c458e649`（#716）、`a403703b2`（#1380）。其余关闭用的 PR 都用 `gh pr view` 对过，状态是 MERGED。

## 批次

只排「修」。每批同一区域，按主流程先做。FU-132 在第一批。做完若一条里还有挂着的半边，按台账头的规则把原条收窄标 done，另开一条承载剩下的。

### 批次 1 · 默认模型与旧令牌

- 条目：FU-132、FU-131
- 区域：`rust/apps/agent24d/src/capabilities/`、`default_model.rs`、`elevation_summaries.rs`、`tests/first_party_http.rs`、`tests/elevation_summaries_http.rs`
- 规模：两处都是 S，预计远小于 300 行、少于 8 个文件
- 验收 FU-132：照 `skill_activation_refuses_legacy_and_creative_tokens_and_admits_the_host`（`first_party_http.rs:1812`）。旧共享令牌 `PUT /api/v1/models/default` 得 403，默认模型不变；Creative 403；ProductHost 200。删掉 `middleware.rs:77` 那一行，这条测试变红。
- 验收 FU-131：隐藏字符或 121–128 字的 id 在进在场确认之前得到处理器的 400，默认模型不变。普通 id 仍是确认之后才保存。不放宽 `shown()`（`first_party.rs:822` 仍是 120）。

### 批次 2 · 入站空白正文

- 条目：FU-123 的空白一半
- 区域：`rust/apps/agent24d/src/inbound/service.rs` 及同目录测试
- 规模：S
- 验收：只含空白的正文与空串一样拒绝，不起 run。`None` 和空串的现有拒绝不变。
- 不做：瞬时错误误标 `start_failed`。那是并发边缘，留在收窄后的新条里。

### 批次 3 · REST 日程的星期字段

- 条目：FU-71
- 区域：`rust/crates/agent24-scheduler` 的 `next_fire.rs` 与 REST 创建
- 规模：M。做法取拒绝数字星期，与模块路径一致，不把数字悄悄改成 POSIX。
- 验收：REST 创建 `0 7 * * 1-5` 被拒。`MON-FRI` 从固定的周六起点第一次触发在周一。正对照：今天数字形式会被接受，且从周六触发到周日。

### 批次 4 · 已装服务时的重启提示

- 条目：FU-68
- 区域：`os_routes.rs` 的重启提示、`agent24-cli` 的 `service.rs` / `main.rs` 及对应测试
- 规模：M
- 验收：装了 launchd 时，提示或执行的是 `launchctl kickstart -k`（或同等、仍由 launchd 认领的路径），不是手动 `daemon stop && daemon start`。没装服务时文案和行为不变。

### 批次 5 · 模块握手的两个报错

- 条目：FU-94、FU-95 的超时一半
- 区域：`rust/crates/agent24-os-proto/src/module.rs` 及单测
- 规模：S
- 验收 FU-94：`A24_HANDSHAKE_TOKEN` 不是合法 UTF-8 时，错误不是 `EnvError::Missing`。变量真缺失时仍是 `Missing`。
- 验收 FU-95(a)：内核连上之后不写握手响应，SDK 在有限时间内失败，而不是一直挂着。
- 不做：FU-95(d) 跨 `exec` 的 fd 测试。留在收窄后的新条里。

## 全表

| FU | 一句话 | 类别 | 理由 | 预计改动 | 规模 |
|---|---|---|---|---|---|
| FU-1 | rekey 撞上 (org,space) 唯一约束时冒出裸 Sqlx，不是 Conflict | 挂 | 只有没迁走的陈旧 v1 行才走得到，新库主流程不撞 | agent24-memory/src/lib.rs | S |
| FU-2 | 记忆库打不开时 daemon 只 warn，然后无记忆继续跑 | 挂 | serve_steps 写明失败就降级启动；只抬日志级别并不恢复记忆，拒绝启动要另定 | agent24d/src/server/memory.rs | S |
| FU-3 | sweep 的注释没写跨进程，KvStore::open 仍公开 | 挂 | 缺的是注释；别的进程 attach 同一文件属于恶意本机进程 | agent24-memory/src/lib.rs | S |
| FU-4 | 首次插入不核对 owner_key 是否编码了声明的 (org,space) | 挂 | 要直接伪造公开 API 的 key，不是用户主路径 | agent24-memory/src/lib.rs | M |
| FU-5 | Conflict 只打呈递身份，不打已存行 | 挂 | 拒绝已经发生，缺的是日志里的已存身份 | agent24-memory/src/lib.rs | S |
| FU-6 | usr 与 os 前缀的不相交性已有非 ASCII 交叉积测试 | 已修 | os_memory.rs:2112 personal_spaces_are_disjoint_from_module_spaces 覆盖「用户」和 🙂。提交 926c90e60，gh pr view 639 已合并 | — | — |
| FU-8 | Decision 仍是可忽略的返回值，没有 AccessGrant | 挂 | 开工前就标成待拍板的类型改造；现调用点有检查，改收据是另一个量级 | agent24d/src/authz.rs | L |
| FU-9 | 模块拿不到 KvStore 仍靠私有字段，没有 scoped feature | 挂 | 出借路径已经不给根句柄；cargo feature 是额外构建期保证 | agent24-memory/Cargo.toml | M |
| FU-10 | CLI os list 的打印形状变了 CI 不会红 | 挂 | 路由对 OpenAPI 已有门；剩下是 CLI 文本契约 | agent24-cli/src/main.rs | M |
| FU-11 | 核实后 HTTP 会再验 os 输入，原先怀疑的洞不成立 | 作废 | 在线 enable 只转发；写 os.json 前 daemon 拒绝未知名字（os_routes.rs:1082）。没有单独补洞的 PR，按台账规则不标 done | — | — |
| FU-12 | 召回排序仍是 RRF，没有 frecency | 挂 | 召回质量，不是主流程对错 | agent24-memory/src/hybrid.rs | M |
| FU-13 | 模型权重没有 lockfile，不一致时也不会失败 | 挂 | 权重在上游运行时，本仓今天按配置选后端 | agent24-models | M |
| FU-14 | ACP 与自定义协议已按缝裁决 | 已修 | decision.md:1243 ADR-031 状态为已采纳，并写明裁决本条。提交 dcdf62a8，gh pr view 151 已合并 | — | — |
| FU-15 | docs/laws 与 PR 模板的法律门都在 | 已修 | 目录随 #143 进入；模板「受影响法条」是 11172285，gh pr view 345 已合并。标 done 用收口的 #345 | — | — |
| FU-18 | agent loop 没有按趋势打断的打转检测 | 挂 | 已有 10 次硬上限；按趋势拉人是长程体验 | agent24-agent/src/run_manager/execute.rs | M |
| FU-20 | 摘要压缩仍接受任意非空模型文本；Guardian 畸形输出已 fail-closed | 挂 | 审批门那一侧已经关上。剩下是压缩文本怎么降级，不是当前放行 | agent24-agent/src/router_helpers.rs | M |
| FU-21 | 知识层不扫祖先目录；微信 token 仍只在启动时登录一次 | 挂 | 目录注入路径不存在。微信桥是开发工具，过期重登不进产品主流程 | packages/wechat-bridge/src/main.ts | S |
| FU-22 | 没有可复用的角色-工具矩阵 | 挂 | 未来能力。只读边界仍只是 explorer 这一处 | agent24-tools | L |
| FU-23 | 开源/商业表已写在 SPEC-EDITIONS，≤50 人与法律升格还没定 | 挂 | 产品决策残留。#143 写了表，但本条后半仍待确认，不整条标 done | docs/specs/SPEC-EDITIONS.md | S |
| FU-24 | 要不要独立 Auditor 仍未拍板 | 挂 | 原文就是 Cos72 之后再议，并和本地小模型成本冲突 | — | L |
| FU-25 | Skill 分发的 distro 缝和版本覆盖都没有 | 挂 | 分发还没做，现在没有老用户升级路径可坏 | agent24-store/src/skill.rs | L |
| FU-26 | 用户能看能撤断言，事件/巩固/知识层仍看不见 | 挂 | 断言页已有（#672/#673）。剩下三层是产品可见性，不整条标 done | apps/desktop Memory 页 | M |
| FU-27 | 轨迹与产物已分表，法律仍写没有类型区分，并等 Auditor | 挂 | mem_trace_* 与 mem_artifacts 在 #125 就分开了，早于本条。CONTEXT.md:102 在此后仍写没有类型区分，并明确等 FU-24。不把 #125 当成关掉本条 | docs/laws/CONTEXT.md | S |
| FU-28 | 改了 skill 正文但 version 不升，CI 不红 | 挂 | 分发未做。运行时的同版本冲突不是这条 diff 门 | .github/workflows/ci.yml | S |
| FU-29 | 旧桥的 at-most-once 回复随 nostr-bridge 删除 | 作废 | packages/nostr-bridge 在 b72b076d 删除，gh pr view 1785 已合并。自动入站回复明确不迁。#627 的 outbox 是用户发送，不是这条 reply() | — | — |
| FU-30 | 两个微信桥仍会在 rename 上互盖 | 挂 | 开发工具，双进程不是受支持配置 | packages/wechat-bridge/src/config.ts | S |
| FU-31 | 微信游标仍是 Node fsync，没有 F_FULLFSYNC | 挂 | 断电窗口，且桥是开发工具。文档没有写成抗断电 | packages/wechat-bridge/src/config.ts | S |
| FU-33 | Hyphae 仍没有 daemon status 与心跳文件 | 挂 | 上游。本仓 catch-up 只能报 unknown 或 incomplete（agent24-comm/src/daemon.rs:828）。泡测已不用 canary | agent24-comm/src/daemon.rs | M |
| FU-34 | F5 不再依赖默认二进制名 agent-speaker | 作废 | SOAK-F5.md:3 写明旧桥和 canary 已移除。查找顺序是 A24_HYPHAE_BIN（comm_routes.rs:380）。同一删除是 #1785 | — | — |
| FU-35 | 泡测自动结论仍只盖 health 和日程条数，采样是离散的 | 挂 | 泡测工具，不是产品主流程。SOAK-F5.md:14 已写明 PASS 不证明入站 | scripts/soak-monitor.sh | M |
| FU-36 | 判据 2 仍用日程条数，跑完的一次性任务也能过 | 挂 | 同一份泡测脚本。存在性不是活性，但不挡产品 | scripts/soak-monitor.sh | S |
| FU-39 | 两个 nostr 桥抢健康快照的路径已经没了 | 作废 | soak-monitor.sh 不再读 generation。桥与健康文件随 #1785 删除 | — | — |
| FU-40 | CI 仍不跑 cargo doc | 挂 | 文档链接债。本次没重数警告条数 | .github/workflows/ci.yml | M |
| FU-42 | 最大 initialize 已量过，正对照仍是恒真的长度比较 | 挂 | 上限测量在 #166 的 initialize.rs:887。正对照是 vec 自比（:931），token 长度注释仍写生成器不存在。判据的正对照没满足，所以不标 done；也不值得单开一批 | agent24-os-proto/src/initialize.rs | S |
| FU-43 | minirelay 上 10/30 的 canary 判据已从泡测拿掉 | 作废 | SOAK-F5.md:14：PASS 不评估 Nostr。上游 1/3 本次未再测。删除桥的 PR 是 #1785 | — | — |
| FU-44 | 失败握手后重启会铸新 token，并有测试 | 已修 | supervise.rs:1242 a_restart_does_not_reuse_the_token。提交 b64f3008，gh pr view 171 已合并 | — | — |
| FU-48 | 撤销文案仍写一律不再发送，比 dispatch 强 | 挂 | 行为结果归为未知是对的，差的是 SPEC 和 rustdoc 措辞 | SPEC-ME3、os-proto drain.rs | S |
| FU-51 | 变异自证在真实 cargo 下仍会偶发作废 | 挂 | 测试脚手架。失败方向是作废，不是假绿。本次未复跑 | docs/agent/mutate.py | M |
| FU-52 | 变异脚手架六条注释/断言精度大多还在 | 挂 | 不改行为。第①条例外已写进 mutate.sh，其余仍在 | docs/agent/mutate.sh | S |
| FU-55 | daemon 当 PID 1 或 subreaper 时 stop 会等不到组空 | 挂 | 文档已写成前提（容器用 tini）。养子收尸器是边缘部署 | agent24-os-proto/src/supervise.rs | L |
| FU-65 | sweep 仍认不出 exec 走、命令行不带 TMPDIR 的孤儿 | 挂 | 夹具自带寿命上限。根治要进程容器 | docs/agent/mutate.py | M |
| FU-66 | gone 是进程组不是进程树，两条补强测试还没有 | 挂 | 有意语义的测试缺口，不改停止行为 | agent24-os-proto supervisor 测试 | S |
| FU-68 | 已装 launchd 时，提示的 stop+start 会让 daemon 脱管 | 修 | 24/7 服务的主路径：干净退出后 KeepAlive 不拉起，随后手动 start 不再受监管 | os_routes.rs、agent24-cli service.rs 与 main.rs | M |
| FU-69 | 记忆游标仍是明文 seq 的 base64 | 挂 | 旁路，解不出别的分区。不是越权读 | agent24d/src/os_memory_page.rs | M |
| FU-80 | disable 与 shutdown 交接已用在途请求钉成确定的 | 已修 | 提交 4eb88a5c 只改测试。gh pr view 1287 已合并。本次没有再连跑 50 次 | — | — |
| FU-81 | panic 投递测试改为等 attempts>=2 再拨钟 | 已修 | deliveries.rs:1896。第二父提交就是 3360ade26，合并提交 9c458e649 写明 pull request #716。gh pr view 716 返回 404，号以合并提交为准 | — | — |
| FU-82 | 设计文档里的 model_callback 测试过滤器对不上现名 | 挂 | 只影响按文档筛测试，不改变行为 | agent24d/src/model_callback.rs 测试名 | S |
| FU-71 | REST 日程仍接受数字星期，1-5 会静默错一天 | 修 | 用户或 agent 写的 cron 走这条。模块路径已经拒绝数字星期 | agent24-scheduler next_fire.rs 与创建路径 | M |
| FU-73 | 卸掉的模块其日程行一直留着 | 挂 | 没有可靠卸载信号。按「这次没扫到」删会误伤 | agent24-scheduler | L |
| FU-75 | 不带 module 的 usage 仍回 cost_usd 0.0 | 挂 | 带 module 的已经是 null（routes.rs:315）。全局 0.0 是故意留下的金样，改 null 会动 wire | agent24-protocol types.rs、routes.rs | M |
| FU-76 | os list 不显示 model_access | 挂 | 要读 manifest 才知道能否外发，但不改变实际路由。回调侧已一律 LocalOnly | domain.rs MountReport、os_routes.rs | M |
| FU-77 | 逐次 local_only 的前提没了：回调一律 LocalOnly | 作废 | model_callback.rs:528 丢掉 access，privacy 恒为 LocalOnly。合并提交 a403703b2 写明 #1380。gh pr view 1380 返回 404，号以合并提交为准。原判据「不带字段仍走远端」与现行路由相反 | — | — |
| FU-84 | 审批 gate 仍只执行 schedule_callback | 挂 | 新内核能力加保留路由，要单独设计 | module_approval_broker.rs | L |
| FU-85 | remember 仍不幂等，SDK 靠翻页找 dedup | 挂 | 新 wire 字段和唯一索引，计划外的内核任务 | memory 回调与 agent24-os-sdk | L |
| FU-86 | os-proto 没有拆成 kernel/module feature | 挂 | 编译时间。不挡主流程 | agent24-os-proto/Cargo.toml | L |
| FU-87 | SDK 没有 retry_class | 挂 | 等真实调用方的分类表再提取 | agent24-os-sdk/src/error.rs | S |
| FU-88 | check-proto-exports.py 不在仓库，accept_one 仍返回 UnixStream | 挂 | 和 FU-86 同一类切开。不是当前事故 | agent24-os-proto/src/endpoint.rs | L |
| FU-94 | 握手 token 不是 UTF-8 时误报成环境变量缺失 | 修 | 排障会被带去找一个没设的变量。只加一个错误变体 | agent24-os-proto/src/module.rs | S |
| FU-95 | SDK 握手没有超时；跨 exec 的 fd 测试也没有 | 修 | 内核不回应时模块启动会一直挂。(b)(c) 已经不在：缺 id 会立刻 Protocol，透传名单已是显式数组。本批只做超时；(d) 继续挂 | agent24-os-proto/src/module.rs | S |
| FU-97 | 设计文档仍写 FiredBody 拒绝未知字段，代码已经不拒绝 | 挂 | WIRE 规格与代码一致。差的是 ME4-S3 设计稿没跟上 | docs/design/ME4-S3-os-sdk.md | S |
| FU-98 | delete 测试没断言 list；全空白 Fire-Id 仍放行 | 挂 | 测试名与空白头都是罕见输入，不挡主流程 | agent24-os-sdk scheduler 测试、fired.rs | S |
| FU-99 | 握手成功帧与命令注册已在同一把锁里 | 已修 | attach_registry.rs:585 先入队再装 live_calls，都在 inner 锁内。gh pr view 600 已合并（标题含 FU-105）。#543 先收过窗口。AgentEar 真机连跑本次没复跑 | — | — |
| FU-100 | 模型完成结果没有 finish_reason | 挂 | 协议扩展，要先设计评审 | agent24-models、model_callback.rs、SPEC | M |
| FU-101 | 附着目录防劫持有三份；PATCH 仍回全量列表 | 挂 | (a) 的顺序测试从 #529 就在，放宽复核会红。剩下是去重和响应形状 | attach_listener.rs、attached_routes.rs | M |
| FU-102 | 撤销与入队没有真并发测试 | 挂 | 测试缺口。现有覆盖是单线程顺序 | agent24-attach attach_registry 测试 | S |
| FU-104 | node 参考模块没演示 scheduler | 挂 | WIRE 仍写明这是范围控制。缺的是示例，不是内核行为 | examples/node-module | M |
| FU-106 | 响应只按 id 路由，不核对 method | 挂 | 作者已判定打不中：id 连接内单调且 pending 按连接私有。代码未变，不标已修 | agent24-os-proto/src/module.rs | M |
| FU-107 | 序列化失败会变成一行空帧 | 挂 | 作者已判定当前 Value 输入走不到。代码仍是 unwrap_or_default，不标已修 | agent24-os-proto/src/module.rs | S |
| FU-108 | 回退目录不存在时先 stat 再 mkdir | 挂 | mkdir 对已存在路径原子 EEXIST。作者判定不是真跟随符号链接。不标已修 | agent24d/src/server/callback.rs | L |
| FU-109 | 发布流水线仍用 ubuntu-22.04 | 挂 | 镜像 2027-04-17 才下线。今天还能构建 | .github/workflows/release.yml | M |
| FU-110 | KILL 之后的组确认仍固定再等 500ms | 挂 | 关机最多多等 500ms，不改变停的结果 | agent24-comm/src/daemon.rs | S |
| FU-111 | Starting 窗口里的配置重启会丢掉手动自启意图 | 挂 | 还没进入 Running 的窄窗口。已经写过 autostart 的主路径不走这里 | agent24-comm/src/daemon.rs | S |
| FU-112 | 晋升前复查 leader 是否已退出，没有能钉住它的测试 | 挂 | 删掉复查现有测试仍绿。测试缺口 | agent24-comm tests/daemon_supervise.rs | S |
| FU-120 | policy_version 不含 max_age 和 rate_limit | 挂 | 只改这两个数哈希不变。写进预映像会改对外版本号 | agent24d/src/inbound/admit.rs | S |
| FU-121 | 会话里有过聊天后，入站原文不再按事件清理 | 挂 | 要按事件删、同时保住用户聊天，得另设计 | agent24-memory/src/session_log.rs | L |
| FU-122 | 入站摘要深分页没有 (at,id) 索引 | 挂 | 深分页性能。主路径是最新一页 | agent24-store inbound.rs、迁移 | S |
| FU-123 | 只含空白的入站正文会当 prompt 起 run | 修 | 入库只拒绝 None 和空串（inbound/service.rs）。瞬时错误误标 start_failed 是并发边缘，本批不做，做完空白后把原条收窄另开 | agent24d/src/inbound/service.rs | S |
| FU-124 | 群聊和私信每轮只看最新 200 条，限额仍是占位 | 挂 | 漏看是已记录的安全方向。占位要拍板，Mac mini 耗时没量 | inbound/hyphae_source.rs | L |
| FU-125 | 工单、手机遥控、自动回发都还没长出来 | 挂 | 本里程碑明确不做。三条 rg 今天仍无命中，这是守门不是已修 | agent24d/src/inbound | S |
| FU-126 | 过期清理动不了审计摘要和自唤醒 prompt | 挂 | 改 audit detail 会断链。原文已知限制 | inbound_scrub.rs、audit.rs | L |
| FU-127 | 撤销审计在 ack 前断开时可能再写一行 | 挂 | 台账写了本轮不修。pending 只在内存 | capabilities/revoke.rs | M |
| FU-128 | 桥令牌留在 serde Value 里，drop 时不清零 | 挂 | 台账写了本轮不修。内存残留，不是主路径泄漏到磁盘 | agent24-cli/src/main.rs | S |
| FU-129 | Documents 的 declared 仍可能来自两份清单 | 挂 | 台账写了本轮不修。展示来源不一致 | component_routes.rs | M |
| FU-130 | revoke 按路径删除，中间换上的新文件可能被删掉 | 挂 | 目录替换与撤销交错。台账写了本轮不修 | agent24-cli/src/bridge_token.rs | M |
| FU-131 | 服务模式保存默认模型时，121–128 字或格式字符到不了处理器的 400 | 修 | shown() 上限 120（first_party.rs:822），对不上就 not_elevated_operation（elevation_summaries.rs:93）。处理器允许 128（default_model.rs:27）。普通模型 id 不受影响，改动局部 | elevation_summaries.rs、default_model.rs、elevation 测试 | S |
| FU-132 | 旧共享令牌改默认模型没有测试守 | 修 | legacy_unelevatable 已拒绝 PUT /api/v1/models/default（middleware.rs:75）。删掉这一行现有测试仍绿。#1881 评审 Medium | agent24d/src/tests/first_party_http.rs | S |

## 不在 85 条里

`ME4-CODEX-DEBT-1` 到 `8`、以及 `10`（9 条）仍是 OPEN。它们要的是事后补 Codex 审，不是产品缺陷。本次没有逐条重跑 Codex，也没有找到把它们标 done 的记录。全部挂。不改台账。

FU-11 核实结论：在线 `os` 写盘前 daemon 会拒绝未知名字（`os_routes.rs:1082`）。原先「HTTP 不复核」不能再当成已知缺陷。没有对应的修复 PR，所以勾选保持 `[ ]`。

## 出处里没打开的

- #716、#1380：`gh pr view` / pulls API 404。合并提交原文写了这两个号，第二父分别是修测试的 `3360ade26` 和 LocalOnly 的 `2f814620d`。
- FU-43 的上游 1/3 到达率：本次没有再跑 minirelay。
- FU-80：没有再连跑 50 次。代码上交接已改成等在途请求。
- FU-99：没有复跑 AgentEar 真机 E2E。锁内安装看的是当前 `attach_registry.rs`。
- FU-40：没有重跑 `cargo doc`，警告条数不报。

