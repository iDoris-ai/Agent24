# 评审处置清单（2026-10-27）

> 来源：`CODE-REVIEW-2026-10-27.md`。按 jason 要求拆成两份：**发布前必须修**（进 `v0.6.0-preview.20261028.1` 的 main）和**后续完善**（发版后按里程碑推进）。
> 判据：进第一份的条件是「在默认产品路径上敞开 + 修复成本 S（≤1 天）+ 不改契约、不加迁移」。改契约、加迁移、拆大函数的一律进第二份。
> 最终版复审：四条线全部合入 main 后，对 main 再做一轮同口径的源码评审，更新本清单。

## 一、发布前必须修（两个 PR，进 main）

### PR-A `fix/pre-release-desktop-hardening`（Electron，只跑 vitest/typecheck）

| # | 条目 | 来源 | 做法 | 验收 |
|---|---|---|---|---|
| A1 | `backendProxy` 方法+路径白名单（本片只剔除审批决定、shutdown、tool-overrides、capabilities、attached/os 管理；schedules 写、os 命令、comm unlock/daemon、memory settings 等 Elevated 路由仍在白名单，由 **CORE-7c**（随 CORE-2d 在场证明）统一收口） | R1 / C-01 / S-02 | 主进程维护 `ALLOWED_PROXY_ROUTES: {method, pathTemplate}[]`，只放行渲染层实际用到的 `/api/v1/*` 读与低风险写；**剔除** `POST /approvals/{id}`、`POST /module-approvals/{id}`、`POST /shutdown`、`PUT|DELETE /tool-overrides/*`、`DELETE /standing-grants/*`、`POST /capabilities/*`、`POST|PATCH|DELETE /attached*`、`PATCH /os/*`、`POST /os/*/stop`，这些改走专用 IPC（现有审批页需要的「决定」走 `ApprovalDecide` IPC，主进程弹原生确认框后再发） | 单测：白名单外路径返回 `{error:{code:'forbidden'}}`；审批决定经专用 IPC 仍可用；现有 renderer 调用的每条路径都在白名单（用 grep 生成的清单做正对照） |
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
