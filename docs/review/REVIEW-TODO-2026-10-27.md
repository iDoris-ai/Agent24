# 评审处置清单（2026-10-27）

> 来源：`CODE-REVIEW-2026-10-27.md`。按 jason 要求拆成两份：**发布前必须修**（进 `v0.6.0-preview.20261028.1` 的 main）和**后续完善**（发版后按里程碑推进）。
> 判据：进第一份的条件是「在默认产品路径上敞开 + 修复成本 S（≤1 天）+ 不改契约、不加迁移」。改契约、加迁移、拆大函数的一律进第二份。
> 最终版复审：四条线全部合入 main 后，对 main 再做一轮同口径的源码评审，更新本清单。

## 一、发布前必须修（两个 PR，进 main）

### PR-A `fix/pre-release-desktop-hardening`（Electron，只跑 vitest/typecheck）

| # | 条目 | 来源 | 做法 | 验收 |
|---|---|---|---|---|
| A1 | `backendProxy` 方法+路径白名单（本片只剔除审批决定、shutdown、tool-overrides、capabilities、attached/os 管理；schedules 写、os 命令、comm unlock/daemon、memory settings 等 Elevated 路由仍在白名单，由 **CORE-7c**（随 CORE-2f 外壳迁移，依赖 CORE-2d 在场证明；见下表 CORE-7c 行）统一收口） | R1 / C-01 / S-02 | 主进程维护 `ALLOWED_PROXY_ROUTES: {method, pathTemplate}[]`，只放行渲染层实际用到的 `/api/v1/*` 读与低风险写；**剔除** `POST /approvals/{id}`、`POST /module-approvals/{id}`、`POST /shutdown`、`PUT|DELETE /tool-overrides/*`、`DELETE /standing-grants/*`、`POST /capabilities/*`、`POST|PATCH|DELETE /attached*`、`PATCH /os/*`、`POST /os/*/stop`，这些改走专用 IPC（现有审批页需要的「决定」走 `ApprovalDecide` IPC，主进程弹原生确认框后再发） | 单测：白名单外路径返回 `{error:{code:'forbidden'}}`；审批决定经专用 IPC 仍可用；现有 renderer 调用的每条路径都在白名单（用 grep 生成的清单做正对照） |
| A2 | `backendProxy` 超时与体积上限 | C-01 | `http.request` 30s 超时，响应体上限 8 MiB，错误形状统一为 `{error:{code,message}}` | 单测：挂起的假 daemon 30s 内返回超时错误；超限体返回 `payload_too_large` |
| A3 | IPC sender 校验 + 主窗口导航守卫 | C-06 / S-02 | 所有 `ipcMain.handle` 包一层 `guard`：`event.senderFrame` 必须是主窗口主 frame 且 URL 为应用自身；主窗口加 `will-navigate` 拒绝与 `setWindowOpenHandler` 外链交给 `shell.openExternal` | 单测：伪造 sender 的调用被拒 |
| A4 | oMLX IPC 加固 | C-05 / A-04 | `OmlxModels`/`OmlxWarmup` 的 `url` 必须解析为 loopback 且 http(s)；`port` 校验整数 1–65535；`OmlxStart` 的 api key 改走子进程环境变量，不进 argv；去掉源码硬编码默认密钥（改为设置项，空则不探测） | 单测：非 loopback URL 被拒；argv 中不含 key |
| A5 | 隐藏 node-daemon 时代的死页面 | R2 / A-02 | 导航里移除 ModulesManager、CodeSandbox、ServiceBoxDemo、HelloModule 入口（代码可保留到 REV-01 再删），对应 IPC（`ModulesList/Discover/Install/Uninstall`）停用 | typecheck + 现有 vitest 绿；手工：默认 Rust 后端下导航无 404 页 |

### PR-B `fix/pre-release-daemon-hardening`（Rust，只编 agent24d / agent24-mcp / agent24-cli）

| # | 条目 | 来源 | 做法 | 验收 |
|---|---|---|---|---|
| B1 | REST `Host` 白名单 + 浏览器 `Origin` 拒绝 | R4 / C-03 / G-API-2 | auth 中间件前加一层：`Host` 必须是 `127.0.0.1:{port}`、`localhost:{port}`、`[::1]:{port}`（port 为实际监听端口）；带 `Origin` 头且非 `null`/桌面固定来源的请求返回 403。`GET /health` 同样受 Host 约束 | 单测：错误 Host 403、正确 Host 200；带浏览器 Origin 403；contract-rust 全绿；CLI/桌面/桥现有调用不受影响（它们都用 `127.0.0.1:{port}`） |
| B2 | MCP 入站客户端套 FU-74 加固 | C-04 | 抽 `agent24_protocol::client::loopback_client(connect_timeout)`（`no_proxy` + `redirect::Policy::none()` + 连接超时），CLI/TUI/worker/mcp 统一改用；`agent24_run` 加总时限（默认 600s，可配） | 单测：设置 `HTTP_PROXY` 时请求不走代理；重定向不跟随 |
| B3 | 服务模式不把 bearer 写进 stdout 日志 | S-04 | `--enroll-endpoint`/service 安装路径下，legacy ready 行不含 token（token 只进 0600 的 `daemon.json`）；`service install` 预建 `logs/` 0700、日志文件 0600 | 单测：service 模式 ready 行无 `token` 字段；现有 LaunchAgent 测试绿 |
| B4 | release 工作流拒绝测试 feature | C-19 | `release.yml`/`release-desktop.yml` 加一步 `cargo tree -e features -p agent24d \| grep -E 'test-lock-override\|test-util'` 必须为空 | 工作流语法检查；本地跑同一命令为空 |

两个 PR 都要：pre-pr-check 1.5.0 逐条回应；Mac mini Codex 挑战一轮；PR-Daemon APPROVE；合入 main 后再吸收进 `ab/core`（避免 CORE 线后续冲突）。

## 二、后续完善（发版后，按里程碑）

| 优先 | 编号 | 条目 | 来源 | 挂靠 | 工作量 |
|---|---|---|---|---|---|
| P1 | REV-01 | 删除 node-daemon 残留页面与 IPC，desktop 摘掉 `@agent24/node-daemon` 依赖，node-daemon 收缩为 contract-tests 最小子集 | R2 | 随 CORE-8 进 main | M |
| P1 | G-API-3a | OpenAPI 门补方法级比较 + comm 10 条路由 + `GET /approvals/{id}` | R3 / C-02 | ab/core | S |
| P1 | REV-04 | 聊天只送本轮文本，上下文由 daemon 已压缩视图提供；压缩移后台不占会话锁；`next_turn` 持久计数器；`/chat` 打开流式 Progress 经 WS 推 delta | R6 / P-01–03、P-10 | 新「性能」线，与 TUI-2 共用 | M |
| P1 | REV-02 | CORE-5 范围补 documents.db；memory 打开失败改 `/health` 可见 degraded | R5 / A-06 | CORE-5f | M |
| P1 | CORE-2b | ProductHost 1 小时 TTL + 父管道续期；事件按主体过滤、吊销断连 | S-05 / C-03 / C-13 | ab/core | L |
| P1 | ID-ERR-1 | iDoris v0.3.2 的 `502` + `error.type=upstream_auth_failed` 映射为终态 `Rejected`（不重试、不换 provider），提示检查 `IDORIS_OMLX_API_KEY`；现 `idoris.rs` 把 5xx 一律当 `Unavailable` | iDoris#583/#582 | ab/idoris（等 v0.3.2） | S |
| P1 | REV-10 | `VerifiedBinary::install`（agent24-comm）写出可执行文件后立刻 exec：多线程 daemon 中其它线程 fork 会继承写 fd → Linux 首次安装 Hyphae 偶发 ETXTBSY。改为子进程写入或对 spawn 的 ETXTBSY 有界重试 | #1429 发现 | ab/core 或 COMM 线 | S（修复 PR #1452）|
| P2 | FLAKY-1 | 偶发失败：① `module_tool_card` 的 `no_package_identity` **实为产品 bug**（socket 路径超长，非计时）→ #1456 已修；② `wal_maintenance` 测试假设错误 → #1456 已修；③ `attach_cli` 约 19 s 超时根因未定位，#1456 改了进程管理但不关闭，续查进行中 | 10-28 多次 | #1456 已合；③ 进行中 | S |
| P2 | CI-HYB | CI 没有设 `HYPHAE_TEST_BIN`，`groupchat_real_binary` 在 CI 里被跳过；改为用 `Reproduce Hyphae linux-x64` 产物跑 | #1431 R4 | COMM | S |
| P1 | AE-PAIR | AgentEar 在桌面端开着时配对失败：capabilities 模式下 daemon.json 没有 bearer，CLI attach 报 host authority unavailable。jason 10-28 拍板：不做临时修复，走 CORE-2 每客户端 enroll（ab/core）进 main 解决 | 10-28 人工测试 | ab/core CORE-2 | M；10-28 下午开 CORE-2 AgentEar 纵切：#1448（S1+S2）、#1447（S3 桌面确认）、#1446（S4 CLI 登记），计划 `docs/agent/CORE-2-AGENTEAR-SLICE.md`（该文件随 #1448 合入，此前只在 feat/core2-agentear-s12 分支） |
| P1 | CORE-2-FULL | CORE-2 完整方案（jason 10-28：纵切发版后立即执行）：2c 余项（Linux 签名清单、FD 登记权、配对码退路）；2b 余项（ProductHost 有限 TTL 与父管道续期、开发者令牌 Q6/R3）；2d 余项（Swift LocalAuthentication/Touch ID helper、headless 提供者）；2f 余项（TUI 与桌面自身改走登记、LaunchAgent 服务安装、backendProxy 收窄、G-API）；提升入口按操作逐个开放，每个须先有基于正文的确认摘要 | AE-PAIR 后续 | ab/core | L |
| P1 | CORE-7c | 收窄 `backendProxy` 剩余 Elevated 路由（schedules 写、os commands、comm unlock/daemon、memory settings PUT）：renderer 不再借 ProductHost 直调，改走主进程 + CORE-2d 在场证明。挂靠 CORE-2f（与 `CODE-REVIEW-2026-10-27.md:38`、`backend-proxy.ts` 注释统一） | A1 / R1 | ab/core（CORE-2f） | M |
| P1 | DESIGN-OD | Design 菜单仍显示「组件未安装（按需下载，本版暂未开放）」：Open Design（pin 0.22.2）只支持打包进 resources 或 `A24_OPEN_DESIGN_DIR`，没有按需下载。改为走通用组件安装器 `agent24-components`（HY-1 #1458）：锁定发布物 URL 与 sha256、能力中心卡片与 Design 菜单一键下载校验启用。**前置**：Open Design fork 合并并产出可校验发布物（`creative-serve-web.ts` 的 `OPEN_DESIGN_SHA` 待定），由 jason / OD 线确认 | jason 10-28 | 组件安装器线（随 M-IDL1） | M |
| P2 | CORE2-S4-DELEGATE | `agent24 os delegate <name>`（#1448 为过覆盖门新增）要在 S4 接入登记与 `presence_required` 自动提升；当前与其他命令一样只在 legacy 可用 | #1448 | ab/core（S4 #1446） | S |
| P3 | TRAY-DETAIL | 托盘 error 状态不再显示 `BackendManager::statusDetail()`（中文长句不受界面语言控制）；把 detail 做成可本地化字段（协议版本、pid）后再放回托盘/提示 | #1443 Low-1 | ab/core | S |
| P3 | OFFLINE-CLOSE | 离线入口「先 `Store::close()` 再释放单例锁」的顺序没有测试守护（PR-Daemon 变异 M2 存活）；补能观察到 WAL 句柄/锁顺序的测试或写明不变量 | #1443 Low-2 | ab/core | S |
| P2 | SW-1 | `self_wake` 堆积：**拒绝过去时间已交付**（#1434 第二轮 3540beb3，测试 `past_at_is_rejected_without_creating_a_wake`），桌面已完成行折叠+清理也已交付；余项 daemon 侧按保留期回收已完成 at 行 → #1451 已合入 main。`MAX_PENDING_WAKES` 是否需计入已触发行待重新评估 | #1434 调查 | 已基本完成（#1434、#1451） | S |
| P1 | ID-HR | iDoris v0.4.0：503 必须先看 reason_code，`headroom_confirmation_required` 时去问用户并带 `X-iDoris-Headroom-Breach: confirmed` 重发，不得当普通 Unavailable 重试或换 provider；/capabilities 1.1 字段（degraded_alternative(s)、headroom_breach_option 含 quant、备选含 capability）；另：503 `supervisor_busy` = 同模型正以不同余量确认加载中，稍后用同一请求头重试、不问用户；`headroom_breach_option` 按「哪个条目带该字段」定位，不按 resident:true 找，快照内最多一次（iDoris#586/#587） | idoris-25 10-28 | ab/idoris | M（ID-HR-1 #1455 已合 ab/idoris；ID-HR-2 #1457）|
| P2 | G-API-3b | `ApiErrorCode` 枚举统一 Rust/OpenAPI/桌面三处错误码清单 | C-07 | ab/core | M |
| P2 | REV-05 | 记忆性能：FTS 重建离开 ready 路径、`content=''` 外部内容表、撤回删 FTS 行、`active_ids` 部分索引、`run_messages` 不再复制会话尾部、进程内走准入信号量 | R7 / P-04–07、P-12 | ab/memory-p1（c3b 之后） | L |
| P2 | REV-06 | `serve()` 按启动阶段拆分，结构性测试改为阶段函数单测；MCP 与模块并行挂载 | R8 / A-01 / C-10 / P-09 | 随 CORE-5f/2e | L |
| P2 | REV-07 | `TaskTracker` 管后台任务，关停时 cancel 在途 run 并等落终态 | C-11 | ab/core | M |
| P2 | REV-03 | 跨库不变量 ADR（run 终态 vs 记忆保存，幂等键） | A-07 | 设计 PR | S（文档） |
| P2 | REV-09 | 启动最前端建立 owner-only 数据根；日志预建 0600 | S-04 | CORE-3 | S |
| P2 | C-08/C-09 | `packages/api-client` 加运行时（发现、版本校验、超时、错误解码）；桥与 renderer 迁移；桥改用窄令牌 | — | CORE-2f | L |
| P2 | A-11 | 统一 Clock/等待助手，禁止新测试裸 `sleep`；真实二进制测试 CI 必设变量 | — | REL-9 续 | M |
| P3 | REV-08 | store 分子模块（module_*、workspace 分离） | A-08 | — | M |
| P3 | C-14 | `trait Transport` 抽象 UDS/命名管道 | — | CORE-2b/2c 前 | L |
| P3 | P-13/P-14/P-15 | 日程 `(enabled,next_run_at)` 索引；桌面轮询合并；CI clippy/test 分 job、`cargo deny` 收重复依赖 | — | — | S/S/L |
| P3 | A-15/A-16/A-18 | `~/.agent24` 字面量收敛；protocol crate 拆出 fs/锁；ARCHITECTURE-LAYERS 依赖表自动生成 | — | DOCS 线 | S |
| 待拍板 | §5 | sidecar-host、进程内 DomainModule、worker/os-cwd 去留 | A-09 / A-10 | jason | — |
