# PLAN-M2 — M-E 收口余项

> 立于 2026-10-07。集成分支 `ab/m2`，task 分支 `ab/m2-NN-<短名>`，PR base = `ab/m2`（规则见 `AGENTS.md`「里程碑方向分支工作流」）。
> 输入：`roadmap.md` M2（F2.1 = SPEC F4，F2.2 = SPEC F3，F2.3 = SPEC F6 / F7）、`docs/specs/SPEC-ME-FOLLOWUPS.md`（写于 2026-08-22）、`followups.md`、`COMPONENT-ROADMAP.md` C1 / C6、issue #661 / #663。
> 核实基线：`origin/ab/m2` @ `d408a5a`（= main @ #724）。下文 file:line 都指这个提交。
> 状态台账：`tasks.md`「M2 台账」。本计划只为**核实后仍然存在**的条目拆 task；设计只评审 1 轮，之后冻结。

---

## 1. 逐条核实

SPEC 写于 2026-08 下旬，之后 ME-3、ME-4、M10、M1 都已合入 main。每条判据都**先给正对照**，证明这个检查方法能查出问题，再下结论。

### 1.1 F4（#134 复审中判定「单独治」的五条）

| ID | 现状（证据） | 正对照 | 结论 |
|---|---|---|---|
| **F4a** OpenAPI 缺端点 | 从 `build_router_with_modules`（`rust/apps/agent24d/src/server.rs:1056-1205`）提取 `.route("…")`，与 `protocol/openapi.yaml` 的 `paths` 做集合差。内核共有 **11 条路由不在契约里**：`/os`、`/os/{name}`、`/os/{name}/stop`、`/os/{name}/commands/{command}`、`/attached`、`/attached/{name}`、`/timings`、`/timings/summary`、`/capabilities/creative`、`/capabilities/{capability_id}/revoke`、`/events`。另外 `/api/v1/comm/*` 由 `comm_routes.rs:336` 挂载，不在这个函数里，也没进契约。反方向上，`/sin90/*` 七条在契约里，但内核已不再提供（Sin90 自 T11 起是进程外模块，经代理转发）。CI 只跑 `pnpm lint:openapi`（`.github/workflows/ci.yml:158-159`），这个检查只能对已写进契约的内容做 lint，**少写一个端点它发现不了**（FU-10 说的是同一个问题）。桌面端用手写类型调用 `/os`、`/attached`、`/comm`、`/timings`（`apps/desktop/src/renderer/pages/agent/api.ts` 等） | 在 openapi 副本里把 `/health` 改名成 `/healthx`，同一个脚本随即把 `/health` 报为缺失。这证明脚本能查出缺失的端点，判据不是恒真 | **仍存在，且范围比原来大**（原文只提到 `/os`） |
| **F4b** `patch_os` 在 tokio worker 上做阻塞锁 / fsync | 全仓只有 `OsConfig::set_enabled` 一个调用方会去拿 `lock_exclusive()`（`os_config.rs:106/177`），而 `set_enabled` 只在 `spawn_blocking` 里被调用（`os_routes.rs:454-457`，回读也在 `:465`）。由 #183（SUP-5）修复 | 用同一个 grep 能查到 `render_at` 在 async 路径上同步读 `os.json`（`os_routes.rs:241-243`），说明这个方法能查出 async 路径上的同步 IO。但那是不拿锁的小文件读，不会无限期阻塞，不属于 F4b 说的那种危害 | **已解决**（#183） |
| **F4c** `os list` 回答的是临时 daemon 的状态 | `cmd_os` 用的是 `connect()`（`agent24-cli/src/main.rs:966`），而 `connect()` 在没有常驻 daemon 时会 `spawn_daemon(true)`（`:644`）。临时 daemon 的包根是一个**随机临时目录**（`agent24-os-packages/src/lib.rs:78-93`：「An ephemeral daemon must not read the real user's packages」）。所以在没有常驻 daemon 时：`os list` 会输出 **`(no domain OS installed)`**（`main.rs:1876-1878`），即使用户已经装了 sin90；`os enable/disable sin90` 会被当成「未知名字」拒绝。`offline_hint` 本来就是为「daemon 不在」这种情况写的（`main.rs:968-982`），但这条路径**永远走不到**，因为 `connect()` 从来不会失败到这一步 | `attach_only()`（`main.rs:664-671`）就是「只连常驻 daemon、绝不起临时实例」的现成实现，`os uninstall` 已经在用它。这说明代码里能区分这两种情况，`os list/enable/disable` 只是没有用上 | **仍存在，而且比原文写的更糟**：原文以为只是状态描述不准确，实际是会说出错误事实 |
| **F4d** `needs_models` 是常量，但措辞仍在断言 | 现在已经是真谓词：遍历磁盘包的 manifest，检查 `requires_models()` 非空且模块已启用（`server.rs:1769-1772`，#182 引入） | 同一段代码上方的注释（`server.rs:1760-1764`）仍写着「This is a CONSTANT, not a predicate」，和代码自相矛盾。这说明「对照措辞与代码」的方法能查出不一致 | **已解决**（#182）；只剩一段过时注释，放进 M2-05 顺手修，不单独立 task |
| **F4e** PATCH 放行 `Refused` 条目 | `patch_os_at` 遇到 `MountOutcome::Refused` 时直接返回 `admission_refused_response`（`os_routes.rs:835`）；判据测试 `criterion1_refused_blocks_and_does_not_persist`（`:1890`）等。由 #196（T8/ME-3g）修复 | 变异对照记在 #196 的判据测试里（如 `:1921`：把匹配分支放宽后测试变红） | **已解决**（#196）。仍有一处已知边界：同名多报告时无条件放行（criterion 6/7），这是 #196 有意划定的范围，不属于 M2 |

### 1.2 F3 · 命名空间 `/api/v1/os/<name>`

| 现状（证据） | 结论 |
|---|---|
| 仍是 `/api/v1/<name>`（`agent24-domain/src/lib.rs:547-549`）。「撞路由导致 panic」已经通过保留段名单 + 双向集合相等测试，变成了 `Refused`（`agent24d/src/domain.rs:181`、`:3353`），附着模块也用同一套检查（`is_reserved_kernel_segment`，`:233`）。**SPEC 的两条前提已不成立**：① 「今天没有外部消费者」不再成立：已发布的 Sin90 v0.5.1 manifest 写死了 `route_namespace: /api/v1/sin90`，内核要求它与派生值**逐字相等**（`agent24-domain/src/lib.rs:780-783`），所以一改就会拒绝所有已安装的包；此外 Cos72、`agent24-os-sdk`、桌面端，以及调度回调路径 `/api/v1/<ns>/_a24/scheduler/fired`（`os-proto/src/kernel_call.rs:72`）都会受影响。② `/api/v1/os/{name}` 这个位置**已经被内核自己占用**：`PATCH /os/{name}`、`/os/{name}/stop`、`/os/{name}/commands/{command}`（`server.rs:1168-1189`）。原方案原样执行会和内核路由相撞，要改就得另选前缀 | **需 jason 拍板**（见 §3 D1） |

### 1.3 F5 · 记忆层隔离一致性（SPEC §7 第 4 项）

| ID | 现状（证据） | 结论 |
|---|---|---|
| F5a KV 的 namespace / owner 模型 | 模块拿不到 `KvStore`：进程内句柄 `OsScopedMemory` 不暴露它；进程外的 RPC 只有 `_a24/memory/{private,scoped}/{get,recall,recent,remember,forget}`，没有任何 kv 方法 | **已解决**（F1 / T8.5c，#210–#225） |
| F5b `mem_events.scope_owner` CHECK 要用 `trim` | `0011_events_owner_trim.sql` 用的是双参 trim 字符集 | **已解决**（#138） |
| F5c `agent24-store` 列表查询没有 tie-breaker | `repo.rs:266/347/353/605/612/1022` 都带 `, id DESC/ASC` | **已解决**（#138） |

正对照：`0002_events.sql` 原本的约束是 `<> ''`，用同一个 grep 能命中，说明判据能区分修复前后。

### 1.4 F6 · 模块目录的 symlink 安全

| 现状（证据） | 结论 |
|---|---|
| 仍然只是检查：`prepare_dir` 先 `symlink_metadata` 再 `create_dir_all`（`agent24d/src/domain.rs:587-617`，文档已如实写明「This is a check, not a guarantee」）。进程外模块通过**路径字符串** `A24_DATA_DIR` 拿到自己的目录，然后自己去打开（`os-proto/src/launch.rs:37/566`）。所以即使内核改用 `openat`，子进程按路径重新打开这一段的 TOCTOU 仍然存在。要真正保证，就得把目录 fd 交给子进程（例如用 `agent24-os-cwd` 已有的 `install_current_dir_fd` 把它设成 cwd），而这会改 wire 契约 | **仍存在，但在当前威胁模型下不是真问题**：能利用 TOCTOU 的必须是同一 uid 的本地进程，而它本来就能直接写 `~/.agent24`。祖先目录是软链属于合法配置（比如 `~/.agent24` 放在外接盘上）。实际会发生的那种情况（`os/cos72` 软链指向 sin90 目录）已经被挡住。**需 jason 拍板**（见 §3 D2） |

正对照：包根 `ensure_packages_root`（`agent24-os-packages/src/lib.rs:128-200`）做了更强的检查（独占创建 0700、属主、权限），同时文档写明了它残留的 TOCTOU。这说明本仓库能区分「检查」和「保证」，这里的结论不是因为没看出差别而得出的。

### 1.5 F7 · 领域 OS 记忆配额与保留

| 现状（证据） | 结论 |
|---|---|
| **配额已经交付**：`mem_owner_usage` 由触发器维护，`mem_events_bi_quota` 在超额时以 `RAISE(ABORT,'mem_quota:rows'/'bytes')` 拒绝写入，默认 `'*'` = 200 000 行 / 256 MiB payload（`agent24-memory/migrations/0014_owner_usage_quota.sql`，#207）。**限流也已交付**：进程外 memory 的令牌桶跨 generation 不重置（`agent24d/src/os_memory.rs:582/622-636`，T8.5c-W-mount #216–#218）。**保留 / 压实没有做**：所有迁移里都没有 retention 或 expire 逻辑（grep 唯一的 `ttl` 命中是 0007 注释里的 `brute-force`，属于误命中）。**新发现**：`'*'` 默认配额同样作用于 **personal 分区**。M1 之后 agent loop 会把会话轮次写进 `mem_events`，一旦到顶，每一轮都会发出 `MemoryWriteFailed` 事件，记忆从此停止写入。对话本身不会中断（`agent24-agent/src/lib.rs:651-664`），但没有任何回收路径 | 配额和限流**已解决**；保留策略**仍未做**，并且已经和 jason 2026-10-07 在 C3 P1 登记的「事件日志原文永不删的体积治理」（`tasks.md` 顶部）是同一件事。**需 jason 拍板**归属（见 §3 D3） |

正对照：同一个 grep 在迁移里查 `quota` 有 30 处命中，说明这个检查能看到已落地的机制。

### 1.6 FU-45 · 模块是否需要知道请求的真实来源

| 现状（证据） | 结论 |
|---|---|
| 内核剥掉了 `x-forwarded-*` 整族和 `x-real-ip`（`os-proto/src/proxy.rs:105/206`），没有另外生成任何来源头。到期条件是「出现第一个真正需要来源的领域 OS」；用 `gh search code` 在 iDoris-ai、MushroomDAO 两个组织里搜 `X-Forwarded-For`，没有任何模块代码命中（唯一命中是一篇博客 markdown） | **未到期**（属于「已不适用于 M2」）。正对照：同一个搜索确实能命中那篇博客，说明搜索本身是有效的 |

### 1.7 顺带核实（C1 / C6 相关的 followups 与 issue）

| 条目 | 现状 | 结论 |
|---|---|---|
| **FU-70**（+ FU-54）`_a24/approval/{gate,advise}` 回调没有绑定所属请求的生命周期 | `approval_callback.rs` 里 `lifecycle` 的出现次数是 **0**；`broker.insert`（写审批行 + 推送 `module-approval.required`，`:204-215`）只受连接级 `CALL_TIMEOUT` 30s 约束。所属请求已经超时之后，仍可能凭空多出一条待审批记录 | **仍存在**。正对照：同一个 grep 在 `memory_callback.rs` 命中 9 处，在 `scheduler_callback.rs` 命中 6 处 `bind_to_lifecycle`；scheduler 的模块文档还点名「不复制 FU-70」（`scheduler_callback.rs:24-26`） |
| FU-41 临时包根没有所有权检查 | `ensure_packages_root` 已落地（`agent24-os-packages/src/lib.rs:128`），但台账还没勾 | **已解决**，M2-07 负责勾掉 |
| FU-49 回调要绑定到对应的那一代 | 回调 handler 持有的是每个连接自己的 `Arc<Generation>`（`approval_callback.rs:123/140`、`memory_callback.rs:127`），不经过 `Current::get()` | **已解决**，M2-07 负责勾掉 |
| FU-53 params 解析后的内存放大 | `PARAMS_MAX_NODES/DEPTH/STRING_BYTES`（`os-proto/src/rpc.rs:435-441`，#199） | **已解决**，M2-07 负责勾掉 |
| FU-10 契约漂移 CI | 与 F4a 是同一个问题 | 由 M2-03 治理路由部分；`os list` 的输出形状由 M2-02 的 schema 覆盖 |
| issue #661 | Open Design creative view 的 bounds 测试 / generation / 上限 clamp | **不属于 M-E**，留在 OD-M12（`PAUSED`） |
| issue #663 | M10 期间约 200 个 stacked PR 的评审遗留（workspace decoder、sidecar 等） | **不属于 M-E**，没有一条落在 `agent24-os-*` / `os_routes` / `domain` 上，留在 OD-M12 |

### 1.8 汇总

| 条目 | 结论 | 去向 |
|---|---|---|
| F4a | 仍存在（11 条路由 + 没有覆盖门） | M2-02 / 03 / 04 |
| F4b | 已解决 #183 | M2-07 记账 |
| F4c | 仍存在（会说出错误事实） | M2-05 |
| F4d | 已解决 #182（剩一段过时注释） | 随 M2-05 |
| F4e | 已解决 #196 | M2-07 记账 |
| F3 | 需拍板（原方案的两条前提已不成立） | §3 D1 |
| F5a/b/c | 已解决（F1 / T8.5c、#138） | M2-07 记账 |
| F6 | 仍是检查而非保证；当前威胁模型下不是真问题 | §3 D2（条件 task M2-08） |
| F7 | 配额与限流已解决；保留策略未做 | §3 D3 |
| FU-45 | 未到期 | 不动 |
| FU-70 / 54 | 仍存在 | M2-06 |
| FU-41 / 49 / 53 | 已解决，但台账未勾 | M2-07 |

---

## 2. Task 列表

原则：只修真问题；每个 task ≤ 300 行（不含生成文件 `packages/api-client/src/openapi.d.ts`）；验收先写成测试并证明它在修复前失败。**高风险类**（安全 / 权限 / 数据写入 / 迁移 / 协议）必须等 PR-Daemon APPROVE 后才能合并。

核实后多数条目已经解决，真正还要做的只有 5 个代码 task，加 1 个计划 task、1 个收尾 task 和 1 个条件 task。**不为凑够 10 个而扩大范围。**

| ID | 短名 | 内容 | 依赖 | 高风险 | 估算 |
|---|---|---|---|---|---|
| M2-01 | `plan` | 本计划 + `tasks.md` 台账 + `roadmap.md` M2 启动标注 | — | 否 | 文档 |
| M2-02 | `openapi-os` | 补 `/os` 族四个端点的契约 | M2-03 | **是**（对外协议） | ~250 |
| M2-03 | `route-gate` | 路由 ↔ OpenAPI 双向覆盖门（Rust 测试） | — | 否 | ~150 |
| M2-04 | `openapi-attached-timings` | 补 `/attached`、`/timings` 两族契约 | M2-03 | **是**（对外协议） | ~200 |
| M2-05 | `os-cli-no-ephemeral` | `os list/enable/disable` 不再回退到临时 daemon；修 F4d 过时注释 | — | 否 | ~150 |
| M2-06 | `approval-lifecycle` | 审批回调绑定所属请求的生命周期（FU-70 / FU-54） | — | **是**（权限 + 数据写入） | ~200 |
| M2-07 | `closeout` | SPEC / followups / tasks 记账收口，按 D1–D3 的裁决落文 | M2-02…06、D1–D3 | 否 | 文档 |
| M2-08 | `os-dir-openat` | **条件 task**：只有 D2 选 B 时才做 | D2 = B | **是**（安全） | ~250 |

建议入队顺序：M2-03 → (M2-02 ∥ M2-05 ∥ M2-06) → M2-04 → M2-07。M2-05 和 M2-06 与契约线互不依赖，可以并行。

### M2-02 `openapi-os` — 补 `/os` 族契约

- **做什么**：在 `protocol/openapi.yaml` 中加入 `GET /os`、`PATCH /os/{name}`、`POST /os/{name}/stop`、`POST /os/{name}/commands/{command}`，以及 `DomainOsList` / `DomainOsView` / `DomainOsUpdate` 和 stop / command 的响应 schema。字段逐一对照 `agent24-protocol` 的 Rust 类型；错误码对照 `os_routes.rs` 中的各个 `*_response`（`attached_module` 409、`admission_refused`、`registry_invalid` 503、`stop_failed` 等）。然后跑 `pnpm gen:api`，让 `@agent24/api-client` 导出 `DomainOs*`。
- **不做**：桌面端改用生成类型（可以留作后续，不在 M2 范围内）；不改任何运行时行为。
- **验收（可证伪）**：① 从 M2-03 的 `UNDOCUMENTED_KERNEL_ROUTES` 里删掉 `/os` 族四条。**删掉之后、openapi 补上之前**，覆盖门测试必须变红（PR body 贴出这次失败的输出）；补上之后变绿。② 新增一个契约测试：取 `os_routes` 现有单测里真实的 `DomainOsList` 响应 JSON（含 `registry_error`、`resources: missing`），用 openapi schema 校验通过。正对照：删掉一个必填字段后校验失败。③ `pnpm lint:openapi` 通过，`pnpm gen:api` 之后无 diff。

### M2-03 `route-gate` — 路由 ↔ OpenAPI 覆盖门

- **做什么**：在 `agent24d` 里加一个测试，沿用 `reserved_segments_match_the_kernel_routes_exactly`（`domain.rs:3353`）扫描源码的办法：从 `build_router_with_modules` 提取全部 `.route("…")`，与 `protocol/openapi.yaml` 的 `paths` 做**双向集合相等**比较。允许偏离的只有两张显式名单，每条都要写明理由和负责的 task：
  - `UNDOCUMENTED_KERNEL_ROUTES`：今天这 11 条。`/os*` 由 M2-02 清掉；`/attached*`、`/timings*` 由 M2-04 清掉；`/capabilities*` 归 OD-M11（`PAUSED`）；`/events` 是 WS，契约在 `events.schema.json`。
  - `MODULE_SURFACE_PATHS`：`/sin90/*` 七条。它们是模块面，不是内核路由；是否继续留在内核契约里，在 M2-07 记一笔，不在本 task 决定。
  - `/api/v1/comm/*` 不在这个函数里，沿用 `EXTRA_RESERVED_NOT_IN_BUILD_ROUTER` 的处理方式，写明它是已知的盲区，归 COMM 线。
- **为什么要双向相等**：新加一条路由却不写契约时测试会红；名单里的条目已经被补进契约、名单却没跟着删时测试也会红。名单因此不会悄悄过期。
- **验收（可证伪）**：① 在测试内置的源码样本里多加一条假路由，测试变红；② 在 openapi 样本里多加一条内核没有的路径，测试变红；③ 在真实代码上测试为绿。PR body 贴出 ①② 两次变红的输出。

### M2-04 `openapi-attached-timings` — 补 `/attached`、`/timings` 两族契约

- **做什么**：补 `POST/GET /attached`、`DELETE/PATCH /attached/{name}`、`GET /timings`、`GET /timings/summary` 的契约和 schema（桌面端已经在调用：`/attached` 见 `apps/desktop/src/renderer/pages/voice/api.ts:21`、`shared/ipc-types.ts:220`；`/timings` 见 `pages/voice/VoicePanel.tsx`）。`AttachedView` 必须写明**永远不含 token 和 token hash**（`attached.rs:795-796`）。然后跑 `pnpm gen:api`。
- **验收**：与 M2-02 相同，先从名单里删掉对应条目，覆盖门变红，补上后变绿。另外加一个契约测试，用 schema 校验 `list_attached` 的真实响应；正对照是往响应里塞一个 `token` 字段后校验失败（schema 设 `additionalProperties: false`）。

### M2-05 `os-cli-no-ephemeral` — `os` 子命令不再回退到临时 daemon

- **做什么**：`cmd_os` 的 `List / Enable / Disable` 分支改用 `attach_only()`（`main.rs:664`），替换现在的 `connect()`。没有常驻 daemon 时：
  - `os list`：明确输出「daemon 未运行」，再给出现有的 `offline_hint` 指引；**不得**输出 `(no domain OS installed)`。
  - `os enable/disable`：直接返回现有的 `offline_hint` 错误。`main.rs:968-982` 这条路径今天走不到，改完后才真正生效。
  - 顺手把 `server.rs:1760-1764` 那段「This is a CONSTANT」的过时注释改成与代码一致（F4d 的残留）。
- **不做**：不新增离线读 `os.json` / 包根的本地视图。真需要再说，避免把它做成第二个注册表实现。
- **验收（可证伪）**：CLI 集成测试，用临时 `HOME` 并预装一个包，不起 daemon：① `agent24 os list` 的输出含「not running」，不含 `(no domain OS installed)`，且整个过程**没有起任何 agent24d 进程**（检查 state 文件 / 临时包根目录都没有被创建）；② `agent24 os disable <name>` 以非零码退出，输出里有 `os.json` 的编辑指引。修复前跑 ① 必然失败（会输出 `(no domain OS installed)`），PR body 贴出这次失败的输出。

### M2-06 `approval-lifecycle` — 审批回调绑定请求生命周期（FU-70 / FU-54）

- **做什么**：`ApprovalSubmitHandler`（gate / advise）改成和 `memory_callback.rs:127` 一样的形状：先通过 `admit_callback_bound(request_id)` 拿到 lifecycle，再把「token 准入 + `broker.insert`」包进 `bind_to_lifecycle`，超时就映射为 `timeout`（与 `scheduler_callback.rs:385` 的映射一致）。`status` 是独立查询，**保持不绑定**（`approval_callback.rs:226-229` 已写明理由）。
- **必须先在 PR 里写清楚（一页语义说明，PR-Daemon S1 规则）**：① 生命周期判定与 token 消费的先后顺序。所属请求已经结束时，token **不得**被消费，审批行**不得**写入。② 幂等查找（Step 3）命中已有行时，是否仍要求请求存活（建议：不要求，原样返回已有行，与今天的语义一致）。③ insert 已经提交后才超时，返回 `timeout` 时那条行怎么处理（建议：保留该行；返回之前先查一次，如果已提交就照常返回成功，不能出现「写入成功却报 timeout」）。
- **验收（可证伪）**：采用 FU-70 原文的判据。用存储层人为拖慢的桩件，在所属请求只剩约 100ms 时发起 `gate`，约 100ms 后收到 `timeout`，且 `module_approvals` 表里**没有新行**；正对照是同样的桩件、请求剩余时间充足时，调用正常完成并写入一行。修复前第一个断言必然失败（会写入一行，并在约 30s 后才返回）。另外补一组变异验证：去掉 `bind_to_lifecycle` 包裹后测试变红。

### M2-07 `closeout` — 记账收口

- 在 `SPEC-ME-FOLLOWUPS.md` 顶部加「2026-10 状态」段，逐条写 F3–F7、FU-45 的结论和证据（取自本文 §1）。正文不改，保留出处。
- `followups.md`：勾掉 FU-41（`ensure_packages_root`）、FU-49、FU-53（#199）、FU-70 / FU-54（M2-06 的 PR）；FU-10 标注「路由部分由 M2-03 承接」；FU-19 跟随 D2 的结论。
- 按 D1–D3 的裁决：D1 结果写进 ADR-029 的后续说明（选择维持现状时，只追加一段「F3 已裁决关闭」）；D3 选择移交时，把「personal 分区默认配额到顶」写进 C3 P1 的体积治理条目。
- `tasks.md` M2 台账置为 `DONE`；`roadmap.md` M2 标为收口。

### M2-08 `os-dir-openat`（条件 task：只有 D2 选 B 时才做）

- **做什么**：`prepare_dir` 改用 `rustix`（`agent24d` 已开启 `fs` feature，见 `apps/agent24d/Cargo.toml:70`），从 state dir 的 fd 开始，逐级用 `openat(O_DIRECTORY|O_NOFOLLOW)` 和 `mkdirat` 打开或创建 `os/` 与 `os/<name>`，在内核这一侧消掉 `symlink_metadata → create_dir_all` 之间的 TOCTOU。**文档要如实写明仍然做不到的事**：子进程按 `A24_DATA_DIR` 路径重新打开；`~/.agent24` 本身及其祖先是软链时照样跟随（这是合法配置）。
- **验收**：① `os/<name>` 是软链时被拒（沿用现有测试）；② `os/` 本身是软链时被拒（新增，修复前会通过）；③ 正常路径照常创建。

---

## 3. 裁决记录（jason，2026-10-07）

### D1 · F3：维持 `/api/v1/<name>` + 保留段名单；关闭 F3

- **裁决：维持 `/api/v1/<name>` 加保留段名单，关闭 F3。**
- **理由**：
  1. **原方案和内核撞路由。** `/api/v1/os/{name}`（PATCH）、`/os/{name}/stop`、`/os/{name}/commands/{command}` 已经是内核路由（`server.rs:1168-1189`）。真要改，就得换一个新前缀（比如 `/api/v1/m/<name>`），这是另一份设计。
  2. **「今天没有外部消费者，越早越便宜」已不成立。** 已发布的 Sin90 v0.5.1 和 Cos72 的 manifest 都写死了 `route_namespace`，内核要求逐字相等（`agent24-domain/src/lib.rs:780`）。一改，所有已安装的包都会变成 `Refused`，必须跨仓库协同重新发版。调度回调的路径约定、SDK、桌面端也要跟着改。
  3. **原来要解决的那个危害已经被转化了**：撞名从「daemon 起不来（panic）」变成了「该模块 `Refused`」，并且有双向测试钉住（`domain.rs:3353`）。
- **维持现状的代价（要说清楚）**：内核每新增一个顶层段，就会占掉一个模块名（例如 M1 新增的 `memory`），已经安装的同名模块升级后会被 `Refused`。缓解办法是一条约定，不需要写代码：**内核新路由优先挂在已有的段下面**，新增顶层段要在 PR 里说明理由。
- **如果选择改**：不放进 M2。单独开一个里程碑，做 manifest v2 双栈（一个版本内两个前缀都接受），并与 Sin90 / Cos72 / SDK 协同发版。

### D2 · F6：接受检查级 symlink 处理；关闭 F6，不做 M2-08

- **裁决：A，接受「检查」级别，关闭 F6，不做 M2-08。** 理由：要利用这里的 TOCTOU，必须是同一 uid 的本地进程，而它本来就能直接改 `~/.agent24` 下的任何东西。实际会发生的误配置（模块目录软链到另一个模块）已经被拦住，契约措辞也已经收窄。真正的保证需要把目录 fd 交给子进程，这会改 wire 契约，代价和收益不成比例。
- **B：做 M2-08。** 只在内核这一侧把 `os/` 和 `os/<name>` 改成 `openat` 逐级打开，代价约 250 行。子进程按路径重开这一段仍然不保证。
- **C：做到底**：用 fd / cwd 把目录交给子进程，并改 wire 契约。不推荐放进 M2。

### D3 · F7：移交 C3 P1，与日志体积治理合并；P1 优先处理

- **裁决：移交 C3 P1**，与 jason 2026-10-07 登记的「事件日志原文永不删的体积治理」（`tasks.md` 顶部、`MEMORY-STRATEGY.md` §4.2）合成一件事，M2 不做。理由：配额和限流已经交付，「留什么、删什么、压实成什么」属于记忆产品的决策，不是 M-E 内核的收口。
- **必须优先处理**：`'*'` 默认配额（20 万行 / 256 MiB）同样作用于 personal 分区。M1 之后会话轮次会写进 `mem_events`，一旦到顶，记忆会静默停止写入：每一轮都会发出 `MemoryWriteFailed`，对话不会中断，但也没有任何回收路径。将默认配额压在 personal 分区的风险、静默停写及恢复/告警纳入 C3 P1 体积治理验收。

---

## 4. 不在 M2 的（防止范围蔓延）

- 桌面端把手写的 `/os`、`/attached` 类型换成生成类型。等契约补齐后再看是否值得做。
- `/capabilities*`（OD-M11，`PAUSED`）、`/api/v1/comm/*`（COMM 线）的契约。
- FU-76（`os list` 显示 `model_access`）。这是功能缺口，不是 bug。
- FU-1…FU-5（分区目录的诊断信息）、FU-2（记忆库打不开时只打 warn）。这些属于 C3，不属于 M-E 收口。
- issue #661 / #663，都属于 OD-M12。
- K-3（OpenAPI 改为由 Rust 类型生成）。M2-03 只做覆盖门，不做生成。
