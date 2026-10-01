# Changelog

All notable changes to Agent24 are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

**行为变化**
- **F4b 入站执行默认冻结**（COMM-5a）：即使配置了 `A24_NOSTR_ALLOWED_NPUBS`,Nostr 入站消息
  也不再自动触发 `agent24d` run——`pollOnce` 仍然轮询、仍然喂给 FU-32 的活性探针
  （`liveness.observe`),只是不再对消息调用 `bridge.handle` / `runToCompletion`。设置
  `A24_NOSTR_F4B_INBOUND=1` 可临时恢复旧行为;后续按入站执行将改走 T01-E 的高层授权。

## [0.5.0] — 2026-09-30

**ME-3 完整收口 + 调度/推理回调 + 模块 SDK 正式落地 + Sin90/Cos72 独立可装**。自 0.4.0
起主要是 ME4-M5（SDK → Sin90 迁移 → Cos72 落地 → wire 文档）与发版前 Codex 补审修复；
v0.4.0 已交付的 ME-3/ME-4/A3 主体本版不再重复列出。

**破坏性变更 / 升级须知**
- **`shell_exec` 与外部 MCP server 子进程不再继承 daemon 的全部环境变量**（ADR-032 J-6，
  FU-103，#571）：改为显式白名单 `CHILD_ENV_WHITELIST`（`PATH`/`HOME`/`USER`/
  `LANG`/`LC_ALL`/`LC_CTYPE`/`TMPDIR`/`SHELL`/`TERM`），不放行任何 `*_KEY`/`*_TOKEN`/
  `*_SECRET`/`A24_*`/`OMLX_*`/`IDORIS_*`。依赖 daemon 环境变量透传的 `shell_exec` 命令或
  MCP server，**升级后可能拿不到之前隐式可见的变量**——需要的变量要么已在白名单内，要么
  在 `~/.agent24/mcp.json` 对应 server 的 `env` 字段里显式声明（该字段此前被静默丢弃，
  本版起生效，只注入给声明它的那个 server，不再进程级共享）。

**新功能**
- **模块 SDK 正式迁移落地**（ME4-M5）：
  - Sin90（独立仓库）线协议金样 `TS.1.0`（出站 `(method,params)` 序列 + 入站回放，Sin90 #73）
    与迁移到 `agent24-os-sdk` v0.1.0 `TS.1.1`（`src/adapter_agent24/` 手写握手客户端替换为
    SDK，真实挂载黑盒零行为变化，Sin90 #74）。
  - **Cos72 最小样例模块首次落地**（独立新仓库 `MushroomDAO/Cos72`，ME4-5.3.x）：
    pilot 规划七件套（Cos72 #3）→ 骨架（manifest + SDK 挂载 + SQLite 迁移 + 事件，Cos72 #4）
    → `mytask` 实体与路由（发布/认领/提交，Cos72 #5）→ 审批发积分（advise + 模块轮询
    `status`，幂等入账，Cos72 #6）与积分账本回放 → 任务完成摘要经 `remember_once`
    写入内核私有记忆（outbox 对账，Cos72 #7）→ 真实挂载黑盒，含与 Sin90 同时挂载时
    互相读不到对方记忆与 schedules 的隔离验证（Cos72 #8）。
- **进程外模块 wire 规范 + Node.js 参考实现**（ME4-5.4.1 / T14，#585）：
  `docs/specs/WIRE-OOP-MODULE.md` 逐方法记录 params/result/18 个错误闭集，每条事实标注
  file:line；`examples/node-module/`（零 npm 依赖的纯 Node 标准库参考模块，接管 fd 3、
  UDS 握手、events/memory 往返）；`rust/apps/agent24d/tests/me4_node_module_blackbox.rs`
  真实黑盒（挂载→代理→事件→记忆，10/10 连跑绿，无 node 时 SKIP 不 panic）。**范围收窄**：
  参考模块只演示 events/memory，scheduler upsert/fired 与 model/approval 只有文档、无参考
  实现，记 FU-104。

**安全修复**
- **ADR-032 J-6 子进程环境变量白名单**（见上「破坏性变更」，#571）：新增
  `agent24-tools::env_whitelist`；`shell_exec` 改为 `env_clear()` + 白名单；外部 MCP
  server 子进程同一白名单，按 server 隔离 `env` 声明。变异验证（删掉 `env_clear`）
  →两条测试变红，失败输出真实打印出本机 shell 里的 token，印证漏洞存在。

**修复（Codex 补审 ADR-032/A3，ME4-CODEX-DEBT-9 收口）**
- **A3 附着模块原子写顺序**（#588）：`attached.rs::write_atomically` 改为先 chmod 0600 →
  写入 → fsync → 再 rename；rename 后目录 fsync 失败只 warn 并返回 `Ok`（rename 已对外
  可见，不能让调用方误以为未提交而跳过 `on_commit`，此前的顺序会导致 DELETE/rotate/
  disable 之后旧 token 在活注册表里仍然有效）。
- **A3 空文件 fail-closed**（#588）：`attached.rs::AttachedStore::load` 遇到存在但为空的
  `attached.json` 现按 malformed 处理返回错误，不再静默当成全新空注册表。
- **A3 关机期间握手竞态**（#588）：`AttachRegistry` 新增 `is_closed()`；关机期间的握手回
  新增的 `HandshakeError::ShuttingDown`（复用既有 `unavailable` ErrorKind，未新增闭集
  条目），不再误判 `auth_failed` 导致模块停止重连。
- **A3 启动/关机竞态**（#588）：`server.rs` 里 `attach_registry_cell` 填好之后立即重查一次
  `cancel.is_cancelled()`，命中就立刻 `revoke_all()`，覆盖「stopping 任务先查到空
  registry 从而跳过撤销」的交错顺序。
- **A3 `serve_attached` 任务泄漏**（#588）：三个 `tokio::spawn` 的 `Teardown`（负责
  AbortOnDrop）改为紧跟 spawn 之后、在 `async move` 块构造之前建好再整体 move 进
  future，future 首次 poll 之前被丢弃时三个任务不再泄漏；#543 钉住的 wire 顺序语义不变。
- **桌面语音面板 AgentEar payload 校验**（#587）：`VoicePanel.tsx` 对 `payload.phase` /
  `speech.state` / `transcript.text` 加类型守卫，恶意/畸形 payload（如
  `{"phase":"__proto__"}`）不再让 React 因非法子节点抛错导致整个桌面 UI 卸载；新增
  面板级 `VoicePanelErrorBoundary`，崩溃只降级本面板。

**已知限制（如实列出）**
- FU-105（C 级，#588 范围排除）：`attach_listener.rs::handle_connection` 里握手成功帧
  `enqueue_raw` 入队与 `attach_kernel_calls` 装配 `KernelCalls` 之间仍有一个极窄窗口，
  该窗口内并发调用 `commands/*` 可能拿到 `503 module_not_ready`——修复需要改
  `AttachRegistry::attach_kernel_calls` 签名且跨两个调用点，本轮范围排除，留作后续。
- 本轮 Codex 补审只覆盖 A3 的 5 个实现 PR（#526/#527/#529/#532/#534），发现的 Medium
  及以上问题均由 #587/#588 修复合并。#515/#523/#528/#543/#544 仍未经 Codex 补审
  （ME4-CODEX-DEBT-10），按 v0.4.0 先例不阻塞发版。
- 其余延续自 v0.4.0 的已知限制（流式未支持、A3 仅 P2、FU-71/73/75/77 等）不变，
  见 `[0.4.0]` 一节，本版未处理。

**其他**
- `docs(open-design)`：叠加 PR 评审 playbook 文档（#572），不属本轮主线，随 main 一并带入。

---

## [0.4.0] — 2026-09-28

**进程外领域 OS + 内核回调面 + AgentEar 附着**。自 0.3.0 起约 150 个合并（约六成是 ME-3 进程外领域 OS 全套，含 T11 破坏性变更）；最后一周补上内核回调面（ME-4 S1/S2）、模块 SDK 原型、AgentEar 附着（ADR-032/A3）与发版前修复。

**⚠️ 破坏性变更 / 升级须知**
- **Sin90 不再编译进 agent24d**（T11，#342）。`agent24-sin90{,-os,-store}` 三个 crate 已删除；`/api/v1/sin90/*` 只在安装了进程外 Sin90 包（`agent24 os install <dir>`，仓库 `iDoris-ai/Sin90`）之后才会经内核代理出现。本版**没有** Sin90 的预编译发布物（那是 ME4-6.0.2 / v0.5.0 的事），需要从源码构建。
- 存储迁移 0007（schedules 的 owner/key/revision/三态暂停 + `schedule_deliveries`）、0008（`module_model_usage`）、**0013（`model_call_timings`，模型调用延迟明细账本）**会在首次启动时自动执行，不可回滚到 0.3.0。
- Schedule 视图新增 `owner`/`effective_enabled`/`disabled_by` 字段，**`action` 变为可空**（模块行）。直接消费 REST 的客户端要判空（桌面端已在 #465 前向兼容）。
- RPC `ErrorKind` 从 17 种变成 18 种（新增 `unavailable`，#451）。

**行为变化**
- 模块经 `_a24/model/complete` 调用推理回调、且**显式**传 `complexity: simple` 时，若由本地 loopback 的 oMLX 服务，会自动带 `chat_template_kwargs.enable_thinking=false`（Qwen3 系推理模型不再先输出大段思考）——AgentEar 语音场景的动机，但对**任何**显式 simple 的模块调用都生效（#544）。未传 `complexity` 与 `complex` 不受影响；非本地或非 oMLX 的 provider 从不发送该字段。

**新功能**
- **进程外领域 OS（ME-3，ADR-031）**：
  - `agent24 os install/uninstall`，含包根即执行边界和原子安装（#157–#161、#170）
  - manifest `spawn` 与两步解析（#156、#167）
  - 版本协商、NDJSON 帧、`initialize` 握手（#162、#164、#166）
  - 受约束代理（#173）
  - Supervisor 持有并监督模块进程，热 disable 先撤后杀（#171、#175、#178–#184）
  - 回调通道（#176、#177），入站改走 Unix 域套接字（#192）
  - 能力授予与事件回调（#199）、模块审批 gate/advise/status（#201、#203）、启用准入校验（#196）
  - 请求生命周期信号、权威配额、分页游标、OOP 记忆挂载（#205、#207、#210–#212、#217、#218、#224、#225）
  - 运行失败分类（#191）、包在运行中被改或被删（#193）、连接复用竞态与可操作报错（#194）
  - 停机可观测：`GET /api/v1/shutdown`、`daemon status` 的停机段（#187–#189）
  - 仓外包端到端黑盒（#262）
- **ME-4 内核回调**：
  - 调度回调 `_a24/scheduler/*`，含模块行所有权、REST 护栏、fired 投递泵、重启续投（#444、#447、#453–#456、#462、#465、#469–#472、#478、#479、#487、#488、#500–#502、#504）
  - 推理回调 `_a24/model/complete`，含 manifest `model_access`（缺省 `local_only`）、准入与公平、按模块用量、`GET /api/v1/usage?module=`（#446、#448、#451、#457、#461、#464、#505、#506、#510–#512）
- **模块 SDK 原型**：新 crate `agent24-os-sdk` 0.1.0（五个客户端：events/memory/approval/scheduler/model，外加 fired）和 `agent24-os-fd`（#514–#516）。它是原型，接口可能变。
- **AgentEar 附着（ADR-032 A3，P0–P2 单轮端到端）**：
  - 附着模块注册：`POST/GET/PATCH/DELETE /api/v1/attached`，CLI `agent24 os attach add/list/revoke`，token 只存哈希（#526）
  - 握手、代际、附着监听与生命周期（#527、#529）
  - 反向命令 `POST /api/v1/os/{name}/commands/{speak|stop_playback}`（#532）
  - 桌面端「语音」面板，转写只在内存（#534）
  - 需要 AgentEar ≥ v0.25.2，推荐 v0.26.1（附着 token 改存 `~/.agentear/agent24/token`，升级不再弹钥匙串授权提示）
- **桌面端**：
  - 「OS 模块（内核挂载）」视图（FU-90，#528）
  - sidecar 生命周期、进程树所有权、有界就绪与健康检查（#253–#255、#259、#260）
  - 新 logo：替换为 iDoris 像素风女孩（#541）
  - 顶栏真实默认模型名、回复耗时后缀（首字/总计 ms）、`model.call` WS 事件、语音面板延迟展示（#544）
- **CLI**：能力发现 fail-closed（#226、#227）
- **可观测性**：`model_call_timings` 原始账本（迁移 0013）+ `GET /api/v1/timings`（可按 source/since 过滤）+ `GET /api/v1/timings/summary`（count/p50/p95/max），同时记录 AgentEar 自报的分段耗时（record/asr/llm/tts/…），不落任何 prompt/response/transcript 内容（#544）

**修复**
- 模型路由：回环判定改用 `reqwest::Url`，Local provider 不走代理、不跟随重定向（FU-72，#448）。CLI 和 worker 访问本机时不走代理（FU-74，#473）。
- Nostr 入站加正向活性信号（FU-32，#147）。
- 启动时清掉 `write_durable` 崩溃后留下的孤儿临时文件（FU-67，#197）。
- 桌面端：
  - 默认模型不再选中生图、语音或嵌入模型（FU-89）
  - dev 渲染地址读 `VITE_DEV_SERVER_URL`（FU-91）
  - 侧栏显示真实端口（FU-93）（均 #528）
- HOME 路径较长时，回调 socket 回退到有防劫持检查的 `/tmp/a24-run-<hash>`（FU-92，#528）。
- **A3 握手→命令可用竞态（C1）**：握手成功行改走出站队列且在 `attach_kernel_calls` 装配 `KernelCalls` 之前入队，消除窗口期内 `commands/*` 误判 503 `module_not_ready` 的竞态（很可能是 AgentEar 真机 E2E 首轮失败的根因，#543）。
- **FU-83 探针原子写（C2）**：两处测试探针改 `.tmp` + `os.replace` 原子写，消除 CI 偶发失败（#543）。
- **SDK `FiredBody` 宽松解析（E4）**：去掉 `deny_unknown_fields`，避免内核以后给 fired body 加字段时，所有旧 SDK 编译的模块回 400、定时任务静默失败（#543）。
- 若干测试稳定性修复（#186、#214、#476、#482）。

**文档**
- ADR-031（进程外协议）、**ADR-032（AgentEar/iDoris 接入）已接受**，P0–P2 已交付
- ME-3 设计与 SPEC-ME3 改写
- PLAN-ME4 与 S1/S2/S3 设计
- 2026-09-27 现场演示记录（#523）
- L3 轨迹改为 ATIF v1.8 交换格式（#540）
- README：两套扩展机制与接口面（#149），本版按 T11/ME-3/A3 现状重写（架构图、组件表、CLI 列表、里程碑表）
- iDoris 集成设计（#98）、SOAK-F5 实测（#154）

**AgentEar 兼容性**：需要 AgentEar ≥ v0.25.2，推荐 v0.26.1（附着 token 改存 `~/.agentear/agent24/token`，0600 权限，升级不再弹钥匙串授权提示）。

**已知限制（如实列出）**
- **评审深度**：ME4 全部实现和 A3 系列只经过本地 Opus 与 PR-Daemon 评审，**没有经过 Codex 对抗评审**（`ME4-CODEX-DEBT-1~9`，额度 09-29 19:28 恢复后补审）。
- 推理回调和反向代理都**不支持流式**，非流式首字约 4.9s。P3（流式）排在 v0.5.0 之后。
- A3 只做到 P2：`proposal`/`confirm_reply` 只展示不执行；gate 可执行集合为空。
- AgentEar 真机 E2E 5 次里 1 次首轮失败，很可能已由本版 C1（#543）修复，待复验（`docs/agent/followups.md` FU-99）。
- **FU-71**：REST 和 self-wake 路径上的 cron 星期字段按 1=周日解释，`0 7 * * 1-5` 实际是周日到周四，会**静默错一天**。
- FU-75：`/api/v1/usage` 的 `cost_usd` 恒为 0.0。FU-77：`remote_allowed` 模块不能逐次收窄到 local-only。FU-73：卸载模块留下的 schedules 不会自动清理。
- `protocol/openapi.yaml` 没有收录 `/api/v1/os*`、`/api/v1/attached*`、`/commands/*`、`usage?module=`。它还保留着 7 条 `/sin90/*`，这些路径现在只在装了 Sin90 包时存在。
- F5 7×24 泡测仍未跑。FU-34（上游二进制改名 `hyphae`，默认 `A24_SPEAKER_BIN=agent-speaker`）和 FU-38（默认 relay `relay.aastar.io` 下线）让 Nostr 渠道开箱不可用。FU-29、FU-31 延续。
- `agent24-os-proto` 没有拆 kernel/module feature，SDK 用户要连带编译整个 proto（FU-86）。
- 本版发布未等 Codex 补审（jason 拍板，2026-09-2x）；本地门（fmt/clippy/test/pnpm）与 pre-pr-check 全绿是发版前提，Codex 补审后若发现 High 级问题将出 v0.4.1。

## [0.3.0] — 2026-09-02

M-E：领域 OS 成为一等公民，M-D 记忆底座重做，Nostr 渠道收官。
自 0.2.1 起 72 个合并。

### 领域 OS（可插拔架构，ADR-029）

- `DomainModule` + `KernelCtx` 契约 crate；内核不再按名字认识 Sin90（#127）
- 内核侧挂载器 + Sin90 成为第一个 `DomainModule`：自己的 DB、自己的路由
  命名空间、自己的 event module 名（#131 #132）
- 配置驱动的领域 OS 注册表 + `agent24 os` CLI（daemon 拥有注册表）（#133 #134）
- `domain-os.yml` 清单带 `deny_unknown_fields` —— 拼错字段名报错，而不是
  静默的空能力集

### 记忆（M-D 重做：可进化 / 可替换 / 可组合）

- MD-1 Condenser 缝 + 崩溃重放，签名已冻结（#113 #116）
- MD-2 EventStore（情节权威）+ ArtifactStore（markdown-CAS）+ 双谱系对账，
  checksum 移动检测、**无静默删**（#114 #115）
- MD-3 AssertionStore 双时相（矛盾=新版本非删）+ FTS5 Retriever，owner 隔离
- MD-4 MemoryWriter 写门：WebFetch/Unknown 默认不落持久，投毒语料测试
- MD-5 Consolidator：幂等 + **增量 == 全量重跑**
- MD-6 向量检索机制 + 换模型 reindex 状态机 + FTS 兜底
- MD-7 知识层：层级合并 + 触发注入 + **审核门控 auto-memory inbox**
- MD-8 长任务符号轨迹：压缩率 >99% 且 **100% 可恢复**
- **F1 `ScopedMemory`**：两个领域 OS 不再共享记忆底座（#139，六轮对抗复审）
- **F8 所有权改为 (org, space)**，org 成为一等实体；分区目录 + v1→v2 re-key，
  全程一个事务（#140，六轮对抗复审）
- F5 两处排序 tie-breaker + 一条比字面更弱的 CHECK 约束（#138）

### 渠道

- **F4 Nostr 收官**（#85–#95）：出站 register/say/search + 入站 gated +
  npub 白名单；与 agent-speaker 双向真联调；strfry 真 NIP-33 relay 覆盖定论
- **两条会让 7×24 静默失效的缺陷**（#142）：
  - Nostr 桥的 `execFile` 无 deadline —— 子进程挂起会让入站轮询循环**永久停摆**
    （`tick()` 是串行自调度，promise 不 settle 就没有下一轮）。加 60s deadline
    + SIGKILL。**注意这只关上了「子进程挂起」那一支，不含 relay 静默，见下方
    已知缺口 FU-32。**
  - 微信会话映射改为原子写：temp → `fsync` → `rename` → `fsync` 父目录，
    外加一代 `.bak` 回退与损坏时的回退读取。**限定（FU-31）**：macOS 的
    `fsync` 不保证驱动器刷新自身写缓存（那需要 `F_FULLFSYNC`，Node 不暴露），
    所以这关上的是 page cache 那个窗口，**不是「抗断电」**

### 生态

- 模块发现服务 + 浏览过滤（#90 #91 #94）

### 文档

- ADR-030 + SPEC-ORG-SPACE + M1 规划层（#141）
- 四份 vendor 研读笔记 + `docs/laws/` + Skill 分发规格 + 三档能力边界（#143）

### 已知缺口（如实列出，不粉饰）

- **FU-32（A 级）**：入站 relay 静默 —— 桥无法区分「收件箱为空」与「relay
  连接已死」。#142 只关上了子进程挂起那一支。**F5 泡测前必须处理。**
- **FU-29**：Nostr 回复是 **at-most-once**，且子进程超时被杀时**投递状态不确定**
  —— 一条回复可能被静默丢弃，且不会重试（盲重试可能重发）。这是本次发布的 F4
  渠道的用户可见行为。修法需持久化 outbox + 发送侧幂等键。
- **FU-31**：`fsync` 在 macOS 上不含 `F_FULLFSYNC`，见上方微信桥条目的限定。
- F5 7×24 泡测**尚未跑过**（物理任务）
- `agent24 os` 只有 `list`/`enable`/`disable`，**没有 `install`** —— 第三方
  领域 OS 仍需编进二进制（ME-3 未开工）
- 没有 web UI

### 一个会被当成 bug 的正常现象

`agent24 os list` 在 v0.3.0 里仍显示 `sin90 v0.2.1`。**这不是漏改的版本号** ——
领域 OS 模块有**独立的版本线**：真值来源是 `agent24-sin90-os/domain-os.yml` 的
`version` 字段，`identity_matches_the_manifest` 断言 `MANIFEST_VERSION` 等于解析
出的 manifest，`os list` 显示的是**模块版本，不是产品版本**。此前两者恰好都是
`0.2.1` 只是巧合，本次发布把巧合打破了。

## [0.2.1] — 2026-07-26

Hardening of the H9 explorer subagent (found in an adversarial re-review of
v0.2.0) plus protocol/changelog corrections.

### Fixed

- **Explorer is now truly network-free** (H9 security): the read-only registry
  the `explore` subagent runs against no longer includes `http_fetch`. `Read`
  class means "no side effect on the machine", not "no egress" — a GET could
  still send bytes an `fs_read` just returned to an arbitrary URL, which in an
  ungated, model-spawned helper is an exfiltration channel. The explorer now
  has `fs_read` only; a network-capable researcher, if ever wanted, must be a
  separate gated tool.
- **Explorer fanout is bounded** (H9): a single model turn is capped at 16 tool
  calls (mirroring the main loop) and each `explore` call has a 120s wall-clock
  ceiling, so no input shape can make one exploration run for hours.
- **Explorer panics are contained** (H9): the sub-loop runs in its own
  supervised task; a panic becomes a `ToolError` instead of unwinding past the
  caller and leaving a dangling `running` tool-call row.
- **Empty exploration answers are distinct** (H9): an explorer that produces no
  text returns an explicit sentinel, so the caller can tell "found nothing"
  from "produced nothing".
- **Protocol doc**: `approve_for_target` (H4) is now documented in the
  `Decision` type in `openapi.yaml`, and the note that `approve_for_session` is
  not offered for `external` tools is recorded there.

## [0.2.0] — 2026-07-26

**M-H — the human boundary.** Everything a person needs to stay in control of an
agent that runs while they're away: what needs asking, how far a "yes" reaches,
and what an error actually tells them. Studied from Andrew Ng's OpenWorker and
put through an adversarial review before landing (see
`docs/reference-notes/openworker.md`). Protocol changes are additive (a pre-0.2
client keeps validating), with one deliberate behaviour change: `external`-risk
tools no longer offer the broad `approve_for_session` grant — see H4.

### Added

- **Declared risk classes** (H1): a tool's `risk_class`
  (`read`/`write_local`/`exec`/`external`) is now the one property the approval
  path reads, and `requires_approval` is derived from it — two hand-maintained
  lists can no longer drift. Additive protocol field; gating outcomes are
  byte-for-byte unchanged. Each class earns a different exemption path, which is
  what the next two features are built on.
- **User-local risk overrides** (H2): a glob rule the machine's owner writes to
  relax (or tighten) an individual tool's class — the release valve that makes
  MCP's conservative `external` default actually usable. A user may correct a
  guess (third-party `external`) but never overrule knowledge (a builtin's
  class). The store is user-local and is never written by a module, persona, or
  MCP server. `GET/PUT/DELETE /api/v1/tool-overrides`.
- **Target-scoped standing grants** (H4): "always allow" for an external tool
  now binds to an **exact target** (`send → #ops`), owned by the session or the
  schedule that fired the run, matched exactly and revoked when its schedule is
  deleted. The broad whole-tool `approve_for_session` is no longer offered for
  external tools — the safe option is the only option. `Approval.standing_target`
  labels the choice; `GET/DELETE /api/v1/standing-grants`.
- **Read-only explorer subagent** (H9): the `explore` tool delegates a bounded,
  read-only investigation to a fresh sub-agent with its own context, so the
  dozens of file reads it takes to answer "where is X handled?" never crowd the
  main transcript. Read-only and no-recursion are structural — the sub-run's
  registry simply never contains a write/exec tool or `explore` itself.

### Changed

- **Provider errors say what to do** (H12): `openai returned HTTP 429` becomes a
  named cause plus the provider's own message — bad key vs wrong model vs spent
  quota. A rate-limited or 5xx primary now falls through to a healthy backup
  provider (previously every HTTP error stopped there); auth/model errors still
  stop, since a different provider can't fix a config mistake.

## [0.1.0] — 2026-07-24

The first release of the **Rust-core Agent24**: a 24/7 personal/community
workflow agent (not a coding agent). The Electron desktop shell now ships the
Rust `agent24d` daemon as its default backend, speaking the frozen v1 protocol.

### Added — Rust core (agent24d + agent24 CLI)

- **Domain state machines + store** (C1): exhaustive Run / Approval / ToolCall
  state transitions; SQLite persistence via sqlx with `BEGIN IMMEDIATE`
  transactions and a hash-chained (sha256) tamper-evident audit log.
- **Agent loop v1** (C2): `POST /api/v1/runs` → background execution with
  first-class cancellation woven through every await point; full WS lifecycle
  events; fail-closed orphan-run sweep on startup.
- **Tool system** (C3): `Tool` trait + registry with a fixed dispatch pipeline
  (capability whitelist → approval gate → timeout). Builtins: `http_fetch`
  (SSRF-guarded, resolve-then-pin against DNS rebinding), `fs_read`/`fs_write`
  (cap-std dirfd-anchored, beneath-only traversal), `shell_exec` (argv
  execution, never a shell string).
- **Approval system** (C4): fail-closed approval broker — every non-answer path
  (timeout, run-cancel, dropped channel) resolves negative; the store row is
  the single arbiter; `approve_for_session` grants scoped to (session, tool);
  runs enter `awaiting_approval` while a decision is pending.
- **Wall-clock scheduler** (C5): cron / every / at schedules with pre-advance
  (crash cannot double-fire), skip-missed (no replay bursts), and fail-safe
  disable after 5 consecutive failures. Timezone/DST-correct cron.
- **`agent24 tui`** (C6): a ratatui operator client — runs · event stream ·
  approval queue — with WS streaming, auto-reconnect, and REST reconciliation.

### Added — Desktop

- **Runs / Schedules / Approvals pages** (C7): live REST-polling views;
  Schedules form with an instant next-fire preview; desktop notifications on
  new approvals rendering the server's `available_decisions`.

### Changed

- The desktop shell defaults to the Rust `agent24d` backend
  (`AGENT24_BACKEND=node` opts back into the legacy mock).

### Protocol

- Contract-first v1 API frozen in `protocol/` (openapi.yaml +
  events.schema.json), enforced by dual-backend contract tests and a CI
  zero-drift gate.
