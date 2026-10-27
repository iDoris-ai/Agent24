# Agent24 全代码评审报告（2026-10-27）

> 评审基线：`origin/ab/core` @ `4f4f49ab`（含 main 全部内容 + ADR-037 CORE 线至 CORE-2a-2）。范围：`rust/apps/*`、`rust/crates/*`、`apps/desktop`、`packages/*`、`protocol/`。
> 方法：四个维度各由一名独立评审员只读审查（不改文件、不构建），产出原始稿见 `2026-10-27/` 附录 A–D；本报告由协调者（Opus）逐条核实、去重、定级并给出处置。所有引用的文件行号以基线为准。
> 评审员：架构与分层 = Sonnet；结构与通信 = Sonnet；安全 = Codex gpt-6.1-sol；性能 = Grok 4.7。
> 结论先行：**没有阻塞 `v0.6.0-preview.20261028.1` 发布的缺陷**；但有一组在默认产品路径上敞开的边界问题（桌面全权代理、REST 无 Host 校验、契约门有洞），应作为发版后第一优先级处理。

## 1. 总评

四份评审在同一处汇聚：**agent24d 的工程质量不均衡**。进程外模块这条链（JSON-RPC、attach socket、监管、熔断）帧长、并发、超时、错误闭集都有常量和大量测试，质量高；`unwrap/expect` 受 workspace lint 挡住，生产路径几乎为零；crate 依赖图单向、无环。相反，壳与 daemon 之间的边界（HTTP/WS、桌面 IPC、契约 CI）是最薄的一层，而它恰恰是 ADR-037「daemon 是本体，外壳皆客户端」要依赖的那一层。

四个维度发现合计 **61 条**（High 13、Medium 32、Low 16），去重后跨维度共同指向 8 个根因，见 §3。多数已被 ADR-037 / CORE-2 / CORE-5 设计登记为后续片；本报告标出的是「设计已知、代码里仍敞开」的部分，以及设计文档没覆盖到的缺口（documents.db 不在迁移保护范围、comm 路由不在 OpenAPI、桌面残留 node-daemon 死功能、聊天路径的 O(n²) token）。

## 2. 协调者核实与修正

对原始稿做了抽样核实（基线 worktree 上 grep/读码），以下条目与原稿不同，以本节为准：

| 条目 | 原稿说法 | 核实结果 |
|---|---|---|
| C-16 第 4 点 | 「`/health` 的迁移水位尚未返回」 | **错误**。基线已含 CORE-4（#1301）：`agent24-protocol/src/types.rs:30-32` 有 `protocol_version` 与 `migrations: HealthMigrations`。C-16 其余三点成立。 |
| P-08 | 「没有生产环境 WAL checkpoint，磁盘剩余不在写入门上，Rust 树里没有 statvfs」 | **对 ab/core 成立，对产品线已部分解决**：`ab/memory-p1` 已有 `wal_maintenance.rs`（c5，#1283：空闲时 PASSIVE→TRUNCATE）和 `disk_gate.rs`（c4，#1296：statvfs，5s 缓存，内容门 max(512MiB, 2×WAL)），随 #1341 进 main。P-08 中「预算要把 FTS/WAL 算进去」仍未做。 |
| A-12 | CLI 直接开 `agent24.db` 绕过 daemon | **已由 CORE-5a（#1333，基线之后合入）收口**：离线入口先取单例锁、查停靠标记、只读核版本、不迁移、缺库不建库。 |
| C-05 / A-04 | 硬编码密钥 `xiaobao8088` 在桌面源码出现 9 次 | 实数 **6** 处（`grep -rn xiaobao8088 apps/desktop/src`）。问题本身成立。 |
| C-02 第 1 点 | OpenAPI 门只比路径不比方法 | 成立，且已确认一处方法级漂移：`server.rs:1217` 注册 `get(get_approval).post(decide_approval)`，`openapi.yaml` 的 `/approvals/{id}` 只有 `post`。另核实 `openapi.yaml` 中 `/comm/` 出现 0 次。 |
| C-03 | REST 无 Host/Origin 校验 | 成立：`server.rs` 中无任何 `header::HOST` 读取。 |
| S-05 / A-13 | ProductHost TTL 为 `u64::MAX` | 成立：`server.rs:1572` `Duration::from_secs(u64::MAX)`。 |
| A-01 / C-10 | `serve()` 约 1270 行 | 成立：`server.rs:1403` 至 `:2675`，文件共 6517 行。 |
| 性能稿开头 | 「P1-02C 两份文档在此提交不存在」 | 属实：它们在 `ab/memory-p1`。性能稿因此以 ADR-037、CORE-5 文档和 `0014_owner_usage_quota.sql` 为据，不影响结论。 |

其余条目按原稿置信度（CONFIRMED / PLAUSIBLE）保留，未逐条复核的以原稿为准。

## 3. 跨维度根因与处置（Top 8）

按「默认产品路径是否敞开 × 修复成本」排序。「映射」列指向已存在的里程碑任务；没有的新建 `REV-nn` 编号，登记进 `docs/MILESTONES-2026-10-26.md`。

| # | 根因 | 来源 | 现状（基线） | 处置 | 映射 | 工作量 |
|---|---|---|---|---|---|---|
| R1 | **桌面 `backendProxy` 让渲染进程借全权 token 访问任意路径**；无超时、无体积上限；主窗口 `sandbox:false`、IPC 不校验 sender、无导航守卫 | A-02 A-03 S-02 C-01 C-06 | 默认 legacy 模式下敞开。任何 renderer XSS = daemon 全权（含审批决定、shutdown、capability mint） | **发版后第一件事**：主进程按「方法+路径模板」白名单转发（直接复用 `route_policy.rs` 的 POLICIES 生成 TS 常量），提升级路径（审批决定、shutdown、tool-overrides、capabilities、attached 写）从代理中剔除、改专用 IPC + 原生确认；加 30s 超时与体积上限；`senderFrame` 校验；主窗口 `will-navigate` 拒绝；preload 打包后开 sandbox | CORE-7（原「渲染进程隔离」）拆为 7a 白名单+超时（S）、7b sender/sandbox/导航（M）、7c 短期窄令牌（随 CORE-2f） | S+M |
| R2 | **桌面残留 node-daemon 时代的死功能与依赖**：`/api/modules*`、`/api/codebox/*`、`/api/service-box/*` 页面在 Rust 后端下 404；desktop 仍依赖 `@agent24/node-daemon`；node-daemon 自身有未鉴权安装入口与同意前执行（S-01、S-03） | A-02 S-01 S-03 C-20 | node-daemon 默认不启动（`AGENT24_BACKEND=node` 才跑），CORE-8 已把它移出安装包（ab/core），但源码与桌面依赖仍在 | 删除上述页面与 IPC 或改接 `/api/v1/os`；desktop 摘掉 node-daemon 依赖，根构建链不再先构建它；node-daemon 只留 contract-tests 的最小子集（health/chat/runs），`contract` job 标 rust-only 为主。S-01/S-03 不单独修，随删除一并消失 | **REV-01**（新） | M |
| R3 | **契约门有洞**：OpenAPI↔路由 CI 只比路径不比方法、不含 comm（10 条）、靠源码文本扫描；两套扫描器并存；表外路由缺省按模块路由放行；错误码是开放字符串、三处清单不一致 | C-02 C-07 C-12 A-13 | `tui-coverage` 以 OpenAPI 为分母，comm 和漂移路由不在分母里，TUI「能力对齐」统计偏乐观 | 复用 2a-2 已有的带方法 `parsed_routes` 驱动 OpenAPI 门（路径+方法双向）；补 `/comm/*` 与 `GET /approvals/{id}` 的 OpenAPI；`decide()` 对表外已匹配模板改为拒绝；`ApiErrorCode` 枚举统一三处清单并加源码扫描测试。中期把路由收敛为单一数据表驱动 Router/POLICIES/OpenAPI/tui-coverage | G-API-3 拆为 3a 门补方法+comm（S）、3b 错误码枚举（M）、3c 单表路由（L） | S+M |
| R4 | **REST 无 Host/Origin 校验；WS 事件对所有连接广播、不复验吊销、不过滤主体** | C-03 C-13 S（已核实未列） | 业务接口仍要 bearer，匿名 DNS rebinding 目前不能执行业务；一旦 capability 模式成默认、对开发者令牌开放，低权令牌可读全部会话事件 | Host 白名单中间件（`127.0.0.1:{port}`、`localhost:{port}`、`[::1]:{port}`，约 20 行）与浏览器 `Origin` 统一拒绝，**随 CORE-2e 一起做**；事件按令牌主体过滤 + 吊销断连 + 全局 `event_id`/`?since=` 重放 + 服务端 ping，随 CORE-2b | G-API-2（Host/Origin）、G-API-4（事件授权） | S / M |
| R5 | **三库失败语义不一致，documents.db 不在迁移保护范围**：`agent24.db` 打不开即失败，`memory.db` 打不开只 warn 继续，`documents.db` 由独立二进制自管；跨库（run 终态 vs 记忆保存）无事务也无补偿协议 | A-06 A-07 | CORE-5 D1 已把 memory 失败改为整体门（5b 协调者已写，未接线）；CORE-5 文档未提 documents.db | CORE-5 范围补一条：「daemon 拥有的库 + 它启动的第一方模块库」，documents 在 daemon 拉起它之前走同一快照回调，或声明进快照清单；5f 接线前 memory 打开失败至少改为 `/health` 可见的 degraded。A-07 的跨库缝先写成 ADR 级不变量（允许哪个方向丢、幂等键是什么），再决定是否要 outbox | CORE-5（范围修订，设计 PR 已冻结 → 以 **REV-02** 作为 CORE-5 的 follow-up 条目）、**REV-03**（跨库不变量 ADR） | M / M |
| R6 | **聊天热路径的 O(n²) 与阻塞**：桌面每轮重发全部历史；`/chat` 在返回前同步等压缩模型（持会话锁，最长 30s）；每轮为 turn 序号全扫会话事件；`/chat` 与 run 不用已有的流式 Progress（首字要等全文，最长 120s） | P-01 P-02 P-03 P-10 P-11 | 用户可感知的最大一块：长会话越用越慢、越贵；首字延迟与 ADR-026 目标相悖 | 桌面只送本轮文本，上下文由 daemon 的已压缩 `SessionLog` 视图提供；压缩移到后台任务、不占会话锁；`next_turn` 改持久计数器；`/chat` 打开 `Progress` 并经 WS 推 delta；`load_view` SQL 侧 LIMIT | **REV-04**（新，性能线首项；与 TUI-2 聊天输入共用 daemon 侧会话视图） | M |
| R7 | **记忆库写放大与启动阻塞**：FTS 重建在 ready 之前、单事务、失败每次启动重来；中文 bigram 使倒排项 ~n−1 倍、FTS 行撤回不删；每轮 `active_ids` 全量加载现行信念、缺部分索引；每轮 run 把会话尾部复制进 `run_messages` 再删；记忆库 5 连接但进程内路径不进准入信号量 | P-04 P-05 P-06 P-07 P-12 | c4/c5 已解决磁盘门与 checkpoint（见 §2）；其余未动 | FTS 重建离开 ready 路径、分批事务、失败保留旧索引；`content=''` 外部内容表；撤回删 FTS 行；部分索引 `(scope_owner) WHERE recorded_to IS NULL AND qualified=1`；resume 直接读 `SessionLog`，`run_messages` 只留本 run 新行；进程内走同一把准入 | **REV-05**（新，记忆性能；排在 c3b 之后）、P-04 挂到 CORE-5f（ready 顺序） | M / L |
| R8 | **agent24d 组合根过胖，结构性测试靠读自己源码**：`serve()` 1270 行；后台任务无 JoinHandle/TaskTracker，关停不等 scheduler/pump/在途 run；store 成为杂物间（28k 行、18 个领域迁移）；~15k 行无生产消费者的抽象（sidecar-host、进程内 DomainModule、worker） | A-01 A-08 A-09 A-10 C-10 C-11 C-17 | 已逼出 `include_str!("server.rs")` 切函数体做断言的测试 | 按启动阶段拆 `StartupPhase`（acquire_lock → migrate → build_state → mount → spawn_background → commit_ready → serve → drain），结构性测试改为对阶段函数的单测；`TaskTracker` 管后台任务，关停时 cancel 在途 run 并等落终态；store 分子模块；**裁决**：sidecar-host 与进程内 DomainModule 去留（见 §5） | **REV-06**（serve 拆分，与 CORE-5f/2e 的启动改造合并做，避免两次重排）、**REV-07**（任务生命周期）、**REV-08**（store 分层）、§5 裁决项 | L / M / M |

## 4. 各维度要点（详见附录）

### 4.1 架构与分层（附录 A，18 条：High 4 / Medium 8 / Low 5）
- 做得好：crate 依赖单向无环；数据目录解析 Rust/TS 共用 fixture 真正单一来源；`daemon.json` 写删持锁；comm 有依赖白名单测试可作全仓模板。
- 主要问题：R1、R2、R5、R8。另有 A-04/A-05（oMLX 管理、Open Design 组件安装、文档分块上传、AgentEar 排序等约 9.8k 行非展示逻辑在 Electron 主进程，TUI 不可用，违反 ADR-037 决策 4）、A-11（344 处 `sleep` vs 3 处 `start_paused`；`HYPHAE_TEST_BIN` 缺失即空过）、A-15/A-16（`~/.agent24` 字面量 30 余处；L0 契约 crate 带 fs 与锁逻辑）、A-17（CLI 无 daemon 时仍 spawn ephemeral，与 ADR-037 决策 2 偏差 → CORE-6）、A-18（ARCHITECTURE-LAYERS 依赖表陈旧 → 用 `cargo metadata` 生成并进 CI）。

### 4.2 结构与通信（附录 B，22 条：High 3 / Medium 11 / Low 8；含 17 条通道盘点表）
- 做得好：OOP 模块链（通道 #5–#9）帧长、并发、超时、熔断、错误闭集齐全；run 执行任务的 panic 有双层监督。
- 主要问题：R1、R3、R4、R8。另有 C-04（MCP 入站 `reqwest::Client::new()` 未套 FU-74 加固、无超时 → 抽 `loopback_client()` 统一，S）、C-05（主进程接受 renderer 给的任意 URL 并带凭据外发；oMLX 密钥进 argv；硬编码默认密钥 → 短期校验 loopback + 密钥走 env，S）、C-08/C-09（api-client 只有类型无运行时，TS/Rust 手写客户端十几份；Nostr/WeChat 桥持全权 token、不看协议版本、无超时 → 共享客户端，L）、C-14（核心通道无传输抽象，Windows 只有「摸底」CI → CORE-2b/2c 前先定 `trait Transport`）、C-19（`test-lock-override` 是生产可见的测试开关 → release 工作流加 feature 检查，S）。

### 4.3 安全（附录 C，5 条：High 3 / Medium 2；另 6 项核实后未列为漏洞）
- S-01/S-03（node-daemon 未鉴权安装、同意前执行）→ R2；S-02 → R1；S-05（ProductHost 永不过期且全权）→ CORE-2b 的 1 小时 TTL + 父管道续期（Q4 已拍板）。
- S-04（数据目录 0700 加固晚于敏感文件打开；legacy ready 行含 token 且被 LaunchAgent `StandardOutPath` 持久记录）：PLAUSIBLE，但修法便宜 → **REV-09**：启动最前端建立并验证 owner-only 数据根，日志预建 0600，服务模式不把 bearer 写 stdout（CORE-3 LaunchAgent 片一并做）。
- 已核实不构成现阶段漏洞：HEAD/OPTIONS/404 先验 bearer（2a-1 修过）；Creative 实际只放行 `GET /models`；WS 不复验令牌但当前无低权令牌可进；REST 缺 Host 校验但业务仍要 bearer；attach 目录与 UID 校验齐全；COMM 口令清零容器经 stdin 交付。

### 4.4 性能（附录 D，16 条：High 4 / Medium 8 / Low 4）
- 主要问题：R6、R7。另有 P-09（ready 前串行：锁→孤儿扫描→两库迁移→MCP 串行挂载 10s 预算→模块逐个挂载→模型探测 3s；同步 fs 调用跑在 async 上 → 随 REV-06 的阶段拆分并行化）、P-13（日程每 10s 全表读、无 `(enabled, next_run_at)` 索引，S）、P-14（桌面空闲每 5s 约 3 次本机请求 + 每秒重渲染，S，随 C-13/C-21 改订阅）、P-15（CI 两平台各编一遍巨型 crate；Cargo.lock 29 组重复主版本如 `rustix` 0.38/1.1、`tungstenite` 0.24/0.29、`thiserror` 1/2 → clippy/test 分 job 共享缓存，`cargo deny` 收紧）、P-16（启用模块无进程数上限，Low）。

## 5. 需要 jason 拍板的事项

1. **sidecar-host 与桌面 SidecarManager 的去留**（A-09）：14.6k 行、无生产消费者、有专门的 Windows CI 在跑。二选一：让 Open Design 真正走 sidecar-host 并删 CreativeServeWeb 的直接 spawn；或冻结到独立仓库/feature，不再进主 workspace。
2. **进程内 `DomainModule` 挂载缝**（A-10）：生产无实现，只有测试 Fake。建议降为 test-only feature 或删除（ADR-031 方向已是进程外）。
3. **`agent24-worker`**（无消费者）与 **`agent24-os-cwd`**（50 行独立 crate）：挂里程碑或并入。
4. **跨库不变量**（A-07 / REV-03）：run 终态与记忆保存之间允许哪个方向丢失，幂等键用什么。这是 ADR 级决定。

## 6. 处置排期（建议）

| 时段 | 内容 |
|---|---|
| 本次预发布（10-28） | 不改代码。发布说明注明：node-daemon 仅契约参考、默认不运行；桌面模块管理/CodeSandbox/ServiceBox 页为遗留页面（R2）。 |
| 发版后第 1 周 | R1 的 7a/7b（白名单代理、超时、sender 校验、导航守卫）；R3 的 3a（门补方法 + comm OpenAPI + `GET /approvals/{id}`）；R2（删死页面、摘 node-daemon 依赖）；C-04、C-05 加固、C-19 release 检查、P-13 索引（均 S）。 |
| 发版后第 2–3 周 | R6（聊天只送本轮 + 后台压缩 + 流式 delta）；R4 的 Host/Origin 随 CORE-2e；R5 的 CORE-5 范围修订 + memory degraded 可见；R3 的 3b 错误码枚举；REV-09。 |
| 与 CORE 线合并推进 | R8（serve 拆分随 CORE-5f/2e 启动改造）；R4 事件授权随 CORE-2b；R7 记忆性能排在 c3b 之后；共享客户端（C-08/C-09）随 CORE-2f 外壳迁移。 |
| 待拍板后 | §5 四项。 |

## 7. 附录

- [A. 架构与分层](2026-10-27/A-architecture.md)
- [B. 结构与通信（含 17 条通道表）](2026-10-27/B-structure-comm.md)
- [C. 安全](2026-10-27/C-security.md)
- [D. 性能](2026-10-27/D-performance.md)

编号对照：A-nn 架构、C-nn 结构通信、S-nn 安全、P-nn 性能、R-n 本报告根因、REV-nn 本报告新建任务。
