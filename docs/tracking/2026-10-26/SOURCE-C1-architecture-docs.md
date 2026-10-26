# C1 簇调查：架构、设计与技术方案（原料）

调查基线：main `3a433bb0`（2026-10-26）；集成分支 `origin/ab/{memory-p1,kernel,weknora,documenting,office-assistant,comm,decide,idoris,maint}`。全程只读。
对码方式：`cargo metadata`、`Cargo.toml`、`rg`/`grep` 与读源码。记录里的「日期」= `git log -1 --format=%cs` 的最后修改日；「行」= `wc -l`。
约定：没逐条对过码的文档，在「核验」里写明「未逐条对码」，不冒充已验证。

## 0. 总览与计数

范围内文档共 **114 份**：main 上 87 份（范围清单内 86 份，其中含 3 份 `docs/documenting/adr/ADR-DOC-*`；另加 `docs/decision.md`），只在集成分支上的 27 份。
分支上对 main 已有文件的较新/较旧变体（ADR-K1-02、ADR-DOC-02、COMM-HYPHAE、ME4-S2c 等）不重复计数，在对应记录里注明。
下面分 **R01–R57 共 57 条记录**（同主题合并，记录里写明份数）。

按**文档份数**计（一份文档取一个主状态）：

| 状态 | 份数 | 说明 |
|---|---|---|
| CURRENT | 71 | main 45 + 集成分支 26（多为 DRAFT / 设计稿）。其中 ARCHITECTURE-LAYERS、decision.md、ADR-K1-03(main)、SPEC-ME-FOLLOWUPS 另含局部不符，见 §4、§5 |
| HISTORICAL | 35 | main 34（已落地设计稿 FU/T7/T8/T8.5、OD 冻结包与落地指南、联调记录、LOOP/TASKS 等）+ 分支 1 |
| SUPERSEDED | 5 | ADR-016-consume、SPEC-MD-ME、SPEC-MEMORY、SIN90-domain、iDoris-integration-and-entry-router |
| STALE-WRONG | 3 | README 架构部分、laws/APPROVAL（L-APPR-5/8）、SPEC-001 |
| 合计 | 114 | |

按**记录条数**计（57 条）：CURRENT 38、HISTORICAL 9、SUPERSEDED 5、STALE-WRONG 3；R46（docs/comm，6 份 HISTORICAL + 2 份 CURRENT）和 R57（副本说明）不归单一状态。

decision.md 内部 37 条 ADR（ADR-001…036 + 另文 ADR-026）：文内正确标 Superseded 的只有 ADR-018、ADR-023；按代码实际已被取代、却仍标「✅ 采纳」的有 ADR-014、015、016（模块 worker_thread 沙箱部分）、017（“加密 SQLite”）、020、022、024、025（见 §4）。

## 1. 逐文档记录

### 1.1 主架构与根文档

### R01 docs/ARCHITECTURE-LAYERS.md
- 分支：main（kernel/comm/decide 分支上是旧基线副本，无新增内容）。日期 2026-10-15，313 行。
- 主题：L0–L7 分层、真实依赖图、扩展面四类外挂、安全边界、未完成清单。
- 状态：CURRENT（含 5 处局部不符，见 §5-A）。
- 核验：写于 2026-10-03，文头自称「经 Codex 对照代码审查」；2026-10-15 补过 hw/decide。依赖表、`required_operation`、auth 模式、`KERNEL_GRANTS`、`workspace_id: None`（`rust/apps/agent24-cli/src/acp.rs:139,179`）、`CREATIVE_SESSION_PARTITION='persist:agent24-creative'` 都与代码吻合。
- 关键决定：无编号决定，是导航文档。重要事实：默认 `legacy_single_token`；capability 模式未接入产品路径；`agent24-sidecar-host`/`SidecarManager` 未接线；`agent24-worker` 无消费者。
- 链接：内部链接全部有效。外部：`#661`、`#663` issue。

### R02 docs/agent/architecture.md
- main；2026-09-24；111 行。
- 主题：M1 记忆相关的契约与不可动摇的边界（Authorizer 签名、ME-4 新增边界、运行形态）。
- 状态：CURRENT。
- 核验：`Authorizer`/`AccessRequest`/`Actor`/`Op`/`ActiveScope`/`ModulePrivateOnly` 在 `rust/apps/agent24d/src/authz.rs`，与文档签名一致；`CanonicalSession` 已降为遗留导入（`rust/crates/agent24-agent/src/session_memory.rs:261` `import_legacy`）；`rekey_os_partition` 在 `rust/crates/agent24-memory/src/lib.rs:891`；`LOCAL_USER` 仍在用。
- 关键决定（均未标批准状态，但被 laws/MEMORY 引用）：判定接缝签名六项；执行点是句柄发放；CanonicalSession 降为投影；迁移走目录、不得 `LIKE` 前缀；ME-4 边界九条（UDS 上游唯一内核到模块通道、`/_a24/` 保留路径、`Generation` 准入、用户暂停高于模块、模块调模型默认 LocalOnly、SDK 不解析帧、业务真相只在模块自己的 SQLite）；单 daemon、单 SQLite、单用户。
- 链接：`spec.md`（同目录）——docs/agent/spec.md 未核（不在本簇范围）。

### R03 AGENTS.md
- main；2026-10-08；66 行。
- 主题：给编码代理的工作约定：`ab/<方向>` 集成分支、task PR ≤300 行、门禁、Codex 沙箱白名单 4 项、commit author 规则。
- 状态：CURRENT。
- 核验：与用户记忆里的 ab/* 流程一致；示例 `ab/m1-memory` 已不存在（M1 已合 main，#720），属无害过期示例。kernel/comm/decide 分支上的 AGENTS.md 是旧基线，main 的更新。
- 关键决定（jason 2026-10-07 拍板）：每方向一条 `ab/<方向>` 集成分支；约 10 个 task PR 开一次 release PR；安全/权限/数据写入/迁移/wire 类 task PR 必须等 PR-Daemon APPROVE；集成分支每周至少吸收一次 main（merge 不 rebase）；ruleset `ab-integration` 按精确分支名列出；禁止在 Mac mini 上执行任务；一律 pnpm。
- 链接：无失效。

### R04 README.md（架构部分，第 20–280 行）
- main；2026-09-29；282 行。
- 主题：Rust Core + Polyglot 总图、核心组件表、领域 OS vs 能力模块、LLM 运行时、里程碑进度。
- 状态：STALE-WRONG（局部，见 §5-D）。
- 核验：
  - 「`KERNEL_GRANTS = {Events, Memory}`」——代码是 `[Events, Memory, Approval]`（`rust/apps/agent24d/src/domain.rs:75`）。
  - 「M-C 发布 … v0.4.0（当前）」——`rust/apps/agent24d/Cargo.toml`、`apps/desktop/package.json` 都是 0.5.1，最新 tag `v0.5.1-preview.20261016.2`。
  - 「决策日志 ADR-001 ~ ADR-030」——decision.md 已到 ADR-036。
  - 核心组件表缺 `agent24-comm`、`agent24-hw`、`agent24-decide`、`agent24-documents`、`agent24-sidecar-host`。
  - 「Python ML Worker（规划）」「LoRA 一级今天没有 provider」与代码一致。
- 关键决定：无。
- 链接：`docs/WORKSTATION_PLAN.md` 等都存在。外部：`vendor/xiaoheishu` submodule。

### R05 docs/decision.md（ADR-001 … ADR-036 汇总，逐条见 §3）
- main；2026-10-15；1513 行。kernel/comm/decide 分支的 decision.md 是旧基线（比 main 少）。
- 主题：全部架构决策日志。
- 状态：CURRENT（作为日志）。文内 ADR-001…025 里有一组与现状不符，逐条见 §3 和 §5-C。
- 链接：`ADR-026-rust-core-polyglot.md`（同级，存在）；外部多为 PR/issue 号。

### R06 docs/ADR-016-consume-auraai-packages.md
- main；2026-07-24；150 行。
- 主题：Agent24 改为消费 `@auraaihq/*` 包（core/idoris/memory/sdk）。
- 状态：SUPERSEDED，被 ADR-026 §6「ADR-016 范围收缩」取代（`docs/ADR-026-rust-core-polyglot.md`）。
- 核验：文头仍写「进行中（migration 分支活跃）」。`package.json`、`apps/*`、`packages/*` 无任何 `@auraaihq` 依赖；文中的 `src/backend/{llm-gateway,capability-registry,module-installer,module-state,server}.ts` 现在在 `packages/node-daemon/src/` 里原样存在（没被替换）。
- 关键决定：改为消费 `@auraaihq/*`（已摒弃）。编号冲突：与 decision.md 里的 ADR-016（模块安全与权限）同号不同题。
- 链接：无失效。

### R07 docs/ADR-026-rust-core-polyglot.md
- main；2026-08-21；251 行。
- 主题：Rust 内核 + Polyglot Worker 架构（取代 ADR-023）。
- 状态：CURRENT（ACCEPTED，2026-08-21 jason 确认）。
- 核验：Rust crate 布局、REST+WS、`--port 0`、TUI、`ModelProvider`、取消令牌、fail-closed 审批都已落地。**未落地**：`agent24-workflow` crate（不存在）、`agent24-node-host`（不存在，仍是 `packages/node-daemon` 作 mock/参考实现）、Python ML Worker 服务端、`pgl.yml` manifest 钩子。
- 关键决定（11 条）：Rust 为唯一核心运行时；REST+WS；CLI 两种模式；Electron 继续做壳；TS 模块经 Node Host 接入；Python 仅作 ML Worker；OpenAPI/JSON Schema 单一来源；内核不依赖任何 Agent Framework；Tauri 不设为当前目标；不做临时 TS 内核能力；提供 TUI。§6.5 十一条设计硬约束（取消一等公民、审批请求-响应、fail-closed、Guardian 对接 L1 小模型、Tool registry trait、cron pre-advance 等）。
- 链接：`protocol/PGL/CONTEXT.md` 失效（在 Brood 仓库，仓库外）。

### 1.2 docs/adr/ADR-K1-*（内核 K1，main 上）

### R08 docs/adr/ADR-K1-01-module-tool-contract.md
- main；2026-10-16；160 行（kernel 分支同）。
- 主题：进程外模块的工具声明、授权交集、有界调用、`result_unknown`、不自动重试、审计关联。
- 状态：CURRENT。ACCEPTED（2026-10-08 jason 确认，PR-Daemon APPROVE）。
- 核验：文内「现状与差距」表按 K1-1 起草时 `ab/kernel` 行号写，是历史快照。实现已在 main：`rust/apps/agent24d/src/module_tool_runtime.rs`（K1 随 #1022 入 main）。
- 关键决定：工具身份 `<module_id>.<operation>`；可调用权限 = manifest 声明 ∩ 宿主策略 ∩ 用户/组织授权 ∩ 实时可用；第三方默认高风险；内核不自动重试且 `run_id`/`tool_call_id` 不当幂等键；稳定错误码 11 个（`invalid_input` … `result_unknown`）；工具级 `cancelled` 不取消整个 run；审计留存 180 天。
- 链接：`ADR-K1-02/03` 同目录有效。

### R09 docs/adr/ADR-K1-02-session-dataflow-security-modes.md
- main 2026-10-16，110 行；**ab/kernel 2026-10-10，136 行（更新）**。
- 主题：来源标记与政策合并、`LocalOnly`/`CloudAuthorized` 两态、出口门清单、`has_remote()` 过渡门撤除条件。
- 状态：CURRENT。ACCEPTED。main 上的副本落后于 ab/kernel（kernel 多一节 §2.1.1 和反例 9–11，见 §3 K1-02 条）。
- 核验：文内「现状」行号为 K1-2 起草时快照；`TaskProfile::default()` 在主对话路径仍存在（`rust/apps/agent24d/src/routes.rs:569`、`rust/crates/agent24-agent/src/subagent.rs:231`），与「ID-1 未做」一致。
- 关键决定：只有 `LocalOnly`/`CloudAuthorized`，未知一律 `LocalOnly`；云处理默认关闭、首发是否提供另定；`has_remote` 过渡门在 K1-6a **和** K1-6b 都落地并通过 #708 验收后才撤。
- 链接：`docs/laws/PRIVACY.md` 失效（提议中的文件，尚不存在）。

### R10 docs/adr/ADR-K1-03-enable-authorization-trust-revocation.md
- main 2026-10-21，129 行；ab/kernel、ab/memory-p1 有链接修复版（仅 8/7 行差异）。
- 主题：启用时逐工具授权、有效期、撤销、第一方/第三方信任、与 OD capability 的接口、K1-6a.5 可逆草稿补遗。
- 状态：CURRENT。ACCEPTED。
- 核验：main 版有 **13 个失效内部链接**：`../rust/...`（应为 `../../rust/...`）7 个；`adr/ADR-K1-02-...`、`laws/APPROVAL.md`、`laws/CONTEXT.md`、`laws/MEMORY.md`、`open-design-workspace/design/ADR-005-...` 5 个（相对路径少了 `../`）；`docs/laws/PRIVACY.md` 1 个（提议中文件）。ab/kernel 与 ab/memory-p1 上的版本已修链接，且把代码链接固定到提交 `9131bc7c`。「L-APPR-1..7」在 kernel 版是旧口径（main 的 laws 已有 L-APPR-8）。
- 关键决定：许可由宿主签发、逐工具、有明确到期；安装/更新/签名不等于同意；更严的组织/来源限制优先；撤销串行化、在途调用复验；第一方 Documenting 默认安装即启用但首次使用前确认一次；K1-6a.5：可逆草稿写入的免逐次审批资格。

### R11 docs/adr/ADR-K1-04-audit-and-shared-authorization.md
- main 2026-10-21，122 行（kernel 分支仅 1 行差异）。
- 主题：模块工具调用的最小内核审计（元数据，不记正文）、180 天保留、用户查看导出、UI 与 Agent 共用授权权威。
- 状态：CURRENT。ACCEPTED。
- 核验：「`append_audit` 接受任意 JSON、`detail` full fidelity」是 K1-4 起草时的现状；代码里 `rust/crates/agent24-store/src/audit.rs` 的字段白名单是否已收紧——**未核**。
- 关键决定：每次调用形成可关联审计（`run_id + tool_call_id + module_id + operation_id`）；派发前须有可写审计能力；审计写失败不吞；UI 与 Agent 共用同一资源/动作授权权威。

### 1.3 docs/laws

### R12 docs/laws/README.md
- main；2026-10-21；86 行。状态：CURRENT。
- 主题：法律规则（RFC 2119）、与 PR 的闭环（改法律必须单独 PR）、现有三部法律、迁移状态。
- 关键决定：法律编号永不复用；「今天靠什么保证」一栏必须诚实；改变可观察行为的 PR 须点名所涉法律。
- 链接：无失效。

### R13 docs/laws/CONTEXT.md
- main；2026-09-02；115 行。状态：CURRENT（未逐条对码）。
- 主题：L-CTX-1..4：指令文件只在已授权根内解析；注入上下文的来源可点名；兼容输入不改写用户文件；运行记录不是完成证据。
- 备注：README 写「L-CTX-1/2 要到 Cos72 才有真实代码路径」。

### R14 docs/laws/MEMORY.md
- main；2026-09-20；141 行。状态：CURRENT。
- 主题：L-MEM-1..7：模块拿不到原始 store；API 不接受作用域参数；无跨 owner 全局维护；进 key 的只有不可变 ID；分区迁移单事务；re-key 只搬 events+checkpoints；隔离主张不得超过机制。
- 核验：`rekey_os_partition` 与 `authz.rs` 均在码中，与 R02 一致。

### R15 docs/laws/APPROVAL.md
- main；2026-10-21；161 行。（ab/kernel 上是 2026-10-08 之前的旧版，少 L-APPR-8 与 2026-10-21 例外。）
- 主题：L-APPR-1..8：override 仅用户本地、只能收紧内置、external 不给宽泛会话授权、常驻授权窄到确切目标、授权挂在 schedule 上、fail-closed 读权限不存储、工作区写入。
- 状态：STALE-WRONG（局部：L-APPR-5、L-APPR-8 的「今天靠什么保证」一栏）。
- 对照：
  - 文档（L-APPR-8）：「今天没有这条机制。`fs_write` 仍被工作区白名单直接拒绝根外写入，不会弹出『仅本次 / 一直允许这个目录』。`approve_for_session` 仍会放行同一会话内后续的 `fs_write` 与 `shell_exec`。」；L-APPR-5：「`exec` 不得提供会话级授权这一句，今天仍被 `approve_for_session` 对 `shell_exec` 的快路径违反」。
  - 代码：`rust/crates/agent24-policy/src/lib.rs:41-80` `decisions_for`——`approve_for_session` 只对 `RiskClass::Read` 提供（`session_wide_grant_allowed`，:159-161）；`exec`/`write_local` 不再提供；`write_decisions`（:163-）对根外 `fs_write` 提供 `approve_for_target`（目录授权）；`rust/crates/agent24-policy/src/write_scope.rs`（`classify_builtin_write`、`directory_grant_forbidden`）已实现禁区判断（2026-10-21/22 提交）。仍缺：「已确认的工作区根」免询问（lib.rs 注释自述 `A confirmed workspace root is not wired yet`）。
  - 同文另一处：「生产装配传入 `risk: External`、`reversible_draft_declared: false`、`draft_verification: None`（`server.rs:1603-1604`）」——属实，实际位置是 `rust/apps/agent24d/src/server.rs:1598-1604`，行号漂移 ≈5 行。
- 关键决定：见 §3。
- 链接：无失效。

### 1.4 docs/design（按主题分组）

### R16 docs/design/A3-ATTACHED-MODULE.md
- main；2026-10-15；446 行。状态：CURRENT（v2 冻结，已实现）。
- 主题：附着式进程模块（AgentEar）。A3-1…A3-4 已合并，随 v0.4.0 发布。
- 核验：`rust/apps/agent24d/src/attach_registry.rs`、`attach_listener.rs`、`attach_commands.rs`、`attached_routes.rs` 与 `rust/crates/agent24-os-proto/src/attach*.rs` 存在。
- 关键决定（ADR-032 配套）：用户自启、注册 token 连附着 socket、每模块同时最多一代（第二条连接 `busy`）、反向命令走同一连接。
- 链接失效：`docs/agent24-embedding.md`（AgentEar 仓库）；`rust/apps/agent24d/tests/fixtures/fake_agentear.py` 不存在。

### R17 docs/design/COMM-HYPHAE.md
- main 2026-10-06，489 行；ab/comm 2026-10-10，493 行（多 8 行，已知更新）。
- 状态：CURRENT（r2 冻结，已实现）。
- 核验：`A24_HYPHAE_BIN`（`rust/crates/agent24-comm/src/router.rs`）、`--notify=false`（`daemon.rs:750`）、`/api/v1/comm/*`（`comm_routes.rs:336`）、`hyphae.lock.json`（source_sha `144555ca…`，v0.26.3）均存在。文中基线 `a4aa606e`/`bf3322c` 是历史基线。
- 关键决定（D1–D8，jason 2026-09-30）：D1 通信服务在 agent24d 内，`/api/v1/comm/*`；D2 Hyphae daemon 作子进程，固定 `--notify=false`；D3 专用 HOME `<state_dir>/comm/hyphae-home/`，旧 `~/.hyphae` 只能显式导入（复制）；D4 keystore 口令存系统钥匙串，只经 `--password-stdin`；D5 F4b 冻结，COMM 基础通信 zero-run；D6 版本靠 lock + sha256，不写死；D7 任务编号 COMM-*；D8 保留 `A24_SPEAKER_BIN` 兼容。
- 链接：`apps/agent24d/tests/comm_blackbox.rs` 失效（路径少 `rust/`）。

### R18 docs/design/FU-57、FU-60、FU-61、FU-64+ERR-1（4 份）
- main；均 2026-09-16；78/120/525/254 行。状态：HISTORICAL（已落地的设计稿，设计通过后实现）。
- FU-57 运行失败五类别；FU-60 模块入站监听改走 UDS（`A24_LISTEN_FD` 恒 3，族换 AF_UNIX）；FU-61 包在运行中被卸载/替换；FU-64+ERR-1 连接复用撞关闭 + 错误带解决办法。
- 核验：`rust/crates/agent24-os-proto/src/failure.rs` 存在；`agent24-os-fd` 现为 `UnixListener`（`lib.rs:10,57`），与 FU-60 一致；SPEC-ME3 §1 已同步改写。
- 链接失效：FU-61 的 `apps/agent24-cli/src/main.rs`、`apps/agent24d/src/os_routes.rs`（缺 `rust/` 前缀）。

### R19 docs/design/HW-TIERS-capability-hardware-matrix.md
- main 2026-10-15，294 行（kernel/comm/decide 分支为同一份）。状态：CURRENT（ADR-036 配套，jason 2026-10-14 拍板 §10 五条）。
- 核验：`agent24-hw` crate 与 `hardware.json` 落盘已实现（HW-1）；未做：runtimes 探测、HW-3 路由、能力梯与解析器、`ModelRouter` 云端开关（HW-4）。
- 关键决定：见 §3 ADR-036。
- 链接：引用 `iDoris/docs/07`、`iDoris/docs/13`（iDoris 仓库内，未核）。

### R20 docs/design/HYPHAE-CLI-INTEGRATION.md
- main；2026-10-01；81 行。状态：HISTORICAL（跨仓提案，文首自称「待确认，不代表已实现」）。已被 COMM-HYPHAE（r2）取代为现行设计。
- 链接：`../../rust/crates/agent24-comm/hyphae.lock.json` 有效。

### R21 docs/design/INTEGRATION-AGENTEAR-IDORIS.md
- main 2026-10-15，375 行；ab/idoris 2026-10-15，377 行（+2 行）。状态：CURRENT（ADR-032 配套；P3–P5 未做）。
- 核验：`IDORIS_URL` 仍未接线；`ModelRouter::from_env` 只构造 oMLX + Ollama（`rust/crates/agent24-models/src/router.rs:320-`）。
- 关键决定：见 §3 ADR-032。
- 链接失效：`packages/router/package.json`、`packages/router/src/server.ts`（指向 iDoris 仓库内路径，本仓库不存在）。

### R22 docs/design/ME4-S1、ME4-S2、ME4-S2a、ME4-S2b、ME4-S2c、ME4-S3（6 份）
- main；ME4-S1 2026-09-25/971 行；S2 2026-10-17/1180；S2a 2026-10-15/240；S2b 2026-10-15/163；S2c 2026-10-17/297；S3 2026-10-15/1495。
- 状态：CURRENT（均已冻结且已实现）。ME4-S2 的数值被 S2a 修订：`MODEL_CALL_TIMEOUT` 120s → 兜底 4456s，`MAX_METHOD_CALL_TIMEOUT` 300s → 同兜底值；代码 `rust/crates/agent24-os-proto/src/rpc.rs:90` 与 `rust/apps/agent24d/src/model_callback.rs:37` 都指向 `model_budget::MODEL_CALL_BACKSTOP`，与修订一致。
- 分支：ab/documenting 上 ME4-S2c 有 6 行未合入修订。
- 关键决定：S1 调度回调 `_a24/scheduler/*` 与 fired 投递；S2 推理回调 `_a24/model/*`；S2a 按进展计时；S2b 开放 `temperature`；S2c Ollama 思考模型先自由作答、不合格再关思考重答；S3 `agent24-os-sdk` 从两个调用方提取。
- 链接失效：S1 `docs/me4-1.1.1-scheduler-design`（分支名）；S2 `apps/agent24d/src`（缺 `rust/`）；S2c `tools/modal-ollama/runs/...`（不在仓库）；S3 `rust/apps/agent24d/tests/me4_sdk_minimal_blackbox.rs`、`rust/scripts/check-lints.py`、`scripts/check-lints.py`、`scripts/check-proto-exports.py`、`scripts/ts11-test-modules.expected.patch`（均不存在）。

### R23 docs/design/OPEN-DESIGN-INTEGRATION-LANDING-STRATEGY.md、OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md、OPEN-DESIGN-PR-REVIEW-PLAYBOOK.md（3 份）
- main；2026-10-02/409、2026-10-02/556、2026-09-29/347。状态：HISTORICAL（M10 已于 2026-10-03 落地；GUIDE 自称「历史审计记录」；STRATEGY 仍写「M10 execution active」，已过期）。

### R24 docs/design/SHUT-shutdown-observability.md
- main；2026-09-16；116 行。状态：CURRENT（v5 语义说明，已实现）。核验：停机标记/守卫在 `rust/apps/agent24d/src/lifecycle.rs`、`supervisor.rs`。
- 关键决定（用户 2026-09-13）：停机时长保留默认（排空 0.8s + 宽限 0.5s，总 2s），必须有日志、跟踪、可调。

### R25 docs/design/T7a、T7b、T7c、T8、T8.5a、T8.5b、T8.5c-P、T8.5c-W-mount、T8.5c-W-wire（9 份）
- main；2026-09-17…09-19；284/348/147/224/290/244/2813/1751/1057 行（T7a、T7b、T7c 行数依次 284、348、147；T8 224）。状态：HISTORICAL（均「设计已冻结」并已实现，保留作出处）。
- 核验：`approval_callback.rs:813` T7c `schedule_callback`；`KERNEL_OOP_GRANTS` 现含 Events/Approval/Memory/Scheduler/Models。
- 链接失效：T8.5c-P 引 `docs/design/T8.5c-memory-private-rpc.md`；T8.5c-W-mount 与 W-wire 引 `docs/design/T8.5c-W-mount-wire-entitlement.md`（均已不存在，被拆分取代）。

### R26 docs/design/WINDOWS-PORT.md
- main；2026-10-01；315 行。状态：CURRENT（v2 冻结，只写设计）。ADR-DOC-01 D10 与之一致：Windows 暂不可用，待 DEP-C2。
- 链接失效：`protocol/state_file.rs`、`tools/env_whitelist.rs`、`tools/local.rs`（缺 `rust/crates/agent24-` 前缀，实为 `rust/crates/agent24-protocol/src/state_file.rs` 等）。

### R27 docs/design/（只在 ab/kernel 上）K1-06a-grant-path、K1-06a-resource-scope、K1-06b-egress-matrix
- ab/kernel；2026-10-26/187、2026-10-21/87、2026-10-26/141。状态：CURRENT（设计 / 台账；未并入 main）。
- 主题：K1-6a-G 模块工具内核授予路径；K1-6a-R `authorized_resources` 契约；K1-6b.5 逐出口测试矩阵（6b.5a 建表、6b.5b 已补模型类）。
- 关键决定：K1-6a-G 负责人 David，任务 #705 剩余；egress 矩阵以「现有测试 + 缺」两列推进，每补一片勾一项。

### R28 docs/design/DOC-1-06b-module-tools.md（只在 ab/documenting）
- ab/documenting；2026-10-19；140 行。状态：CURRENT（设计）。
- 主题：文档操作作为模块工具（#705 agent 暴露）；分两段：Documenting 模块侧（片 1）+ K1 内核授予路径（走 ab/kernel）。
- 关键决定：关联信息 `run_id`/`tool_call_id` 放请求正文 `context`，不放头；工具名不含点；工具不用 `pending` 信封。

### 1.5 docs/open-design-workspace/design（OD 集成 ADR 与冻结包）

### R29 ADR-001…ADR-006（6 份）
- main；ADR-001/002/003/004/005 均 2026-09-19，82/218/179/183/104 行；ADR-006 2026-10-01/150 行。状态：CURRENT（ADR-001–004 “Accepted / P0 frozen”，ADR-005 “Accepted / 2026-09-19”，ADR-006 “Accepted / frozen for incremental G4”）。
- 核验：见 R01 与 §5-B：目标形态（非持久 session 分区、capability daemon 由 Desktop 拥有、`workspace_id` 绑定）在产品路径上未生效。
- 关键决定：ADR-001 Agent24 是唯一 AI 控制面，第三方只是工作区；ADR-002 `a24.workspace.v1`（`orchestrator_scratch`、`writeback_policy=external`、`serial`，TTL 24h/7d）；ADR-003 `agent24.open-design.v1` ACP 桥，attach-only；ADR-004 Electron Creative host 契约（按 app+workspace 派生非持久分区）；ADR-005 Creative capability 与 authority 隔离（5.6-SOL 审查触发）；ADR-006 Legacy recovery holds。

### R30 G1-G2-SCRATCH-SERVICE-PLAN、G8-BOUNDED-LAUNCH-DECODER、G8-SIDECAR-ACTOR-PLAN（3 份）
- main；2026-10-01；164/142/158 行。状态：CURRENT（“Design frozen / implementation …”）。核验：`rust/apps/agent24-sidecar-host/src/{launch.rs,actor.rs,…}` 与 `agent24-sidecar-host-protocol` 已有实现；文头「implementation not started」已过期（sidecar-host 已实现但未接线 Desktop）。→ 文头状态过期，内容仍有效。

### R31 COMPATIBILITY-MATRIX、P0-REVIEW、README、RISK-REGISTER、open-design.pin.json（5 份）
- main；2026-09-19；41/32/33/35/28 行。状态：HISTORICAL（P0 冻结包与风险登记）。

### 1.6 其他

### R32 docs/architecture/kernel-boundary.html
- main；2026-08-22；480 行。状态：HISTORICAL（自称「带日期的快照」）。
- 对照：文中「`agent24d` 直接依赖 `agent24-sin90`」「`ScopedMemory`/`KernelCtx` 0 处」「M-D 12 模块无外部引用」已全部过期——`agent24-sin90*` crate 已删（T11 #342）；`KernelCtx` 在 `agent24-domain`；agent loop 现经 `session_memory.rs` 使用 `kv.retriever()`。
- 链接：无。

### R33 docs/specs/SPEC-001-engineering.md
- main；2026-07-28；155 行。状态：STALE-WRONG（局部）。
- 对照：文档（§1）仓库布局含 `workers/python-ml/`、crate 清单只列 10 个；（§2）`feat/<task-id>` stacked PR、「merge 后 squash」、（§3）Tier 2「gh Copilot」。代码/现行：无 `workers/` 目录；crates 已 22 个 + 4 个 apps；分支流程以 AGENTS.md 为准（`ab/*` 集成分支、PR-Daemon）；用户全局规则明确禁止用 Copilot 评审。
- 仍有效：依赖方向铁律、`agent24-core` 零框架依赖、renderer 不直接发 HTTP。

### R34 docs/specs/SPEC-002-protocol.md
- main；2026-09-17；206 行。状态：CURRENT（人类可读规范，真源是 `protocol/openapi.yaml` 等）。含 `module-approval.required` 事件（T7b）。未逐条对码。

### R35 docs/specs/SPEC-EDITIONS.md
- main；2026-09-02；207 行。状态：CURRENT（产品决策：开源/商业/企业边界，三条主线已裁决，一条待确认；FU-23）。链接失效：`../../../Brood/protocol/MISSION.md`（仓库外）、`docs/laws/EDITIONS.md`（提议文件不存在）。

### R36 docs/specs/SPEC-MD-ME.md
- main；2026-09-09；307 行。状态：SUPERSEDED（部分）：ME-3 行写「经 MCP/协议」被 ADR-031 改正（“MCP 是领域 OS 可以对外暴露的一个面，不是内核挂载它的那条缝”）；M-D 部分被 ADR-028「Codex 复审收口」重定为权威+投影。其余作为 M-D/M-E 蓝图仍有参考价值。

### R37 docs/specs/SPEC-ME-FOLLOWUPS.md
- main；2026-10-08；347 行（kernel/comm/decide 分支各多 11 行，已知未并入）。状态：CURRENT（文首有「2026-10 状态（M2 收口，2026-10-08）」更新节）。局部过期：正文「12 个 M-D 模块外部引用数全为 0」（2026-08-22 数据）已过期——`rust/crates/agent24-agent/src/session_memory.rs:160` 已调用 `kv.retriever().search_any`；consolidator 仍无消费者。
- 关键决定：F3 已关闭（jason 2026-10-07）——继续 `/api/v1/<name>` + 保留段名单，不迁到 `/api/v1/os/<name>`。

### R38 docs/specs/SPEC-ME3-OUT-OF-PROCESS.md
- main；2026-10-15；833 行。状态：CURRENT。已同步 FU-60（UDS，`A24_LISTEN_FD=3`）。ADR-031 配套。

### R39 docs/specs/WIRE-OOP-MODULE.md
- main；2026-10-17；882 行。状态：CURRENT（第三方开发者可凭此写进程外模块的协议参考）。

### R40 docs/specs/SPEC-MEMORY.md
- main；2026-08-22；62 行。状态：SUPERSEDED——文首自称按 ADR-028 收口「待 spike 后整体重写」；分层表是过渡视图。现行记忆设计看 ADR-028（权威+投影）、`laws/MEMORY.md`、ab/memory-p1 的 ADR-MEMORY-*。

### R41 docs/specs/SPEC-ORG-SPACE.md
- main；2026-08-23；310 行。状态：CURRENT（ADR-030 配套详细设计，含「做不到什么」）。F8 已实现；F9 grants/groups 等第二个真实用户。

### R42 docs/specs/SPEC-SKILL-DISTRIBUTION.md
- main；2026-09-02；155 行。状态：CURRENT（设计定稿，未实现，归 M5）。

### R43 docs/specs/F4-nostr-channel.md
- main；2026-10-01；247 行。状态：HISTORICAL（2026-07-29 设计，与 agent-speaker 的接口契约；已被 COMM-HYPHAE 的 Hyphae 通信取代，D5 冻结 F4b）。`packages/nostr-bridge` 仍在。

### R44 docs/specs/SIN90-domain.md
- main；2026-09-08；573 行。状态：SUPERSEDED——被 `docs/SIN90-PET0-INTEGRATION.md` v0.3 与独立仓库 `iDoris-ai/Sin90` 的 `docs/DESIGN-LIFEOS.md` 取代（Pet0 集成文档自己写了「那份文档写于 T11 迁出之前」）。链接失效：`docs/me3-out-of-process`（分支名）。

### R45 docs/specs/LOOP.md、TASKS.md、TODO.md、README.md（4 份）
- main；2026-07-27/86、2026-09-29/608、2026-09-08/113、2026-09-29/42。状态：HISTORICAL（loop 自主开发手册与 M-A…P4 任务队列）；唯一状态源已转到 `docs/agent/tasks.md`。TASKS.md 链接失效：`docs/adr-026-specs`（分支名）。

### R46 docs/comm 联调与验收记录（7 份）
- main：JOINT-ROUND1（2026-10-01/112）、JOINT-ROUND2（2026-10-01/235）、LOCK-UPGRADE-20261005（2026-10-06/35）、COMM-UNLOCK-ACCEPTANCE（10-06/45）、COMM6-API-ACCEPTANCE（10-06/34）、COMM6-UI-ACCEPTANCE（10-06/26）：状态 HISTORICAL。
- LOCK-UPGRADE-20261024（2026-10-24/28）：状态 CURRENT（当前锁 `144555ca`，v0.26.3；与 `hyphae.lock.json` 一致）。链接失效：`docs/agent/release-v0.26.3-progress.md`。
- JOINT-ROUND2 链接失效：`apps/agent24-cli/src/main.rs`、`apps/agent24d/src/server.rs`（缺 `rust/`）。
- 只在 ab/comm：`docs/comm/COMM-5b-zero-run.md`（2026-10-10/105）：CURRENT；COMM-5b 零运行回归门禁，结构门禁与 T2 随 `cargo test --workspace`，T3 需真实 Hyphae+Go。

### R47 docs/SIN90-PET0-INTEGRATION.md
- main；2026-09-30；405 行。状态：CURRENT（v0.3，T11 之后重写，权威源）。
- 关键决定：Sin90 是独立仓库的进程外模块（`out_of_process_provider`）；Pet0 只经本地 `agent24d` 的 `/api/v1/sin90/*` 代理消费，不直连 Sin90；SPIKE-00 已验证（2026-09-22）。
- 链接失效：`docs/DESIGN-LIFEOS.md`（在 Sin90 仓库）、`tools/Pet0`（仓库外）。
- 备注：用户记忆中「Agent24 提供内置领域模型 Sin90」是 v0.2 的旧说法，已被本文 v0.3 推翻。

### R48 docs/iDoris-integration-and-entry-router.md
- main 2026-09-08，90 行；ab/idoris 2026-10-15，92 行。状态：SUPERSEDED/STALE——文首自称「2026-07-30 提案，之后没有再验证；2026-09 复核时 5 个字段里有 3 个对不上」，不要当契约。现行：iDoris 仓库的《iDoris-Agent24-边界与接口规范》+ ab/idoris 的 `docs/iDoris-consumer.md`（只记消费状态）+ ADR-032/036。

### R49 docs/documenting/adr/ADR-DOC-01、ADR-DOC-02、ADR-DOC-03（3 份）
- main：ADR-DOC-01 2026-10-08/238，ADR-DOC-02 2026-10-17/384，ADR-DOC-03 2026-10-08/202。状态：CURRENT（Accepted：DOC-01、DOC-02 为 2026-10-07，DOC-03 为 2026-10-08）。
- 分支：**ab/documenting 上的 ADR-DOC-02 是 2026-10-26、397 行**，含 DOC-1-06b 的修订（工具路径信封、分页、错误码表），未合入 main；ab/decide 上的 DOC-02 是较旧的 2026-10-16 版。
- 核验：`rust/apps/agent24-documents` 存在，依赖 `agent24-os-sdk`（`domain` 仅 dev），与 D2 进程外一致。
- 关键决定：
  - DOC-01 D1 第一方领域 OS，跨 OS 读取走内核显式授权，只用 `document_id + revision` 寻址；D2 进程外；D4 模块声明工具（规模等同新增 Capability）；D5 类型化 preload（`window.agent24.documents.*`，分块 ≤768 KiB，不经 `backendProxy`）；D6 默认安装、引擎按需下载，第 1 片读取引擎例外（PDFKit/Vision，Swift 辅助进程）；D7 存储在自己的 `data_dir`，不使用 kernel scratch workspace；D8 隐私由内核执行；D9 审计；D10 平台（macOS 优先，Windows 以后）。
  - DOC-02 唯一可编辑权威（#690）、唯一提交点与幂等、类型化错误、Job；Q15 抽取做法（David 2026-10-10）。
  - DOC-03 D1 引擎只用 SuperDoc headless SDK（版本锁定 2.18.0，pin+sha256，`superdoc host --stdio` JSON-RPC，不需要 Node）；D2 只用 headless 以守许可边界；D3 LibreOffice headless 做转换/PDF；D4 字体是产品要求；D5 DOC-1 不嵌编辑画布；D6 隐私与网络。
- 链接失效：`docs/documenting/README.md` 引用 `docs/open-design-workspace/design/ADR-001`（缺扩展名）；DOC-01 引用 `ADR-00x`（占位）。

### 1.7 只在集成分支上的架构类文档

### R50 ab/memory-p1：docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md
- ab/memory-p1；2026-10-23；113 行。状态：CURRENT（DRAFT / 待外审，未批准）。
- 主题：个人记忆生命周期与清除权：no-loss 正常生命周期，仅用户明确确认的 purge 是破坏性例外。
- 关键决定（均为「建议」）：用户操作契约五项——暂停、撤回（已有）、更正 supersede、清除 purge、无痕 incognito（拟议）；purge 采「持久意图 + 每库原子批次 + 幂等后台清理」，状态 `preview→prepared→fenced→cleaning→verified`；首版只提供「选定会话闭包」和「本人全部个人记忆」两档；确认须绑定 actor/scope/预览版本/一次性 nonce；容量默认 200000 行 / 256 MiB（`0014_owner_usage_quota.sql`），非实测。
- 链接：引用 `../agent/PLAN-M1-SNAPSHOT-PROVENANCE.md`（旧草稿，仅归档）。

### R51 ab/memory-p1：docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md
- ab/memory-p1；2026-10-23；63 行。状态：CURRENT（DRAFT / 待独立设计评审）。
- 关键决定（建议）：独立、版本化的逐消息来源映射（暂称 `run_message_origins`），复用 `(run_id,seq)`，不重载 K1 隐私标签；来源五类 UserPrompt/SessionHistory/HostRecall/HostSystem·Assistant·ToolResult；消息、来源映射、K1 标签同一事务；digest 只是一致性检查，不是签名。

### R52 ab/memory-p1：docs/agent/R05-0 … R05-4（5 份）+ PLAN-MEMORY-P1 + MODULE-PROGRESS-EVIDENCE
- ab/memory-p1；R05-0 2026-10-24/41、R05-1 10-24/102、R05-2 10-24/58、R05-3 10-24/76、R05-4 2026-10-26/107；PLAN-MEMORY-P1 10-23/105；MODULE-PROGRESS-EVIDENCE 10-23/62。状态：CURRENT（DRAFT 评审包；R05 仍 DRAFT，R05-4 明确「不接受 ADR、不派 G1」）。
- R05-0 外审包（只列待决）；R05-1 精确契约（PR #1170）；R05-2 D7 来源种类；R05-3 D7f 存储 kind；R05-4 H03 已合并格子收口。外审人 clestons。

### R53 ab/memory-p1：docs/research/SUPERFLUID-AGENT24-IDORIS-REVIEW.md
- 2026-10-23；232 行。状态：HISTORICAL（研究评审，不属架构决定；其余 research 探针文档属其它簇）。

### R54 ab/weknora：docs/knowledge/*（8 份）
- ab/weknora；ADR-KB-01 2026-10-24/45，CONTRACT-OPS-v1 10-20/172，CONTRACT-READONLY-v1 10-21/396，CONTRACT-STATE-v1 10-20/202，CONTRACT-UI-BRIDGE-v1 10-20/197，K1-DEPENDENCIES 10-20/66，POSITIONING-AND-DIFFS 10-24/88，PLAN-KNOWLEDGE-TASKS（同批，另见）。状态：CURRENT（草案：CONTRACT-STATE 为「规范核心 v1（草案）」；POSITIONING 的定位「整体上待确认」，“唯一主对话”一条已由用户 2026-10-11 确认）。
- 关键决定：ADR-KB-01 知识能力进入 Agent24 **唯一主对话**——迁移用户入口与编排权，复用 WeKnora 知识流程，不整套搬运、不另建 Agent 引擎；KnowledgeView 只管资料/Wiki/版本/同步，不托管第二聊天、第二个人记忆或第二套模型配置。ab/documenting 上有同一份 ADR-KB-01 与 PLAN-KNOWLEDGE-CONVERSATION 副本。
- 链接：引用 `iDoris-ai/WeKnora` fork 的 `docs/agent24/CAPABILITY-MATRIX.md`（外部）。

### R55 ab/office-assistant：OA-K-KERNEL-GATE-DESIGN、OFFICE-ASSISTANT-TECH-DESIGN（中/英）
- ab/office-assistant；2026-10-16；459 / 245 / 274 行。状态：CURRENT（Draft，待内核 ADR、待实现；文头声明没有任何部分已实现）。
- 主题：OA-K：模块请求、内核执行的外部动作门——模块交 `(tool, 确切 input, action_key)`，内核存成有约束力的 `ModuleApproval(kind=gate)`，逐次审批，批准后内核执行一次、过出站门、写哈希链审计、返回类型化回执，不自动重试；归属 #735（K1 依赖）。
- 注：同分支另有 OFFICE-ASSISTANT 的计划、讨论、翻译等 14 份，属其它簇。

### R56 ab/idoris：docs/iDoris-consumer.md
- ab/idoris；2026-10-15；24 行。状态：CURRENT。仅记 iDoris 消费状态，跨仓契约唯一权威是 iDoris 仓库《iDoris-Agent24-边界与接口规范》（PR516 固定修订 `1924dda0`）；硬件事实反向权威仍是 HW-TIERS §3.3/3.4 与 ADR-036。

### R57 其他分支副本说明（不单独计状态）
- ab/kernel、ab/comm、ab/decide 上的 `HW-TIERS-capability-hardware-matrix.md`（294 行）、`ADR-DOC-0x`、`ARCHITECTURE-LAYERS.md`、`decision.md`（+81 行，即 ADR-036 前的 hw/decide 内容）与 main 同源或更旧，不另列。ab/maint、ab/weknora（除 knowledge）、ab/office-assistant（除 OA）没有新增架构类文档。

## 2. 汇总 1：当前架构的分层与组件清单（对照 ARCHITECTURE-LAYERS 与代码）

### 2.1 实际工程结构（以代码为准）
- `rust/crates`（23 个）：agent、comm、core、decide、domain、hw、mcp、memory、models、os-cwd、os-fd、os-packages、os-proto、os-sdk、policy、protocol、scheduler、sidecar-host-protocol、store、tools、worker、workspace（全部前缀 `agent24-`）。
- `rust/apps`（4 个）：agent24d、agent24-cli、agent24-documents、agent24-sidecar-host。
- `apps/desktop`（Electron）；`packages/`：api-client、contract-tests、documents-accept、node-daemon、nostr-bridge、wechat-bridge。
- `protocol/`：openapi.yaml（4531 行，手写真源）、events.schema.json、module.schema.json、fixtures。

### 2.2 文档有、代码没有
- `agent24-workflow` crate（ADR-026 §1）——不存在；工作流只剩 scheduler。
- `agent24-node-host`（ADR-026 §3）——不存在；`packages/node-daemon` 是 mock/参考实现兼 TS CapabilityModule 宿主。
- `workers/python-ml/`（SPEC-001 §1）、Python ML Worker 服务端——不存在；`agent24-worker` 只有 Rust 侧契约/客户端，无消费者。
- `docs/laws/PRIVACY.md`、`docs/laws/EDITIONS.md`——提议文件，不存在。
- 「OpenAPI 由 Rust 生成（B4）」、iDoris provider（ADR-032 P4，无 `IDORIS_URL`）、推理回调流式（P3）、任务画像生成（ID-1）、领域 OS 签名（ME-6）、`pgl.yml`——均为计划。
- `agent24-sin90` / `agent24-sin90-store` crate（kernel-boundary.html、SIN90-domain 时代）——已于 T11 删除。
- 「能力梯 / 解析器 / HW-3 路由 / `ModelRouter` 云端开关」（ADR-036 / HW-TIERS）——HW-1 之后均未做。
- 「`@auraaihq/*` 包」（ADR-016-consume）——未采用。

### 2.3 代码有、ARCHITECTURE-LAYERS 没写（或写得不全）
- `rust/apps/agent24-documents`（Documenting 进程外模块，依赖 `agent24-os-sdk`）与 `packages/documents-accept`——整份 LAYERS 都没提。只在 ADR-DOC-01…03 中出现。
- 依赖表缺项（`cargo metadata` 实测）：`policy` 还依赖 `domain`；`tools` 的依赖是 domain/protocol/store/workspace（表里只写 protocol/workspace）；`agent24-cli` 的正常依赖含 `hw`（表里漏，正文写了）；`store` 有 dev 依赖 `decide`。
- K1 已合 main（#1022）但 L5/L3 没写：`agent24d/src/module_tool_runtime.rs`、`consent_routes.rs`、`module_approvals.rs`、`module_approval_broker.rs`、`memory_routes.rs`、`os_memory_page.rs`；`agent24-policy/src/consent_gate.rs`；`agent24-store` 迁移 0014 `decision_log`、0015 `module_consents`、0016 `run_source_tags`、0017 撤销、0018 `reversible_draft`。
- `agent24-policy/src/write_scope.rs`（工作区根外写入分类与禁区）。
- Desktop：`idoris-admin.ts`（ADR-035 匿名 FD 交付 token）、`agentear-events.ts`/`agentear-log.ts`、`documents.ts`/`documents-import.ts`、`model-call-log.ts`、`independent-review.ts`——LAYERS 的 L7 只提 BackendManager/CreativeServeWeb/SidecarManager。
- `agent24d` 的 `host_bootstrap.rs`（capability 模式 ready pipe）、`hardware.rs`（`~/.agent24/hardware.json`）、`attach_*`、`comm_routes.rs`——LAYERS 部分提及。

### 2.4 LAYERS 内部说法与代码不符（摘要，详见 §5）
- L3 memory 行与 §7：「retriever / consolidator 尚未被 agent loop 消费；M1 在 `ab/m1-memory`」——retriever 已被 `session_memory.rs:160` 消费；M1 已 2026-10-07 合 main（#720）；`ab/m1-memory` 分支不存在；consolidator 仍无消费者。
- `decide` 描述为「只被 `GET /api/v1/decide/profile` 消费」——路由确只有该一条（`server.rs:1063`），属实。

## 3. 汇总 2：现行技术决策清单（按主题，括号里是出处与状态）

### 3.1 进程与部署
1. Rust `agent24d` 是唯一核心运行时；Electron 桌面经 `spawn agent24d serve --port 0`，只绑 `127.0.0.1`（ADR-026，ACCEPTED；`server.rs:1894`；ADR-027 遗留：bind-address 配置尚无，仍硬编码 127.0.0.1）。
2. 默认鉴权 `legacy_single_token`，写 `~/.agent24/daemon.json`（0600/0700）；capability 模式（`--auth-mode capabilities --host-bootstrap-stdio`）已实现但 Desktop/ACP 未使用（ARCHITECTURE-LAYERS §L5；ADR-005）。
3. 单 daemon、单 SQLite（`agent24.db`）、单用户 `LOCAL_USER`；org/space 跨物理库属 ADR-030 硬门槛 2，不触碰（agent/architecture.md）。
4. 领域 OS 与模块全部进程外：Sin90、Cos72、Documenting 是独立二进制，经 manifest+握手+回调 socket+受约束代理挂载；进程内 `DomainModule` 挂载缝保留但无生产实现（ADR-029 2026-09-10 修订，ADR-031，SPEC-ME3）。
5. 模块入站监听走 UDS，fd 3，`A24_LISTEN_FD=3`；内核到模块只有一条通道（FU-60，SPEC-ME3，architecture.md ME-4 边界）。
6. 开发流：`ab/<方向>` 集成分支，task PR ≤300 行，release PR 由人开人审；安全/权限/写入语义/迁移/wire 类须 PR-Daemon APPROVE（AGENTS.md，jason 2026-10-07）。
7. 大组件按需下载，不进安装包：Open Design（2026-10-05 裁决，AppImage 146MB→934MB 的教训，`open-design-component.ts`）；Documenting 引擎同理（ADR-DOC-01 D6）。
8. Windows 暂不支持 workspace-bound run 和进程外模块（待 DEP-C2；WINDOWS-PORT，ADR-DOC-01 D10）。

### 3.2 协议与 API
9. v1 REST+WS；`protocol/openapi.yaml` 手写权威，`events.schema.json` 由 Rust `Event` 导出并有零漂移门，`module.schema.json` 手写（ARCHITECTURE-LAYERS §L0，SPEC-002）。
10. 审批是请求-响应、流式是单向通知，`approval.required` 携带 `available_decisions`（ADR-026 §6.5）。
11. 进程外模块协议自造窄 JSON-RPC 回调通道，不用 ACP/MCP；入站是纯反向代理（ADR-031，ACCEPTED，2026-09-02）。ACP 只用于缝 A，由 `agent24 acp` 映射桥实现。
12. 模块路由命名空间 `/api/v1/<name>`，保留段名单+双向集合测试（F3 裁决，jason 2026-10-07）。
13. `/api/v1/<ns>/_a24/` 为内核保留路径（ME-4 边界）。
14. 模块工具：稳定全名 `<module_id>.<operation>`，11 个错误码，类型化结果 `completed|pending`，不自动重试（ADR-K1-01，ACCEPTED 2026-10-08）。
15. Documenting 工具路径关联信息放请求正文 `context`，REST 路径 `/api/v1/documents`（ADR-DOC-02 + DOC-1-06b；该修订只在 ab/documenting）。

### 3.3 存储
16. SQLite via sqlx；哈希链审计日志；workspace 权威数据在 store（ARCHITECTURE-LAYERS §L2）。注意 ADR-017「加密 SQLite」在代码里没有（无 sqlcipher）。
17. 决策日志 `decision_log`+`decision_outcome`，plain DTO，实现归 `agent24d`，不是 `agent24-store`（ADR-034）。
18. 模块业务真相只在模块自己的 SQLite（`~/.agent24/os/<name>/`）；内核私有分区里只放派生摘要（architecture.md）。
19. 审计只记元数据，不记正文/凭据/完整提示；留存 180 天；用户可查看导出（ADR-K1-04）。

### 3.4 记忆
20. 记忆 = 权威 + 投影：`EventLog`（情景权威）、`ArtifactStore`（markdown/知识）、`AssertionLedger`（语义断言、真双时相），FTS/向量/摘要为可重建投影；先在一个 crate 里定 trait 缝（ADR-028 Codex 收口，状态「暂定/需 spike」）。
21. 记忆所有权 = (org, space)，二者不可变 ID 进 key；变得勤的（group、grant、workspace）永不进 key；成员≠访问权；用户∩模块；无 deny 规则；执行点在句柄发放（ADR-030，ACCEPTED；`laws/MEMORY.md` L-MEM-1..7）。
22. 模块只拿能力受限的 `ScopedMemory` 句柄（remember/recall/recent），不拿 `KvStore`/`EventLog`/pool（L-MEM-1/2、README）。
23. `CanonicalSession` 降为遗留导入，EventLog 成为会话唯一权威（architecture.md；`session_memory.rs:261`）。
24. P1（ab/memory-p1，DRAFT）：正常生命周期 no-loss，仅用户确认的 purge 破坏性；R05 逐消息来源映射（ADR-MEMORY-*，均未批准）。

### 3.5 模型与路由
25. `ModelRouter` 按 `TaskProfile{Privacy, Complexity}` 在 Local/Remote/Lora 选 provider，`LocalOnly` 对远端 fail-closed；`from_env` 只构造 oMLX（默认 `http://127.0.0.1:8088`，key 默认）与 Ollama；非 loopback URL 判为 Remote（`router.rs:320-334`；ARCHITECTURE-LAYERS L3）。
26. 主对话/子 agent/REST 聊天仍传 `TaskProfile::default()`；只有 Guardian 与模块推理回调设 `LocalOnly`（`routes.rs:569`、`subagent.rs:231`、`model_callback.rs:808`）。
27. 模块推理回调默认 `local_only`，manifest `model_access.remote_allowed` 才放开（ADR-K1-02 §1、`model_callback.rs:489`）。
28. 硬件探测与整机内存账归 Agent24（`agent24-hw`），模型选型/量化/加载归 iDoris；档位 1–6 仅用于展示，准入用整数字节预算；云端补位默认关且必须由 `ModelRouter` 强制；降级自动、升级需确认（ADR-036，ACCEPTED 2026-10-14；仅 HW-1 已实现）。
29. `agent24-decide` 在 L3，零内部依赖（ADR-036 后仅依赖 `agent24-hw`）；`Outcome` 三态，不能放宽权限；规则命中短路；`Unavailable` 立即停止级联不降级；`llm_simulation` 校准标记由 `DecisionService` 强制（ADR-033，ACCEPTED，含 #688 评审修正）。
30. iDoris 以 OpenAI-compat provider 接入（拆 `idoris-local` 强制 `X-iDoris-Privacy: local_only` 与 `idoris-any`），不做成 OOP 模块；LAN/Tailscale 算远程（ADR-032，P4 未接线）。
31. iDoris Admin session token 由可信 launcher 经匿名 inherited FD/HANDLE 一次性交付，不进 argv/env/文件，renderer 永不得 token（ADR-035，ACCEPTED，#765）。

### 3.6 工具、审批与策略
32. 工具流水线固定：normalize → 能力白名单 → 审批门 → 超时执行；MCP 工具走同一条（ADR-026 §6.5；`agent24-tools`、`agent24-mcp`）。
33. 审批 fail-closed；`approve_for_session` 仅 `Read` 风险提供；`exec` 永远逐次问；external 常驻授权形如 `tool→确切目标`（`policy/lib.rs:41-80,159`；laws/APPROVAL L-APPR-4/5）。
34. Guardian 默认关闭（`A24_GUARDIAN=1` 才开），只有明确解析出 `low` 才自动批准（`server.rs:414-430`，ARCHITECTURE-LAYERS L3）。
35. 模块启用授权：宿主签发、逐工具、有限期、可撤销；安装/签名不等于同意（ADR-K1-03）。
36. 可逆草稿免逐次审批仅限第一方 Documenting（`record.module == "documents"`），且接线闸门 L3–L5 未解（laws/APPROVAL L-APPR-5；`consent_gate.rs`）。
37. 会话数据流两态 `LocalOnly`/`CloudAuthorized`，未知即 `LocalOnly`；`has_remote()` 过渡门要 K1-6a 与 K1-6b 都完成才撤（ADR-K1-02）。
38. 分类 / 处理模式 / 授权三维独立，出口判定看「本次动作实际携带」（`ActionDataflow`），缺证据即 `Unknown` fail-closed（**仅 ab/kernel 的 ADR-K1-02 §2.1.1，2026-10-10 裁定，未并入 main**）。
39. 取消一等公民：长等待都与 `CancellationToken` 竞速（ADR-026 §6.5，ARCHITECTURE-LAYERS L4）。

### 3.7 扩展（领域 OS / 模块 / sidecar）
40. 四类外挂：①进程内 DomainModule（无生产模块）②进程外领域 OS ✅ ③附着模块 ✅（A3）④sidecar 集成（Open Design）（ARCHITECTURE-LAYERS L6）。
41. 内核授予模块的最大能力：进程内 `{Events,Memory,Approval}`；进程外再加 `{Scheduler,Models}`（`domain.rs:75,116-`）。
42. 附着模块：AgentEar 自启，TCC/热键/麦克风归 AgentEar；不做逐轮中继；断连后若走本机边车则回独立模式，任何情况不得把 `local_only` 请求改发远端（ADR-032 D1–D4、B5）。
43. Open Design：Agent24 是唯一 AI 控制面，第三方只是工作区；现行路径 `CreativeServeWeb`+`agent24 acp`（legacy bearer、`workspace_id=None`、全局持久分区 `persist:agent24-creative`）；目标形态 SidecarManager+capability+workspace-bound（ADR-001…006，LAYERS）。
44. Documenting：第一方、进程外、`document_id+revision` 寻址、DOCX 引擎 SuperDoc headless 2.18.0、不嵌编辑画布（ADR-DOC-01/03）。
45. 知识能力（WeKnora）进入唯一主对话（ADR-KB-01，仅 ab/weknora，草案）。

### 3.8 通信（Hyphae）
46. 通信服务在 agent24d 内（`agent24-comm`），`/api/v1/comm/*`；Hyphae 作子进程监管，`--notify=false`；专用 HOME；口令存钥匙串只走 `--password-stdin`；二进制靠 `hyphae.lock.json` + sha256；当前锁 `144555ca`（v0.26.3，2026-10-24）；F4b 冻结，基础通信 zero-run（COMM-HYPHAE D1–D8，LOCK-UPGRADE-20261024）。

### 3.9 桌面
47. Electron+React 是正式桌面外壳；`BackendManager` 托管 daemon（`AGENT24_BACKEND=node|rust`，默认 rust）；renderer 不直发 HTTP；Documenting 用类型化 preload，不经 `backendProxy`（ADR-026，ADR-DOC-01 D5）。
48. Desktop 通用 `backendProxy` 现仍接受任意路径并附带 token——ADR-002/005 要求的隔离未做（LAYERS §L5，PLAN-OD-NEXT F11.0）。

### 3.10 跨平台
49. 移动端：shell-agnostic，官方提供 Tauri 2.0 与 Expo/RN 两个瘦壳示例，只经 HTTP/WS（ADR-027，取代 ADR-018）；远端 daemon 需 bind-address 与配对认证，未做。
50. workspace-bound run 仅 Unix；Windows 无 `WorkspaceService`，产品 API 返回 `503 workspace_unavailable`（LAYERS §L2）。

### 3.11 安全与鉴权
51. 威胁模型明确：进程外模块同 UID、合作但可能有 bug，**不防敌意本地二进制**；`shell_exec` 只 `fchdir`，不是 OS 沙箱；capability 是应用层最小权限不是 OS 沙箱（LAYERS §4，ADR-031 末句）。
52. 仅两处窄 `unsafe`：`os-cwd` 的 `fchdir`、`os-fd` 的 fd 接管；`[workspace.lints] unsafe_code = "forbid"`（`rust/Cargo.toml`）。
53. 隔离主张不得超过机制（L-MEM-7）；法律改动须单独 PR（laws/README）。
54. 第三方开源工具接入路径：边界 ADR → fork+pin → SidecarManager → 复用 `agent24 acp` → workspace+短期令牌 → 精确 SHA E2E（LAYERS §6）。

## 4. 汇总 3：互相冲突的决定（按代码给出现行方）

| # | 冲突双方（出处） | 现行方（依据） |
|---|---|---|
| 1 | decision.md ADR-020「Node http，固定端口 8765，M3 换 Fastify」vs ADR-026「Rust agent24d，`--port 0`，废弃固定 8765」 | ADR-026。`server.rs:1894` `bind(("127.0.0.1", port))`；全仓无 8765 引用。ADR-020 在 decision.md 仍标「✅ 采纳」，未标 Superseded |
| 2 | ADR-023「M3 切 Python FastAPI」vs ADR-026 | ADR-026（ADR-023 已标 Superseded） |
| 3 | ADR-022「默认 MLX（`mlx-lm`），Python 后端集成」vs ADR-026 §5 / 代码 | oMLX 作 OpenAI-compat HTTP 服务：`ModelRouter::from_env` 默认 `OMLX_URL=http://127.0.0.1:8088` 与 Ollama；无 `mlx-lm` 依赖。ADR-022 未标 Superseded；其「设置页切换」也未实现（README 自述） |
| 4 | ADR-024「asyncio.Queue+Step 工作流引擎」vs ADR-026（`agent24-workflow` + scheduler） | 代码：只有 `agent24-scheduler`，无 workflow crate，无 Python 工作流。ADR-026 声明“ADR-024 部分 Superseded”，decision.md 的 ADR-024 未标注 |
| 5 | ADR-025「串行 LLM 推理、6GB 阈值暂停」vs ADR-036「整机内存账归 agent24-hw，准入用整数字节预算，lease 串行」 | ADR-036 为现行（HW-1 已有探测；lease/准入未做）。ADR-025 为 Python 时代设计，未实现 |
| 6 | ADR-016（decision.md）「模块跑 `worker_thread`，manifest 声明权限，keytar 存凭据」vs ADR-029/031（进程外模块，同 UID，无沙箱） | 进程外模块为现行。README 自述 TS CapabilityModule 的 permissions「只做结构校验与安装同意摘要，不参与强制」。ADR-016 的 M3 子进程/签名增强并未做 |
| 7 | decision.md ADR-016（模块安全）vs `docs/ADR-016-consume-auraai-packages.md` | **同号不同题**；后者被 ADR-026 §6 收缩，代码无 `@auraaihq` 依赖 |
| 8 | ADR-014/015「M3 后 Agent24 被 `@auraaihq/skills-*` 取代并改名」vs ADR-026 | ADR-026 + 现状：仓库就是 Agent24，无上述 npm 迁移 |
| 9 | ADR-017「轨迹/记忆默认加密 SQLite」vs 代码 | 代码为明文 SQLite（store 无 sqlcipher/加密）；该条未实现，应改标 |
| 10 | ADR-029 正文「第一方进程内 OS 编进 agent24d」vs 其 2026-09-10 修订 | 修订（OOP）现行：`Build::InProcess` 无生产构造，`agent24-sin90*` 已删 |
| 11 | ADR-028 正文「`agent24-memory` 拆 4 个 crate」vs 同文 Codex 收口「先别拆 crate」 | 收口。仍单 crate `agent24-memory` |
| 12 | SPEC-MD-ME §5 ME-3 行「经 MCP/协议」vs ADR-031 | ADR-031（自述改正） |
| 13 | ADR-033 #3「agent24d 不感知 decide」vs ARCHITECTURE-LAYERS / 代码 | 代码：`agent24d` 依赖 `agent24-decide`，`GET /api/v1/decide/profile`（`server.rs:1063`）；ADR-033 的“零接入”仅对 D0-1 成立，且 decide 已被 ADR-036 加了 `agent24-hw` 依赖（ADR-036 自带“对既有 ADR 的修订”） |
| 14 | SPEC-001 §2/§3（`feat/*` stacked PR；squash merge；Tier-2 gh Copilot 评审）vs AGENTS.md（`ab/*` 集成分支；PR-Daemon；release PR）+ 用户全局规则（禁用 Copilot） | AGENTS.md |
| 15 | README「`KERNEL_GRANTS = {Events, Memory}`」vs LAYERS / 代码 `[Events, Memory, Approval]` | 代码与 LAYERS |
| 16 | laws/APPROVAL L-APPR-5/8「今天没有机制；`approve_for_session` 仍放行 shell_exec/fs_write」vs 代码 | 代码已收紧（见 R15 对照）；法律文字过期 |
| 17 | ADR-004（OD）「按 app+workspace 的非持久分区」vs 代码 `persist:agent24-creative` | 代码为全局持久分区；ADR 是目标形态（LAYERS 已如实标注） |
| 18 | ADR-001/005「Desktop 必须自己拉起并拥有 capability daemon」vs `BackendManager` | 代码复用任意健康 daemon（LAYERS §7 已标注） |
| 19 | ADR-K1-02：main 版 vs ab/kernel 版（§2.1.1 三维独立 + 反例 9–11） | ab/kernel 较新（2026-10-10），尚未合入 main；以 jason 2026-10-10 裁定为准 |
| 20 | ADR-DOC-02：main（2026-10-17）vs ab/documenting（2026-10-26） | ab/documenting 较新（DOC-1-06b 修订：工具信封、分页、错误码表），未合入 |
| 21 | ADR-K1-03：main 的 laws 口径「L-APPR-1..7」vs main laws 已有 L-APPR-8 | 以 laws/APPROVAL 为准；kernel/memory-p1 版 K1-03 已修链接 |
| 22 | LAYERS 与 SPEC-ME-FOLLOWUPS「retriever/consolidator 未被 agent loop 消费，M1 在 ab/m1-memory」vs 代码 | retriever 已消费（`session_memory.rs:160`）；M1 已合 main（#720）；ab/m1-memory 已不存在；consolidator 仍无消费者 |
| 23 | 用户记忆「Agent24 提供内置 Sin90」vs SIN90-PET0-INTEGRATION v0.3 | v0.3：Sin90 是独立仓库进程外模块 |
| 24 | README「v0.4.0 当前」vs 版本号 | 0.5.1（Cargo.toml、package.json、tag） |

## 5. 最重要的 5 处文档与代码不符（对照）

### A. ARCHITECTURE-LAYERS 的记忆与 M1 说法（L3 memory 行、§7、§5）
- 文档：「retriever / consolidator 等高层尚未被 main 上的 agent loop 消费（M1 的工作）」「M1 已在 `ab/m1-memory` 恢复为当前主干，完成后合回 main」。
- 代码：`rust/crates/agent24-agent/src/session_memory.rs:160` 调 `self.kv.retriever().search_any(...)` 做跨 session 召回；`retain.rs` 用 `WriteGate`；`ab/m1-memory` 分支不存在，M1 已 2026-10-07 合 main（#720）。consolidator 仍无消费者。
- 同组遗漏：整份 LAYERS 没有 `agent24-documents`、K1 模块工具/consent（R01 的 §2.3）。

### B. laws/APPROVAL L-APPR-5 与 L-APPR-8 的「今天靠什么保证」
- 文档：「今天没有这条机制……`approve_for_session` 仍会放行同一会话内后续的 `fs_write` 与 `shell_exec`」。
- 代码：`rust/crates/agent24-policy/src/lib.rs:41-80,159-161` 仅 `Read` 可 `approve_for_session`；`write_decisions`（:163-）对根外 `fs_write` 提供目录级 `approve_for_target`；`write_scope.rs` 禁区判断已实现（提交 2026-10-21/22）。唯一仍缺：已确认工作区根的免询问。
- 影响：按“法律必须与实现一致”的 README 规则，这是需要单独法律 PR 去更新的条目。

### C. decision.md 里已被取代但仍标「✅ 采纳」的 ADR（014/015/016/017/020/022/024/025）
- 文档：ADR-020「TypeScript+Node http，localhost:8765」；ADR-022「默认 MLX（`mlx-lm`）」；ADR-024「asyncio.Queue+Step」；ADR-017「加密 SQLite」。
- 代码：`rust/apps/agent24d/src/server.rs:1894` 动态端口、仅 127.0.0.1；`router.rs:320-334` oMLX HTTP :8088；`rust/crates/` 无 workflow crate；store 无加密。
- 只有 ADR-018、ADR-023 被正确标 Superseded。

### D. README 架构部分
- 文档：`KERNEL_GRANTS = {Events, Memory}`；「v0.4.0（当前）」；「ADR-001 ~ ADR-030」。
- 代码：`domain.rs:75` 为三项；版本 0.5.1（`rust/apps/agent24d/Cargo.toml`、`apps/desktop/package.json`）；ADR 已到 036。

### E. ADR-K1-03（main）13 个失效相对链接 + 过期的 K1-02 副本
- 文档：`../rust/...` 等 12 个内部链接与 `docs/laws/PRIVACY.md`。
- 实际：应为 `../../rust/...` 等；`PRIVACY.md` 不存在。ab/kernel、ab/memory-p1 已有修复版；ab/kernel 的 ADR-K1-02 还多出 jason 2026-10-10 裁定的 §2.1.1 未并入 main。

## 6. 汇总 4：只写在过时文档里、必须保留进新文档的历史要点

1. **ADR-026 的 11 条设计硬约束（§6.5）**——这些要点只在 ADR-026，且是评审基准：取消一等公民（openfang 无 `CancellationToken` 的教训）、审批=请求-响应而流式=单向、fail-closed 且 turn 结束统一 drop sender、`available_decisions` 数据驱动、审批与沙箱正交、Guardian 对接 L1 小模型、Tool 用 registry trait 禁巨型 match、cron 到期先 pre-advance + `MissedTickBehavior::Skip` + 连续失败自动禁用、WS 强类型 serde tag enum、crate 解耦用 `KernelHandle` 风格反向注入、审计遥测脱敏。
2. **ADR-026 的显式否决**：不做全量 Rust 重写、不先写 TS 内核、不 fork Goose/Codex/VT Code、GPL（zerostack）只读思路不复制（GPL 会传导到 agent24d 静态链接与下游，破坏 Apache+商业双生）。
3. **ADR-027** 的移动端 shell-agnostic 策略及“需 bind-address 配置 + 远程认证/配对 + Web 壳 Origin/CSRF”遗留——代码仍未做。
4. **ADR-018 的 M0–M4 约束**（避免 Electron-only API / node 原生依赖）仍有效。
5. **ADR-028 Codex 收口**：真双时相需 valid-time + recorded-time 两轴；权威按数据产品分，不全局“文件即真源”；自动写要 candidate→validate→approve→commit；Condenser 绝不删原始事件；本地嵌入=可复现索引；L4 程序性记忆是安全边界；研究报告 12 条事实订正（OpenHands checkout 不含 condenser，letta-code 用 git-backed MemFS，mem0 V3 单抽取，codex 有 Rust `ext/memories`）。
6. **ADR-030** 八条不变量与「不能等第二个用户的三件事」；明确不做清单（不是沙箱、不加密隔离、无行级权限、无配额、表达不了凭据/服务账号/委托等）；「凭据」是相对组织 Agent 目标的最大缺口。
7. **ADR-031** 的 ACP 版本核查（v1 stable vs v2 draft 删除 `fs/*`、`terminal/*`）与「选型不提供安全性」；“MCP 同理不采用为模块协议”的三条理由。
8. **ADR-029** 原三种玩法（用默认 / 基于默认定制 / `os install cos72 && os activate cos72`）与 `~/.agent24/os/<name>/` 独立目录；F3 裁决（不迁 `/api/v1/os/<name>`）的理由。
9. **ADR-033 的评审修正**（#688）：级联规则、`llm_simulation` 强制点、`ThresholdBands` 反序列化须走校验。
10. **ADR-035** 的失陷情形/保证边界全表（launcher、Electron main、iDoris、renderer、root、crash dump、FD 误继承、replay）。
11. **ADR-017**（隐私默认）：默认全本地；技能共享 opt-in 且只发蒸馏结果；iDoris 中间数据不离设备。方向仍有效，“加密 SQLite”一句需改。
12. **kernel-boundary.html** 的 file:line 核验方法与「建好了没通电」的诊断方式，及它促成 ME-1…ME-4 的因果。
13. **OPEN-DESIGN-M10-MAIN-LANDING-GUIDE** 与 LAYERS §6 的 M10 教训：integration 分支漂 633 提交/约 200 叠加 PR；设计文档没随第一片落地；“组件落地”与“产品路径生效”要分开验收——这是 AGENTS.md「每周吸收 main」硬约束的来源。
14. **FU-57/60/61/64、T7a/b/c、T8、T8.5\* 系列**的多轮 Codex 评审结论（每个冻结稿文末）是对应不变量的来龙去脉；新文档至少保留“每个不变量对应哪份设计 + 哪个测试”的索引。
15. **SPEC-ME-FOLLOWUPS** 的六轮对抗复审记录（F1/F8）与 F4c/F4e 缺口说明、`Forwarded`/`X-Forwarded-*` 剥离理由。
16. **SIN90-PET0-INTEGRATION** §3–§9：v0.2→v0.3 的架构变化、SPIKE-00 验证方式、v0.5.0 新增能力追记。
17. **INTEGRATION-AGENTEAR-IDORIS** / ADR-032 的非流式首字 4.86s vs 直连流式 2.80s 数据、`ADR-0008 §5` 六问的答复表、B5。
18. **HYPHAE-CLI-INTEGRATION**（历史）与 JOINT-ROUND1/2 的联调发现（如 F1 “生产路径必然写入真实登录钥匙串”→ `A24_COMM_PASSWORD_STORE` 内存口令开关）。
19. **SPEC-001** 里仍有效的工程铁律：依赖方向、`agent24-core` 零框架依赖、renderer 不直发 HTTP、生成物禁止手改。
20. **ADR-016（decision.md）** 的分阶段安全意图（M1 起步 / M3 子进程+签名 / M5 VM 沙箱）与 AirAccount 信任根设想：未实现，但是 ME-6（领域 OS 签名）的上游需求。

## 7. 计数核对

- 份数核对：main 87 = 设计 29 + OD 工作区 14 + specs 16 + comm 7 + laws 4 + ADR-K1 4 + ADR-DOC 3 + 根级 10（ARCHITECTURE-LAYERS、ADR-016、ADR-026、SIN90-PET0、iDoris-integration、agent/architecture、kernel-boundary.html、AGENTS.md、README.md、decision.md）。
- 分支独有 27 = memory-p1 10 + weknora 8 + kernel 3 + office-assistant 3 + documenting 1 + idoris 1 + comm 1。
- 未纳入：docs/agent/* 计划与台账、docs/research/*、docs/Deployment/*、docs/documenting/（除 adr/）、ab/office-assistant 的其余 14 份产品文档——属其它簇。
- 本文件中的“局部不符”条目（ARCHITECTURE-LAYERS、README、laws/APPROVAL、SPEC-001、SPEC-ME-FOLLOWUPS、ADR-K1-03 main 版链接）都已给出 文件:符号 对照。
