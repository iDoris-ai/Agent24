# ADR-038 C9：`requires` 接入授权——实施方案

> 日期：2026-11-12。状态：方案草案，待 jason 拍板第 7 节后冻结，每期先方案后写。
> 来源：jason 2026-11-12 拍板，X11 入站准入收尾后主线是把组件规范化做完，先 C9，再 ADR-040 技能加载器 v1。C9 也是去掉 wechat-bridge 开发态 legacy 全权令牌的前提（`docs/tracking/2026-11-10/INBOUND-ADMISSION-PLAN.md:245` 第 8 条「开发态继续用 legacy 令牌（C9 未排期）」）。
> 本文只写方案，不含代码。代码行号取自 main `bb03081e8`。

## 1. 一页结论

1. 现在 `requires` 只被解析，没有任何授权路径读它（`rust/crates/agent24-components/src/lock.rs:38`）。但授权并不是空白：进程外 OS 包和 attach 模块的内核能力（events、models……）已经按「模块自报的请求 ∩ 宿主愿意给的」发放（`attach_grant.rs:49`、`domain.rs:2174`），缺的是第三项「用户同意」和统一的审计、展示。
2. 下载型组件（iDoris、Hyphae）从来不持有 daemon 令牌：iDoris 是 daemon 调用的 HTTP 服务，Hyphae 是 daemon 调用的 CLI，它们不回调 HTTP API。所以「组件不再持有 legacy 全权令牌」对它们已经成立，真正还在用 legacy 令牌的是开发态的 wechat-bridge。
3. 方案：把 `requires` 统一成一个小模型，由一个纯函数解析器（`requires` ∩ 该形态宿主愿意给的 ∩ 用户同意）算出「内核能力」和「API 操作」两份授权；未声明即拒，声明了但不在上限内也拒，两类都带封闭的拒绝原因码。
4. 内核能力沿用现有 Grants 通道，只是把授予前加上同意记录检查；API 操作是新东西：新增 `Audience::ComponentRuntime` 令牌，由 daemon 按组件签发，操作集合 = `requires.operations` ∩ 一份显式的组件操作上限 ∩ 同意，默认拒绝新增的 Operation。
5. 升级时 `requires` 的指纹变了，旧同意作废、新增部分不发放，直到用户重新同意；没变就沿用，不能只凭版本号（ADR-K1-03 §3、§4）。所有授予、拒绝、撤销、要求重新同意都写内核审计（ADR-K1-04 §2.1），审计写不进就不发放。
6. wechat-bridge 先补一份清单（`requires.operations = ["inbound.ingest"]`），换成只能调入站这一条路由的受限令牌，令牌绑定来源 `wechat`，由桌面在场确认后经 CLI 签发。legacy 模式本身不动（别的开发工具还在用），只去掉桥对它的依赖。
7. 共 12 片（C9-0 至 C9-11，含一片迁移），每片 <300 行非测试代码；大头是令牌类型与中间件（C9-5、C9-6）和 wechat-bridge（C9-8、C9-9）。需要 jason 拍板 3 件事（第 7 节），其余按推荐走。

## 2. C9 的准确定义

### 2.1 ADR-038 原文要求（逐条）

| # | 要求 | 出处 |
|---|---|---|
| R1 | C9 是「D7 `requires` 授权接线：令牌按 `requires` ∩ grants ∩ 同意发放，组件不再持有 legacy 全权 token」，待排、未定期 | `docs/adr/ADR-038-component-contract-v1.md:370` |
| R2 | 组件能调用的宿主接口 = `requires` 声明 ∩ 宿主愿意给的（`Grants::granting`）∩ 用户按 ADR-K1-03 确认的，三者任一缺失即拒绝 | 同上 `:292` |
| R3 | 令牌由 CORE-2 按组件发放，组件不得持有 legacy 全权 token | 同上 `:292` |
| R4 | `provides` 不参与授权，只用于能力中心展示和技能依赖解析 | 同上 `:293` |
| R5 | 首次启用的同意内容和记录按 ADR-K1-03 §1（逐工具可读/可改/可外发/风险）、§4（第一方、第三方、升级重同意），不另设；只读状态查询不需要确认 | 同上 `:295` |
| R6 | `requires.kernel` 对应 `DomainOsManifest.kernel_capabilities`（`events/models/scheduler/policy/memory/approval`），`requires.operations` 对应 CORE-2 的 `Operation` | 同上 `:156` |
| R7 | `initialize.capabilities` 是模块向内核**请求**的项，对应 `requires.kernel`，不是提供项 | 同上 `:200` |
| R8 | MCP：D7 照常适用，D9 清单由 `tools/list` 生成 | 同上 `:116` |
| R9 | r5 订正：`requires` 目前只被解析和校验，没有授权路径读取；接线不属 C1–C8 | 同上 `:52`、`:290` |

K1 侧的约束（C9 必须遵守，不另设）：许可绑定模块 ID 与来源、权限集合、签发者、签发时间、明确到期时间（ADR-K1-03 §1）；安装、更新、签名有效、首次启动本身不构成同意（§1）；变更权限范围须暂停并重新同意，无法比较按变化处理（§4）；撤销串行化、在途调用取消、fail closed（§3）；授权、重新同意、撤销、过期、政策拒绝要有审计，只记元数据，保留 180 天（§5，ADR-K1-04 §2.1、§2.5）。

### 2.2 相关但不属于 C9（不做）

| 项 | 为什么不属于 | 归属 |
|---|---|---|
| 逐工具权限摘要、工具级同意、撤销联动 | K1-6a 已落地（`agent24-store/src/module_consents.rs`，`consent_routes.rs`）；C9 只在同一张卡上**追加** requires 行，不改工具级逻辑 | ADR-K1-03 |
| 技能的 `requires`（能力 id 列表）及运行前依赖解析 | 同名不同物：技能 `requires` 是 `provides` 的能力 id，不是权限；ADR-040 D2 明写技能不持有权限、清单字段不能放行工具（`ADR-040-skill-contract-v1.md:45-47`） | ADR-040 实施（C9 之后） |
| 多提供者解析、`components.json` 默认提供者 | D9 的解析规则，与授权无关 | ADR-038 D9 |
| 组件工具暴露给 agent 的契约 | D7 末段已划给 ADR-K1-01 | ADR-K1-01 |
| D10 OS 包文件哈希完整性 | 同一章但是完整性，不是授权 | ADR-038 D10 |
| D2a 签名、D8 升级兼容用例 | 已在 C1 完成 / 另有条款；C9 只消费「版本或锁变了」这个事实 | ADR-038 |
| MCP server 的授权 | 没有锁，`requires` 恒为空；工具已是 `requires_approval=true`（`serve_steps.rs:505` 附近的注释） | 现状不变 |
| 好友机制、入站准入逻辑本身 | 2026-11-10 jason 定为之后 / X11 | ADR-041/044 |

## 3. 现状盘点

### 3.1 `requires` 的声明内容

| 组件 | 来源 | `requires` | 谁读它 |
|---|---|---|---|
| iDoris | `rust/crates/agent24-idoris/idoris.lock.json:38` | `kernel: []`，`operations: []` | 仅解析 |
| Hyphae | `rust/crates/agent24-comm/hyphae.lock.json:55` | `kernel: ["events"]`，`operations: []` | 仅解析；锁按发布字节复制，Agent24 不改内容（ADR-038 §7.4） |
| Documents | `rust/apps/agent24-documents/component.json`（编译进 agent24d） | `kernel: ["events","models"]`，`operations: []`；`documents_catalog.rs:222-232` 校验 kernel 取值与 operation 的**字符集**，不校验 operation 是否真是 `Operation` | `/components` 展示（`component_routes.rs:727`） |
| Documents（握手侧） | `rust/apps/agent24-documents/domain-os.yml:9` | `kernel_capabilities: [events, models]`（真正驱动发放的是这份，不是 `component.json`） | `domain.rs:2174` |
| 领域 OS / 进程内 | `domain-os.yml` 的 `kernel_capabilities` | 无 `operations` 字段 | `Grants::granting` |
| attach 模块（AgentEar） | 注册时提交的 manifest | 同上；`ATTACH_OOP_GRANTS` 只含 events、models（`attach_grant.rs:31`） | `attach_grant.rs:49` |
| MCP | 无 | `component_routes.rs:784` 写死空 | 无 |
| wechat-bridge | 无清单（开发工具，不进桌面包，`packages/wechat-bridge/README.md:25`） | 无 | 无 |

### 3.2 令牌与权限来源

| 持有方 | 令牌 / 权限 | 来源与范围 | 证据 |
|---|---|---|---|
| 桌面 | ProductHost | 父进程管道交付，最高权，不带 allowed_operations 限制 | `mint.rs:57-77`；CORE-2 §D6 |
| CLI 等一方外壳 | FirstPartyShell（日常档） | 登记握手，身份只认签名标识 `agent24` + team `HYAP444FN9`；操作集 = 全部路由映射的 Operation 去掉在场通道与 `InboundWrite`，再加 `ModuleRoute` | `first_party.rs:374-385`、`os-proc/src/lib.rs:234`、`first_party.rs:707` |
| Open Design 侧车 | CreativeRuntime | ProductHost 经 `POST /capabilities/creative` 签发；操作 ⊂ `creative_allowlist()`，且有路由级二次门 `creative_route_permitted` | `handlers.rs:123-160`、`operations.rs`（`creative_allowlist`）、`middleware.rs:229` |
| 进程外 OS 包 / attach 模块 | 没有 HTTP 令牌；内核回调通道上的 Grants | `Grants::granting(manifest.kernel_capabilities(), 愿意给的)`；OS 包上限 `KERNEL_OOP_GRANTS`（events、approval、memory、scheduler、models），attach 上限只有 events、models | `domain.rs:116、2174`，`attach_grant.rs:31、49` |
| 进程内 DomainModule | `KernelCtx` 上的 Grants | 上限 `KERNEL_GRANTS`（`domain.rs:75`） | 同上 |
| iDoris、Hyphae | 无 | 被 daemon 调用，环境变量里没有 daemon 令牌 | `agent24-idoris/src/supervisor.rs:881-905`、`agent24-comm/src/runner.rs:484-487` |
| wechat-bridge | **legacy 全权令牌** | `daemon.json` 里的单令牌或 `A24_TOKEN`；capabilities 模式下桥直接报错退出 | `wechat-bridge/src/config.ts:44-67`、`agent24.ts:25-27`；`middleware.rs:71`（legacy_auth 放行所有非 host_only 路由） |

### 3.3 差距（需求 vs 现状）

| 差距 | 说明 |
|---|---|
| G1 | 没有用户同意这一项：内核能力在挂载时直接发放，注册 attach 时只有「放宽隐私须确认」（`attached_routes.rs:105`），不等于逐项同意（K1-03 §1、§4 首次使用前须确认，默认安装不是授予） |
| G2 | 「愿意给的」散在三处常量（`KERNEL_GRANTS`、`KERNEL_OOP_GRANTS`、`ATTACH_OOP_GRANTS`），没有按形态统一的表 |
| G3 | `requires.operations` 无人解释：没有「组件可被授予的操作」上限，没有组件令牌，也没有校验 operation 名是 `Operation` 的规范名（`from_action` 还接受 `session/new` 之类别名，不应出现在 `requires`） |
| G4 | 升级时 `requires` 变化无处理：`module_consents` 的指纹只覆盖工具权限摘要，不含内核能力 |
| G5 | `/components` 只显示「请求了什么」，不显示「实际授予了什么、拒绝了什么」；`component.json` 与 `domain-os.yml` 是两份各自声明的 kernel，无一致性检查（未核实是否已有测试，见第 8 节） |
| G6 | 无面向 requires 的审计事件 |
| G7 | wechat-bridge 无清单、无受限令牌；`POST /api/v1/inbound/events` 在 capabilities 模式一律 404（`middleware.rs:105-112`），在 legacy 且 `A24_DEV_TOOL=1` 才挂（`inbound/routes.rs:165-177`） |
| G8 | Hyphae 的 `requires.kernel=["events"]` 没有对应的回调通道（CLI 不回调）：声明了但无处可发（见第 8 节未核实 1） |

## 4. 目标设计

### 4.1 统一模型

- **输入**：`RequiresSpec { kernel: Set<Capability>, operations: Set<Operation> }`，来源四种（锁、`component.json`、`domain-os.yml` 的 `kernel_capabilities`、新增的开发工具清单），归一后进同一个解析器。字符串到枚举：kernel 用 `agent24_domain::Capability` 的 snake_case；operations 只接受 `Operation::action()` 的**规范点号名**（`components.read`），拒绝 `from_action` 的斜杠别名。出现未知名字 = 该组件 `requires` 无效，整份拒绝，原因 `requires_invalid`（不是丢掉未知项继续，避免「声明了一个拼错的权限」静默变成零权限）。
- **解析器**（daemon 内，纯函数）：`resolve(spec, form, consent) -> Resolution { granted_kernel, granted_ops, denied: [(项, 原因码)] }`，原因码封闭集：`not_declared`、`above_ceiling`、`no_channel`、`consent_missing`、`consent_stale`、`requires_invalid`。
  - 「未声明即拒」：调用方要用的能力不在 `requires` 里，就不在 Resolution 里，没有隐式默认。
  - `above_ceiling`：声明了但超出该形态的上限。上限是显式表：内核能力按形态一张表（把 `KERNEL_GRANTS`、`KERNEL_OOP_GRANTS`、`ATTACH_OOP_GRANTS` 搬进来，值不变）；操作按「组件可授予操作」白名单，**新增 Operation 默认不在内**（对齐 CORE-2 R5、`creative_allowlist` 的做法），明确排除 admin 类：`ModelsAdmin`、`ComponentInstall`、`GrantManage`、`OverrideManage`、`ModuleAdmin`、`CapabilityMint/Revoke`、`ApprovalDecision`、`Presence*`、`ConsentRevoke`、`InboundWrite`、`Shutdown`、`HostLease`。
  - `no_channel`：该形态没有回调通道时，声明的 kernel 项不可服务（Hyphae 的 `events`）。**不是错误**：展示为「已声明、本形态无通道」，不发令牌，不阻止组件运行，不改锁（见 Q 与第 8 节）。
- **两条出口**：
  1. 内核能力：解析结果替换现有 `Grants::granting(...)` 两处调用点，其余 `build_methods_for`、`model_grant` 不动（先做行为不变替换，再加同意）。
  2. API 操作：新增 `Audience::ComponentRuntime`，claims 在现有 `CapabilityClaims` 上加 `component_id`、`requires_fingerprint`，`allowed_operations = granted_ops`。签发入口不自助：只有 daemon 在「组件被启用且 Resolution 里 granted_ops 非空」时内部签发，或由 ProductHost / 带在场证明的一方外壳经新的 mint 路由代签（用于开发工具，见 4.6）。中间件加一道和 `creative_route_permitted` 同构的路由级门 `component_route_permitted`，令牌只能走它的 granted_ops 对应的路由；`host_only_route` 对它一律拒绝。

### 4.2 与现有对象的对齐

| 对象 | 对齐方式 |
|---|---|
| `docs/capabilities/REGISTRY.md` 能力 id | **不参与授权**（R4）。`requires` 里没有能力 id；REGISTRY 只管 `provides`。解析器测试里加一条反向断言：把 `provides` 的 id 塞进 `requires.kernel` 会 `requires_invalid`，防止两套名字混用 |
| `route_policy` 的 Operation | `requires.operations` 的取值域就是 `Operation::action()`；组件令牌的路由准入 = 路由表里该 Operation 的行 ∩ `granted_ops`（`decide` 已按 Operation 查表，无需新表）。等级（日常/提升）照旧：提升级操作对组件令牌一律不放行（v1 上限白名单里不含提升级，见 Q 之外的取舍，不需要拍板） |
| `ATTACH_OOP_GRANTS` | 变成形态表里 `attach` 行的值，内容不变（events、models）。manifest 请求 memory/scheduler/approval 仍被拒，现在拒绝有了原因码 `above_ceiling` 并在 `/components` 可见 |
| ADR-K1-03 enable 授权 | 同意记录与撤销沿用其语义：绑定模块 ID、来源、权限集合、签发者、时间、**到期时间**；复用 `module_enable_epochs` 的启用纪元，纪元前进（重新启用、换包）即旧同意失效（`module_consents.rs:866-915`） |
| ADR-K1-04 审计 | 见 4.5 |
| ADR-045 路由命名 | 新路由只加两条：`GET /api/v1/components/{id}` 的 detail 增字段（不新增路径），签发走 `POST /api/v1/components/{id}/capability`（命名对齐 `/capabilities/creative` 的子资源形式，开工前按 ADR-045 复核） |

### 4.3 同意：requires 变化与升级

- **同意记录**：新表 `component_requires_consents(component_id, source, enable_epoch, requires_fingerprint, kernel_json, operations_json, granted_at, expires_at, granted_by)`。指纹 = 规范化后 `requires` 的哈希（kernel、operations 排序后），不含版本号。
- **首次启用**：启用卡（K1-03 §1 的同一张）追加一栏「它还要向本体申请」，人话列出每个 kernel 项（例如 events = 「发布它自己的活动通知」）和每个 operation（例如 inbound.ingest = 「把微信消息交给本体做入站准入」）。第一方与第三方都要点确认（K1-03 §4：默认安装不是授予）。只读状态查询（`components.read`）不需要确认（R5）。
- **升级**：版本变但指纹不变，沿用，但仍受既有「来源与制品重新核验」约束；指纹变了：先算差集，新增项不发放，减少项立即收回，组件 `/components` 显示 `reason=reauthorization_required`（D5 状态表不新增状态，复用现有 `failed`/`degraded` 之外的 `detail` 字段，具体落点第 8 节未核实 3）。用户重新确认后新指纹生效。无法比较（旧记录缺失或损坏）按变化处理（K1-03 §4）。
- **到期**：同意与令牌都有限期（K1-03 §3 不许永不过期）。同意默认随启用纪元，另设 180 天上限，与审计保留期一致，到期要求重新确认（时长待拍板，见 Q1 附带项，推荐 180 天）。
- **撤销**：禁用、卸载、撤销同意、纪元前进：同一事务里作废该组件全部 ComponentRuntime 令牌（沿用 `revoke` 的 epoch 通知）并停止该组件在途的回调；撤销写不进库则命令失败（K1-03 §3 fail closed）。

### 4.4 legacy 模式

- legacy 单令牌模式本身**不改**（别的开发工具、contract-tests 还在用，`packages/contract-tests/src/helpers.ts`）。
- 内核能力的解析在两种模式下都跑（它在挂载路径，与 HTTP 鉴权模式无关）；同意检查也一样。
- ComponentRuntime 令牌只在 capabilities 模式存在：legacy 下签发路由返回 `capability_mode_required`（同 `handlers.rs:96` 的现有做法）。
- 变化只有一处：wechat-bridge 在 C9-10 之后默认走组件令牌；legacy 分支的路由仍可达但不再是文档里的默认流程，是否最终删除见 C9-11 与 Q3。

### 4.5 审计（ADR-K1-04）

事件集：`requires_granted`、`requires_denied`（含原因码）、`requires_reauth_required`、`requires_revoked`、`requires_expired`。字段按 §2.1 白名单：组件 id、来源、指纹引用、项名（kernel 名与 operation 规范名都是封闭集，不含自由文本）、结果码、时间、actor 引用（宿主确定，不接受组件自报）。不记组件清单全文。

fail closed：发放前须能写审计，写不进则不发放，返回封闭结果码；组件令牌签发路径尤其不静默。**依赖**：K1-7 内核审计的落地状态本文未核实（第 8 节 2）；若还没有统一入口，C9-7 先用「可写审计能力」的接口桩加测试，接口以 K1-7 为准，不另造日志。

### 4.6 wechat-bridge 改用受限令牌

- **性质**：桥是开发工具，不进桌面包（`README.md:25`），不能走一方外壳登记（登记只认签名标识为 `agent24` 的进程，`first_party.rs:707`；node 进程过不了）。
- **清单**：`packages/wechat-bridge/component.json`（形态 `bridge`，不进 `/components` 的能力栏，仅供解析器与签发使用）：`requires: { kernel: [], operations: ["inbound.ingest"] }`。
- **新 Operation**：`InboundIngest`（`inbound.ingest`），加进「组件可授予操作」白名单，**只有它**；不在 FirstPartyShell 的令牌集合里（同 `InboundWrite` 的处理，`first_party.rs:380-382`），也不在 creative 白名单。
- **路由**：`POST /api/v1/inbound/events` 进路由表，`Operation::InboundIngest`，日常档；要求 `A24_DEV_TOOL=1`（现有闸，不去掉，`serve_steps.rs:757-763`）；capabilities 模式下只放行 ComponentRuntime 令牌；**来源由令牌绑定**：组件 id `wechat-bridge` 固定对应来源 `wechat`，请求体里写别的来源仍是 400（现有行为，`inbound/routes.rs:165`），令牌之外的任何 Audience（含 ProductHost）调用该路由一律拒绝，避免别的进程冒充桥。
- **签发**：`agent24 dev bridge-token`（新 CLI 子命令，走 FirstPartyShell + 在场确认，`CapabilityMint` 同级的提升档），daemon 把令牌写入 `~/.agent24/components/wechat-bridge/token`（0600，目录 0700，只写一次，不进日志、argv、env）；桥读这个文件，缺失或过期时报错退出并提示重新签发，不降级去读 `daemon.json`。令牌期限见 Q3。
- **种子名单**：capabilities 模式下 `A24_INBOUND_DEV_ALLOWLIST` 仍然不读（现有约束，`INBOUND-ADMISSION-PLAN.md:173`），名单只能由 ProductHost 经 2f 写入；这意味着 capabilities 模式下开发桥要先在桌面里放行 uid，行为变化要写进 README。

## 5. 分片计划

规模口径：S <100、M 100–300 行非测试代码。派给：Grok 写码、Opus 验收（按既有分工）；评审走 PR-Daemon；每片按 PR 前流程跑 Codex 挑战 / pre-pr-check。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（可测） | 风险 | 派给 |
|---|---|---|---|---|---|---|---|
| C9-0 | `RequiresSpec` 归一与校验：operation 只认规范名，kernel 只认 `Capability`；「组件可授予操作」白名单与形态内核上限表（常量搬家，值不变）；漂移测试遍历全部锁、`component.json`、`domain-os.yml` | `agent24d/src/capabilities/requires.rs`（新）、`operations.rs`、`component_routes/documents_catalog.rs`；测试 | M | 无 | 正对照：现有三份真实声明全部通过。反例：未知 operation、斜杠别名、`provides` 的 id 放进 kernel，全部 `requires_invalid`。**变异**：把白名单改成「全部 Operation」，断言 admin 类被拒的测试变红；新增一个 Operation 不改白名单，断言它默认被拒 | 搬常量时改了值 | Grok |
| C9-1 | 解析器 `resolve`：封闭原因码；`no_channel` 规则；纯函数 | `capabilities/requires.rs` | M | C9-0 | 表驱动：声明∩上限∩同意 8 个格子（缺任一格为空即空）。变异：删掉 `∩ 上限` 一步，超限用例变红；删掉未声明检查，「调用方多要一项」用例变红 | 形态枚举与现有 mount 路径不一致 | Grok |
| C9-2 | 内核出口接线：两处 `Grants::granting` 改走解析器（同意先用「全部放行」适配器，行为不变） | `attach_grant.rs`、`domain.rs`；测试 | S | C9-1 | 既有 `a3_2b_attach_blackbox`、`daemon_modules`、`me4_*` 全绿；新增：attach manifest 请求 memory，原先被静默丢弃，现在 Resolution 里有 `above_ceiling`，授予集合不变。变异：让适配器返回空，已有模块的 events/models 用例变红 | 行为意外变化 | Grok |
| C9-3 | `/components` 展示：`detail.requires` 改为 `{declared, granted, denied[{item, code}]}`，替换 `component_routes.rs:727、784` 的手写；`no_channel` 对 Hyphae 可见 | `component_routes.rs`、OpenAPI、`packages/api-client`、contract-tests | M + 生成物 | C9-2 | 契约测试：Hyphae `events` 显示 `no_channel`；Documents 显示 granted；redacted 视图（非宿主）只给声明、不给 denied 细节（沿用 audience 脱敏，`component_routes.rs:852` 一带）。变异：脱敏分支删掉，测试变红 | OpenAPI 生成物体量大；API 增字段需版本说明 | Sonnet |
| C9-4 | 迁移：`component_requires_consents` 表 + 存储 API（grant、lookup、revoke、expire）+ 既有安装回填策略（Q1 定） | `agent24-store/migrations/00NN_*.sql`（编号以开工 main 为准）、`store/src/component_requires_consents.rs` | M，迁移单独成片 | 无（可与 C9-0 并行） | 迁移幂等；空库、已有 module_consents 的库、被篡改的指纹行三种起点。变异：去掉到期过滤，过期同意仍被当有效，测试变红 | 迁移不可逆；编号冲突 | Grok |
| C9-5 | 同意接入解析器 + 升级重新同意：指纹比较、差集、新增不发放、减少即收回；启用卡追加 requires 栏（人话三语） | `consent_routes.rs`、`capabilities/requires.rs`、`domain.rs` 启用路径；前端文案 | M | C9-1、C9-4 | 首启无同意：kernel 全 `consent_missing`；同意后 granted。升级：指纹不变沿用；加一个 kernel 项，仅新增项 `consent_stale`；减少项立即消失。变异：把指纹改成含版本号，「版本变但权限没变」用例变红；去掉差集，新增项被带出，用例变红 | 与 K1-6a 启用纪元交互；桌面三语文案 | Grok + Sonnet（文案） |
| C9-6 | `Audience::ComponentRuntime`：claims 加 `component_id`、`requires_fingerprint`；`mint_component`（仅 daemon 内部与 ProductHost 代签）；撤销随纪元；TTL | `capabilities/types.rs、mint.rs、validate.rs、revoke.rs` | M | C9-1 | 签发的 ops = granted_ops，不含未授予项；超 TTL 拒绝；纪元前进后旧令牌 401；**变异**：让 mint 忽略白名单，断言 `ModelsAdmin` 被拒的测试变红 | claims 结构被多处 match，编译面广 | Grok |
| C9-7 | 路由级门与审计：`component_route_permitted`；`host_only_route` 对组件令牌恒拒；审计事件五类，审计写不进则不发放 | `capabilities/middleware.rs、route_policy.rs`；审计入口（K1-7） | M | C9-6，K1-7 审计入口（未核实） | 组件令牌访问其 ops 之外的路由 403；访问 host_only 路由 403；审计不可写时签发失败并有封闭结果码。变异：删掉路由门，越权用例变红 | K1-7 状态不明可能阻塞（第 8 节 2） | Grok |
| C9-8 | wechat-bridge 守护侧：`Operation::InboundIngest`、路由表一行、来源由令牌绑定、仅 ComponentRuntime 可调；不在 FirstPartyShell、creative 集合中 | `operations.rs、route_policy.rs、middleware.rs:105-112、inbound/routes.rs`；OpenAPI | M | C9-6、C9-7 | 持桥令牌 POST 微信事件：恰好 1 个 run，tag Unknown（沿用 X11-2g 用例）；同令牌 body 写 `source:"hyphae"`：400；ProductHost、FirstPartyShell、legacy 令牌（在 capabilities 模式）调该路由：拒绝；无 `A24_DEV_TOOL=1`：404。变异：去掉「仅 ComponentRuntime」，ProductHost 越权用例变红 | 路由表与 contract-tests 生成物 | Grok |
| C9-9 | 签发与桥改造：`agent24 dev bridge-token`（在场确认）、令牌文件 0600、桥改读令牌文件；桥清单 `component.json` | `agent24-cli/src/`、`packages/wechat-bridge/src/{config,agent24,main}.ts` 及测试、README | M | C9-8、Q3 | 无令牌文件：桥拒绝启动，**不读 daemon.json**（变异：回退到读 daemon.json，测试变红）；文件权限不是 0600：拒绝；令牌过期：桥退出并提示；桥源码里没有 `daemon.json` 的 bearer 读取；端到端：capabilities 模式 + 开发标记 + 桌面放行的 uid，一条微信消息 1 个 run | 桥现有测试大改；在场确认需要桌面在线 | Sonnet + Opus 验收 |
| C9-10 | 收口：ADR-038 D7 由「待接线」改为已接线并写明解析顺序；§4 C9 行标完成；`INBOUND-ADMISSION-PLAN.md` 第 8 条订正；wechat-bridge README 流程改写 | `docs/adr/ADR-038-…`、`docs/tracking/…`、README | S | C9-9 | 文档与代码一致性核对（每条 file:line 重核）；`pre-pr-check` 通过 | 文档与实现漂移 | Sonnet |
| C9-11 | 去桥的 legacy 分支（Q3 选定后）：`post_wechat_event` 里 `auth_mode != Legacy` 的判定改为只认组件令牌路径；legacy 单令牌模式本身保留 | `inbound/routes.rs`、`serve_steps.rs:757-763`、`wechat-bridge` | S | C9-10 稳定一版 | legacy 令牌调该路由：拒绝；legacy 模式的其他路由不变（contract-tests 全绿） | 影响开发者本机习惯 | Grok |

依赖顺序：C9-0 → C9-1 → {C9-2, C9-4 并行} → C9-3、C9-5、C9-6 → C9-7 → C9-8 → C9-9 → C9-10 → C9-11。C9-4 的迁移单独成 PR；按「多片但以少为好」，C9-2 与 C9-3 可以合并成一个 PR 送审（均为展示与行为不变替换），其余各自独立。

## 6. 明确不做

- 不改 legacy 单令牌模式本身，不改 `daemon.json` 的发布方式（只去掉桥对它的依赖）。
- 不改工具级同意与撤销（K1-6a），不改 ADR-K1-01 的工具契约。
- 不让 iDoris、Hyphae 拿 daemon 令牌；不改 Hyphae 锁（锁按发布字节复制，要改请 Hyphae 仓库发新版，见第 8 节 1）。
- 不实现技能加载器、技能 `requires` 解析（ADR-040，C9 之后）。
- 不给 MCP 加 requires（无锁，现状已是逐次审批）。
- 不新增 D5 状态，不新增组件形态的下载安装能力。
- 不让 `requires` 承载能力 id，不让 `provides` 参与授权。
- 不做组织/管理员策略层（K1-03 §2 的组织限制另有归属）。
- 不把桥做成产品组件：它仍是开发工具，不进桌面包。
- 提升级（Elevated）操作在 v1 不授予任何组件令牌。

## 7. 需要 jason 拍板

**Q1　已安装的老用户，升级到 C9 后要不要重新点一次同意？**
选 (a) 静默沿用：升级时把「现在已经在发放的内核能力」自动记成同意，来源标「沿用」，并写一条审计；选 (b) 弹一次确认，不点就暂停模块的 events/models。(b) 更符合「默认安装不是授予」，但 Documents 这类已在用的功能会在升级后突然要求确认。推荐 (a)：沿用的只是此刻已经在发放的集合，新增项一律要点；另外把同意的有效期定为 180 天，与审计保留期一致。

**Q2　内核能力的同意是并进现有启用卡，还是单独一张卡？**
并进：启用卡多一栏「它还要向本体申请」，用户一次看完工具权限和内核权限，点一次。单独：另开一张卡，流程更清楚，但用户要点两次，且要设计新页面。推荐并进，少一套界面，也符合 ADR-038 D7「同意内容按 K1-03，不另设」。

**Q3　开发态微信桥的令牌怎么给、给多久？**
选 (a) `agent24 dev bridge-token` 在桌面确认后签发，写 0600 文件，8 小时有效、不续期，过期桥提示重新签发；选 (b) 1 小时有效并支持续期，桥能长挂，但要多做续期接口（多一片）；选 (c) 继续用 legacy。推荐 (a)：桥是开发工具，8 小时够一个工作日，不做续期少一片；到期要重签是有意的。另外同时确认：C9-11 最终删掉桥对 legacy 令牌的支持，推荐删（你 11-12 已定 C9 是去 legacy 的前提）。

## 8. 未核实项

1. **Hyphae 的 `requires.kernel=["events"]` 想表达什么**：Hyphae 是 daemon 调用的 CLI，没有回调通道；`events` 是 Hyphae 期望 daemon 替它发事件，还是遗留声明，未向 Hyphae 侧确认。本方案按 `no_channel` 只展示不报错；若要清掉，需 Hyphae 发新锁。
2. **K1-7 内核审计的落地状态**：本文只读了 ADR-K1-04 和 `audit_log` 表的引用（`allocation_reservation.rs`），没找到可直接写「授权类事件」的统一入口，C9-7 可能因此被阻塞或需要先做桩。
3. **「需重新同意」在状态面的落点**：未确认现有 D5 状态加 `detail.reason` 是否足够，或要新增原因码到 `component_routes.rs` 的映射表与 OpenAPI 枚举。
4. **`component.json` 与 `domain-os.yml` 的 `kernel` 一致性**：两处现在值相同（events、models），没查是否已有测试把它们钉在一起（`registered_providers_match_registry_md` 只对账提供方）。
5. **attach 模块 manifest 是否能带 operations**：`DomainOsManifest` 没有 operations 字段，attach 与 OS 包的 `requires.operations` 目前恒空，本方案没给它们加字段；是否有模块要用 operations，未调查。
6. **非 Rust 子进程接收令牌的方式**：4.6 选了 0600 文件；若将来有正式组件需要令牌，用管道（`host_bootstrap` 那类机制）更好，但 Node 与 Go 子进程能否接收未核实，本期只服务桥。
7. **`POST /components/{id}/capability` 的命名**：未按 ADR-045 逐条核对，开工前复核。
8. **K1-6a 同意是否已含内核能力**：只 grep 了 `attach_grant.rs`、`domain.rs`、`consent_routes.rs`、`module_tool_runtime.rs`，没有发现内核能力与同意挂钩，但没有全仓库核查。
9. **同意记录回填的数据量与 Q1 (a) 的实现**：没读 `module_consents.rs` 的迁移与回填先例，C9-4 开工前要读。
10. **桥在 capabilities 模式下的名单行为变化**：名单种子在该模式不生效是现有约束，桌面放行 uid 的体验是否可接受，没有向 jason 确认过。
