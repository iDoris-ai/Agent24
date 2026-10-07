# ADR-DOC-01：Documenting 的定位、进程形态与对外暴露（H1 / H2）

> **状态**：Proposed。生效需 PR-Daemon APPROVE 加 jason 确认，生效后经 `ab/documenting` 的首个 release PR 进 `main`。**DOC-1 的实现必须在这之后开工。**
> **日期**：2026-10-07 · **作者**：David Xu · **Issue**：#701（DOC-1-02）
> **编号**：本目录独立编号（`ADR-DOC-xx`），先例是 `docs/open-design-workspace/design/ADR-00x`，目的是避免和各方向并行追加的全局 ADR 号撞号。
> **依据**：
> - [README](../README.md) §2.1 / §10.3 / §16 / §17 #12；
> - [BASELINE](../BASELINE.md)；
> - #685 补充架构评审 H1 / H2 / M1–M4；
> - `docs/ARCHITECTURE-LAYERS.md`；
> - ADR-029 / ADR-031（`docs/decision.md`）；
> - `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md`；
> - `docs/agent/PLAN-OOP-OS-AND-BACKLOG.md` §七。
>
> 文中行号均按 `dbffe94` 核对。

## 0. 范围

本 ADR 裁定 H1 和 H2，具体包括：

- 定位、进程形态、命名；
- Agent 调用方式，以及它的风险、隐私与审计要求；
- 桌面通道、默认安装、存储归属；
- 平台支持与依赖登记。

操作契约的细节放在 **ADR-DOC-02**（同属 #701）：操作清单、唯一提交点、类型化错误、job、幂等。

## 1. 现状

| 项 | 事实 |
|---|---|
| 进程内 ① | `KERNEL_GRANTS` 只有 Events / Memory / Approval（`rust/apps/agent24d/src/domain.rs:75`），**没有 Models 句柄**。生产 catalogue 为空 |
| 进程外 ② | 已有 Sin90 / Cos72 在用。<br>• 调模型：manifest 需要在 `kernel_capabilities` 里申请 `models`，`model_access` 默认 `local_only`（`agent24-domain/src/lib.rs:235`、`:819-826`）。<br>• 传输：Unix 域套接字（SPEC-ME3 FU-60） |
| 命名 | 命名空间和数据目录都**从 `name` 推导**：必须是 `/api/v1/{name}` 和 `~/.agent24/os/{name}/`（`lib.rs:546-553`、`:780-800`） |
| 公开代理 | 对外入口 `/api/v1/<ns>/*`，限制如下：<br>• body 上限 1 MiB；<br>• 首字节 10 s，总时长 30 s；<br>• 不支持流式；<br>• 剥掉双向的 `X-A24-*` 头；<br>• 客户端请求 `_a24` 路径一律返回 404（`agent24-os-proto/src/proxy.rs`） |
| 内核 → 模块 | `send_kernel_request` 调模块私有的 `/api/v1/<ns>/_a24/...`，每次调用可单独设 `KernelLimits`，不占客户端的并发配额（`os-proto/src/kernel_call.rs:235`）。调度器投递就走这条路（`scheduler_deliver.rs`，`x-a24-fire-id`）。<br>**它只返回状态码**：响应体在限额内读完即丢弃（`KernelResponse { status }`，`:88-95`）。调用语义按“可能重复投递”设计 |
| 工具 | 领域 OS 不能向 agent loop 贡献工具。<br>• 工具表在 `mount_all` 之前构建并冻结（`server.rs:1523-1599`）。<br>• MCP 工具强制为 `External`（`agent24-mcp/src/lib.rs:269-273`）；第三方声明的风险等级“只是猜测”（`agent24-tools/src/lib.rs:400-434`）。<br>• `Read` 类直接跳过审批 |
| 审批类别 | `BrokerGate::check` 按工具**名字**决定类别，未知名字一律落到 `module`（`agent24-policy/src/lib.rs:897-902`）。这只是兜底，不是为模块设计的接缝 |
| 记忆 | agent loop 会自动把每轮的 prompt 和最终回答写进内核记忆（`remember_exchange`，`agent24-agent/src/lib.rs:641`、`:1730`） |
| 桌面 | 运行时只有通用的 `backendProxy`（只能传 JSON、转发任意路径，F11.0 待隔离）。渲染进程只用了 `api-client` 的**类型**。`agentear-events.ts` 是 AgentEar 专用的事件桥 |
| 权限 | 路由级授权、可持久的 run 归属、多用户都是 📐。F11.0 / OD-M11 处于 `PAUSED` |

## 2. 决策

### D1 定位：第一方领域 OS，名词归属采用 §七 的 C

- **数据所有权**：Documenting 拥有原件、版本和产物，存在自己的 `data_dir`。
- **跨 OS 读取**：采用 PLAN-OOP §七 的建议 C（“先做 C，把 B 当方向”）。其他 OS 读文档**必须经内核、走显式授权**，**不允许**直接读 `data_dir`。
  - 目前**没有**模块到模块的调用路径（📐），模块也不能调用 Agent 工具。
  - 在这条路径出现之前，跨 OS 读取不可用。它已登记在 §3。
- **向 B 迁移的准备**：“document”很可能是要回收到内核的名词。所以对外一律只用稳定的 `document_id + revision` 寻址，不暴露存储布局，降低以后迁移的成本。
- **内核职责不变**：run / 模型 / 工具 / 审批 / 审计的权威仍是内核。

### D2 进程形态：② 进程外

| 维度 | ① 进程内 DomainModule | ② 进程外 OS（采用） |
|---|---|---|
| 调模型 | 没有 Models 句柄，需要新增 `Capability` | 有，申请 `models` 即可，默认 `local_only` |
| 先例 / 趋势 | catalogue 为空；T11 已迁出 | Sin90 / Cos72 |
| 解析或 OCR 崩溃 | 拖垮 daemon | 隔离在自己的进程里 |
| **Windows** | **不受 DEP-C2 阻塞** | **Unix 套接字，要等 DEP-C2（`BLOCKED`）** |
| 入站限制 | 不经代理，但路由同样受 1 MiB `MAX_BODY_BYTES` 限制 | 公开代理 1 MiB / 30 s；内核调用另设 `KernelLimits` |

选 ② 的代价是 **Windows 暂时不可用**（D10）。这是产品层面的取舍，需要 **jason 明确确认**。如果不能接受，替代方案是 ① 加上新增 Models `Capability`，那条路要另写内核 ADR。

### D3 命名

`name: documents`，由此推导出：

- 命名空间：`/api/v1/documents`（`documents` 不在内核保留段里）；
- 数据目录：`~/.agent24/os/documents/`；
- 事件：`payload.module = documents`；
- 工具名前缀：`documents.`。

代码同仓：

- OS 二进制：`rust/apps/agent24-documents`，基于 `agent24-os-sdk`；
- 页面：`apps/desktop/src/renderer/pages/documents/`。

### D4 Agent 调用方式：模块声明工具（规模等同“新增 Capability”）

| 方案 | 结论 | 理由 |
|---|---|---|
| A 内核内置代理工具 | 否决 | 内核里会出现领域名词“document”，违反 D1 和 T11 |
| B MCP | 否决，也不作为过渡 | 固定 `External`；固定 60 s 超时；由 `mcp.json` 拉起第二个实例，需要另配凭据回连 OS |
| **C 模块声明工具** | **采用** | 对所有领域 OS 通用，不在内核里放领域名词 |

按 ARCHITECTURE-LAYERS §5，C **与“给 `Capability` 加新能力”同等规模**：manifest 契约、os-sdk、os-proto 的 `_a24` 路由、工具通告、审批类别都要改。所以它需要一份**内核侧 ADR**，按 ME-4 的切法逐片落地。参考的先例是 A3 `host_commands`（manifest 声明可调用命令，`attach_commands.rs`）。本 ADR 只规定最低要求：

1. **申请与授权**：manifest 用 `tools:` 段申请工具，必须和内核授权取交集。启用 OS 时，用户要确认工具清单。
2. **风险等级**：
   - 非第一方模块的工具默认按 `External` 处理，模块自己声明的等级只当作猜测（与 MCP 一致）。
   - `Read` / `WriteLocal` 只授予两类工具：**随安装包分发、pin + sha256 校验过的第一方 package**，或者**用户在启用时逐个确认过的工具**。
   - 用户覆盖时只能收紧。
   - DOC-1 期间还没有“第一方默认安装”（D6），Documenting 自己的工具也因此是 `External`，每次调用都要审批，除非用户在启用时逐个确认过。这一点如实写进 DOC-1 的验收说明。
3. **调用通道**：
   - 内核调用 `POST /api/v1/<ns>/_a24/tools/<op>`，带 `x-a24-run-id` 和 `x-a24-tool-call-id` 头。客户端碰不到 `_a24` 路由，也伪造不了 `X-A24-*` 头。
   - 现有的 `send_kernel_request` 只返回状态码。内核 ADR 要新增一个**返回响应体**的变体，响应体以 `max_response_bytes` 为上限，这个上限也就是工具输出的上限。
   - 工具调用**绝不自动重试**（调度器那条路径默认按“可能重复投递”设计）。`tool_call_id` 作为模块侧的幂等键。
   - 每个工具的 `KernelLimits.total` 不超过注册时的超时。
   - `run_id` 对模块只是信息，内核**不接受**模块在任何回调（审批、模型、事件）里回传的 `run_id`。
4. **注册与可用性**：
   - 工具表在 `mount_all` 之前冻结，所以代理工具要用延后绑定的句柄，参照 `ModuleDeliverer` 的 `OnceLock<Arc<Supervisors>>`。
   - 通告前按模块的实时状态过滤：停用、退避、熔断时都不通告，调用返回类型化的 `module_unavailable`。
   - README §10.3 要求的更细的可用性（引擎是否就绪、知识上下文是否就绪）由 ADR-DOC-02 规定。
5. **审批**：新增 `module:<name>` 审批类别。这是**对策略的改动**，不是复用现有兜底。Guardian 的 `always_review` 按类别配置。
6. **输出标记**：模块工具的输出一律标为**不可信证据**（README §9.3），不能变成指令。
7. **隐私与审计**：见 D8、D9，二者都是 C 的组成部分。

**依赖与验收**：C 是 DOC-1“对话入口”的依赖。

- DOC-1 第 1 片可以先合入页面 + REST；
- **但在 C 落地之前，DOC-1 不能整体验收**，看板上的对话入口项标为 Blocked；
- 不拿 A、B 凑合。

本 PR 同步修改了 README §14 的验收说明。

### D5 桌面通道：类型化 preload

1. **`window.agent24.documents.*`**：主进程只放行固定的路由模板，类型取自 `api-client`，**不给渲染进程任意路径**，不经过 `backendProxy`。
2. **文件导入导出**：
   - 主进程弹原生文件对话框；
   - 用 `application/octet-stream` 的**原始字节分块**，每块 ≤ 768 KiB，留出余量，避免超过 1 MiB 的 body 上限；
   - 偏移量和 sha256 放在**非 `X-A24`** 的请求头里，支持续传；
   - 公开 API 不传绝对路径（ADR-001）。
   - 二进制 IPC 是主进程里的新代码。
3. **长任务进度**：OS 发 `module` 事件。主进程**新增一个通用的模块事件桥**（或者把 `agentear-events.ts` 通用化），按 `payload.module = documents` 转发给渲染进程的类型化订阅。
4. **边界**：不经 IPC 直连引擎。

方向和 F11.0 一致（高权限接口隔离、改用专用 IPC）。代码由 Documenting 写，desktop owner 评审。

### D6 默认安装与引擎

- **OS 本体**：小体积的 Rust 二进制，作为第一方 package 随安装包分发（pin + sha256），首次启动时自动 install 并 enable。
  - 这个机制目前没有（属于 Agent24 依赖，发布前需要）。
  - DOC-1 用现有的 `agent24 os install`。
- **引擎**：一律放进程外，作为**按需下载组件**（DEP-A9 模式），不进安装包。
  - 通用化安装器属于 Agent24 依赖，DOC-1 第 2 片（模板）就需要。

### D7 存储

- 原件、版本、产物都在 `data_dir` 里（ADR-029：用内核记忆，但不拥有它）。
- **DOC-1 不使用 kernel scratch workspace。**
- 存储布局和唯一可编辑权威（#690）由 ADR-DOC-02 规定。

### D8 隐私：两条路径 + run 污点

1. **OS 自己调模型**：manifest 申请 `models`，并写上 `model_access: local_only`（和默认值相同，显式写出）。内核按 `LocalOnly` 路由，远端 fail-closed。
2. **文档内容进入 Agent 对话**：工具声明 `output_privacy: local_only`。内核要做到以下几点，这些都是 C 的组成部分：
   - **通告条件**：在 per-run `TaskProfile`（ID-1）落地前，只在 `ModelRouter` **没有远端档位**时通告这类工具（需要新增 `has_remote()`；router 在 daemon 生命周期内是静态的）。“用户为 Documenting 显式允许远端”的开关是新的内核 + UI 工作，第一版不做。
   - **run / session 污点**：一旦有 `local_only` 工具返回，这个 run 和 session 就被标记为污点。污点**持久化在 session 行上**，因为 session 跨重启存活。被污染的 run：
     - **按风险等级屏蔽**：所有 `External` 工具都不再通告，其中包括 `http_fetch`、MCP 工具，以及其他 OS 的模块工具，同时禁用 standing grant。只有同样声明了 `local_only` 输出隐私的工具例外。
     - **`Exec` 类工具**（如 `shell_exec`）仍走人工审批，但审批界面必须显示污点警告。
     - **跳过 `remember_exchange`**，即不写内核记忆，满足 M1。
     - **拒绝在远端路由上续接这个 session**。

在这些落地之前，对话入口不开放（与 D4 的验收一致）。

### D9 审计

- **现状**：工具调用会存成数据行和事件，但**哈希链审计**只记被拒、审批、风险覆盖和 workspace 保留。
- **C 的要求**：模块工具的每次执行（成功或失败）都写一条哈希链审计，actor 为 `module:<name>`。
- **OS 自身**：在 `data_dir` 维护一份只追加的操作日志，覆盖提交、恢复、导出、删除。
- **与 README §10.3 的显式偏离**：§10.3 承诺“页面直接编辑与 Agent 调用的审计轨迹相同”。但页面经公开代理发起的变更，内核审计不到，**目前只有 OS 的操作日志**。这一项标为**发布阻塞**：首个面向用户的发布之前，必须由内核提供模块变更审计。README §10.3 已同步注明。

### D10 平台

- 在 DEP-C2 完成前，**只支持 macOS / Linux**。Windows 上页面显示“不支持”，不降级。
- 这一点与 README §2“默认基础能力”的定位存在张力，所以列为 jason 的确认项（D2）。

## 3. H2：单用户声明与依赖登记

**DOC-1 至 DOC-3 只支持单用户本机**（legacy 全权 bearer）。界面和文档都不声称多用户、隔离或分权。

下表的负责人和时间点均由 review 定稿（**jason 指定**），在此之前视为未承诺。“何时需要”用里程碑表示，不写日期。

| 依赖 | 何时需要 | 拟议负责方 | 状态 |
|---|---|---|---|
| C 模块声明工具（含 D8 污点、D9 执行审计、内核 ADR） | DOC-1 整体验收（对话入口） | Agent24 kernel | 📐 |
| `ModelRouter::has_remote()`、工具通告过滤、持久化污点 | 同上 | Agent24 kernel | 📐 |
| 启用 OS 时的工具确认界面与模块工具授权 | 同上 | Agent24 kernel / desktop | 📐 |
| 跨 OS 授权读取路径（模块 → 内核 → 模块） | 第一个需要跨 OS 读文档的场景 | Agent24 kernel | 📐 |
| D5 类型化 preload、二进制 IPC、模块事件桥 | DOC-1 第 1 片 | Documenting 实现，desktop 评审 | 随第 1 片交付 |
| OOP 开发安装（`agent24 os install`） | DOC-1 | 已有 | ✅ |
| 通用按需组件安装器（DEP-A9 通用化） | DOC-1 第 2 片 | Agent24 desktop | 📐 |
| 第一方 OS 默认安装 | 首个面向用户的发布 | Agent24 desktop / kernel | 📐 |
| 模块变更的内核审计（页面路径） | 首个面向用户的发布（**阻塞**） | Agent24 kernel | 📐 |
| per-run `TaskProfile`（ID-1） | 解除 D8 第 2 条的过渡限制 | Agent24 decide | 📐 |
| 路由级授权、可持久的 run 归属（F11.0 / OD-M11） | DOC-4 之后（多用户） | Agent24 | `PAUSED` |
| 多用户 / 组织身份（SPEC-ORG-SPACE F9 / F10） | DOC-4 之后 | Agent24 | 未排期 |
| Windows OOP（DEP-C2） | Windows 支持 | Agent24 deployment | `BLOCKED` |

## 4. 代价与后果

- **对话入口取决于内核的 C**，而 C 的规模等同新增 `Capability`。这是有意为之：不在内核里放领域名词，也不用 `External` / MCP 绕过去。
- **Windows 暂不可用**，与其他进程外 OS 一致，待 jason 确认。
- **分块传输与事件桥**多了一层实现，换来的是不突破代理上限、不再新用 `backendProxy`。
- **审计**在 C 和“页面审计”落地前不完整，已显式标为偏离和发布阻塞。

## 5. 本 ADR 的验收

1. PR-Daemon APPROVE + jason 确认三项：D2/D10 的 Windows 取舍、§3 的负责人、D4 的验收约束。确认后状态改为 Accepted，随首个 release PR 进 `main`。
2. README §17 #12 指向本 ADR。§16 的平台依赖指向第 3 节。
3. 内核侧 C 的 ADR 由负责方在 `ab/documenting` 之外另开，本 ADR 只是它的需求方。
