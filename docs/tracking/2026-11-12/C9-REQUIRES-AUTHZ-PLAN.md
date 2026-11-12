# ADR-038 C9：`requires` 接入授权——实施方案（r2）

> 日期：2026-11-12。状态：r2，按评审（Opus「小改后开工」、Codex「rewrite」）与 jason 2026-11-12 三条拍板重写，已无开放问题。每期先方案后写。
> 来源：jason 2026-11-12 拍板，X11 入站准入收尾后主线是把组件规范化做完，先 C9，再 ADR-040 技能加载器 v1。C9 也是去掉 wechat-bridge 开发态 legacy 全权令牌的前提（`docs/tracking/2026-11-10/INBOUND-ADMISSION-PLAN.md:245`）。
> 只写方案，不含代码。行号取自 main `bb03081e8`。

## 1. 一页结论

1. 现在 `requires` 只被解析，没有任何授权路径读它（`rust/crates/agent24-components/src/lock.rs:38`）；而且只有结构校验（kernel 取值、operation 的字符集，`documents_catalog.rs:222-232`），没有权限名语义校验。内核能力其实已在发放：三处 `Grants::granting`（`attach_grant.rs:49`、`domain.rs:1716`、`domain.rs:2174`）按「模块请求 ∩ 宿主上限」发放。
2. jason 拍板：第一方签名、随桌面装的模块，**启用即同意**，只在能力中心和 `/components` 展示拿到了哪些内核能力。第三方同意卡以后再做，本期不做，所以不做撤销即时生效、在途复验、同意随纪元作废那一套。
3. 本期边界：第三方模块在同意卡落地前，内核能力维持现状（请求 ∩ 宿主上限），不新增也不收紧，留 FU。
4. iDoris、Hyphae 从不持有 daemon 令牌，所有真实组件的 `requires.operations` 都是空的，所以不做「daemon 在组件启用时内部签发组件令牌」；唯一需要受限令牌的是开发态 wechat-bridge。
5. 做法：一个解析器统一读取 `requires`，三处 `granting` 改走它（行为不变），`/components` 展示「实际授予」；新增 `Audience::ComponentRuntime`，本期只有写死的 dev-bridge 一档（`inbound.ingest`、来源 `wechat`），经桌面确认后由 `agent24 dev bridge-token` 签发，8 小时、不续期。
6. 同一验收边界里删掉桥对 legacy 全权令牌的支持：客户端停止回退，服务端拒绝 legacy 入站。授权的签发与撤销进现有类型化审计，写不进就不签发。
7. 压成 4 片（A–D），桥线先走；无迁移片。

## 2. C9 的准确定义

### 2.1 ADR-038 原文要求（逐条）与本期处理

| # | 要求 | 出处 | 本期处理 |
|---|---|---|---|
| R1 | C9 是「D7 `requires` 授权接线：令牌按 `requires` ∩ grants ∩ 同意发放，组件不再持有 legacy 全权 token」 | `docs/adr/ADR-038-component-contract-v1.md:370` | 内核能力：解析器接线。令牌：只有桥 |
| R2 | 能调用的宿主接口 = `requires` ∩ 宿主愿意给的（`Grants::granting`）∩ 用户按 ADR-K1-03 确认的，三者缺一即拒 | 同上 `:292` | 第一方随包模块：「启用」即第三项（jason 拍板 1，见 §2.3）；第三方：本期不改（FU） |
| R3 | 令牌由 CORE-2 按组件发放，组件不得持有 legacy 全权 token | 同上 `:292` | iDoris、Hyphae 已无令牌；桥改受限令牌 |
| R4 | `provides` 不参与授权 | 同上 `:293` | 解析器测试反向断言 |
| R5 | 首次启用同意按 ADR-K1-03 §1、§4，不另设；只读状态查询不需确认 | 同上 `:295` | 需订正，见 §2.3 |
| R6 | `requires.kernel` ↔ `kernel_capabilities`；`requires.operations` ↔ CORE-2 `Operation` | 同上 `:156` | 解析器按此归一 |
| R7 | `initialize.capabilities` 是请求项，对应 `requires.kernel` | 同上 `:200` | 不变 |
| R8 | MCP：D7 照常，清单由 `tools/list` 生成 | 同上 `:116` | `requires` 恒空，现状不变 |
| R9 | `requires` 目前只被解析和校验，没有授权路径 | 同上 `:52`、`:290` | 本期收口，由 D 片改文 |

### 2.2 相关但不属于 C9（不做）

| 项 | 归属 |
|---|---|
| 逐工具权限摘要、工具级同意与撤销联动（K1-6a 已落地，`agent24-store/src/module_consents.rs`） | ADR-K1-03 |
| 技能的 `requires`（能力 id 列表）：同名不同物，ADR-040 D2 明写技能不持有权限（`ADR-040-skill-contract-v1.md:45-47`） | ADR-040 实施（C9 之后） |
| 多提供者解析、D10 OS 包完整性、D2a 签名、D8 升级兼容 | ADR-038 其他条款 |
| 第三方模块的同意卡、同意记录、指纹比较、撤销与在途复验 | FU（§5 设计预留，本期不实现） |
| MCP 授权、好友机制、入站准入逻辑本身 | 现状 / ADR-041、044 |

### 2.3 订正一句话（写进 ADR-038 D7 与 ADR-K1-03 §4，由 D 片落笔）

> **第一方签名、随桌面安装的模块，其内核能力（`requires.kernel` ∩ 宿主上限）以「启用」为同意，不另设确认；宿主只在能力中心与 `GET /api/v1/components` 展示实际授予了哪些。第三方与手动安装的模块不适用本句，仍按 §1、§4 逐项确认。**

「第一方」的判定用生产代码签名白名单（Team ID `HYAP444FN9`，标识 `ai.auraai.agent24`、`agent24`、`agent24d`，`rust/apps/agent24d/src/enroll_endpoint.rs:21-23`；校验在 `:51-55`）。注意：这份白名单是用来验**登记进程**的，不是为 OS 包设计的，C9-A 开工前先确认如何用于模块（见第 8 节 1）。

## 3. 现状盘点

### 3.1 `requires` 声明

| 组件 | 来源 | `requires` | 谁读 |
|---|---|---|---|
| iDoris | `rust/crates/agent24-idoris/idoris.lock.json:38` | kernel 空，operations 空 | 仅解析 |
| Hyphae | `rust/crates/agent24-comm/hyphae.lock.json:55` | kernel `["events"]`，operations 空；锁按发布字节复制（ADR-038 §7.4） | 仅解析 |
| Documents | `rust/apps/agent24-documents/component.json` | kernel `["events","models"]`，operations 空 | `/components` 展示（`component_routes.rs:727`） |
| Documents（握手侧） | `rust/apps/agent24-documents/domain-os.yml:9` | `kernel_capabilities: [events, models]`，真正驱动发放 | `domain.rs:2174` |
| 进程内 DomainModule | `domain-os.yml` | 同上 | `domain.rs:1716`（上限 `KERNEL_GRANTS`，`domain.rs:75`） |
| attach 模块（AgentEar 等） | 注册时的 manifest | 同上，上限 `ATTACH_OOP_GRANTS`（`attach_grant.rs:31`，只含 events、models） | `attach_grant.rs:49` |
| MCP | 无 | `component_routes.rs:784` 写死空 | 无 |
| wechat-bridge | 无清单，开发工具，不进桌面包（`packages/wechat-bridge/README.md:25`） | 无 | 无 |

### 3.2 令牌与权限来源

| 持有方 | 令牌 / 权限 | 证据 |
|---|---|---|
| 桌面 | ProductHost（父管道交付） | `mint.rs:57-77`；CORE-2 §D6 |
| CLI 等一方外壳 | FirstPartyShell，操作集 = 全部路由映射的 Operation 去掉在场通道与 `InboundWrite`，加 `ModuleRoute` | `first_party.rs:374-385` |
| Open Design 侧车 | CreativeRuntime，⊂ `creative_allowlist()`，另有路由门 | `handlers.rs:123-160`、`route_policy.rs:217-224`、`middleware.rs:229` |
| OS 包 / attach / 进程内模块 | 无 HTTP 令牌，内核回调通道上的 Grants：OOP 上限 `KERNEL_OOP_GRANTS`（`domain.rs:116`），进程内 `KERNEL_GRANTS` | 上表三处 `granting` |
| iDoris、Hyphae | 无，环境变量里没有 daemon 令牌 | `agent24-idoris/src/supervisor.rs:881-905`、`agent24-comm/src/runner.rs:484-487` |
| wechat-bridge | **legacy 全权令牌**（`daemon.json` 或 `A24_TOKEN`），capabilities 模式下桥直接报错 | `wechat-bridge/src/config.ts:44-67`、`agent24.ts:25-27`；`middleware.rs:71` |

### 3.3 差距

| 差距 | 本期处理 |
|---|---|
| G1 内核能力无用户同意 | 第一方：订正为启用即同意（jason 1）；第三方：FU，不新增不收紧 |
| G2 「宿主上限」散在三处常量 | A 片：按形态一张表，值不变 |
| G3 `requires.operations` 无人解释；校验只有结构（`documents_catalog.rs:222-232`），不校验 operation 是 `Operation` 规范名 | A 片加语义校验 |
| G4 `/components` 只显示请求项（`component_routes.rs:727`），不显示实际授予 | D 片展示「实际授予」 |
| G5 授权类决策无审计 | B 片，复用现有类型化审计 |
| G6 桥：legacy 全权令牌；`dev_wechat_events` 与 legacy 绑死（`serve_steps.rs:1858-1869`，`inbound/routes.rs:175`）；capabilities 模式路由一律 404（`middleware.rs:105-112`） | B、C 片 |
| G7 Hyphae `requires.kernel=["events"]` 无回调通道 | 只展示 `no_channel`，不报错（见第 8 节） |

## 4. 目标设计（本期实现）

### 4.1 解析器（A 片）

- 输入归一：锁、`component.json`、`domain-os.yml` 的 `kernel_capabilities` 都变成 `RequiresSpec { kernel, operations }`。kernel 按 `agent24_domain::Capability` 的 snake_case；operations 只认 `Operation::action()` 的规范点号名，拒绝 `from_action` 的斜杠别名。
- 输出 `Resolution { granted_kernel, denied: [(项, 原因码)] }`，原因码封闭：`not_declared`、`above_ceiling`、`no_channel`、`requires_invalid`。
- **`requires_invalid` = 零授予、在 `/components` 可见、不阻止组件运行**（评审 L4）。
- 形态上限表：把 `KERNEL_GRANTS`、`KERNEL_OOP_GRANTS`、`ATTACH_OOP_GRANTS` 搬进一处，值不变。三处 `granting` 调用改走解析器，**授予集合与现在逐项相同**（行为不变）。attach 请求 memory、scheduler、approval 时，授予集合仍不含，但 Resolution 里有 `above_ceiling`，可见。
- 与 REGISTRY 能力 id 不混用：`requires` 里放能力 id 视为 `requires_invalid`（R4）。漂移测试已存在（`registry_lists_every_capability_id_the_components_emit`，`docs/capabilities/REGISTRY.md` 头部），A 片扩展它覆盖 `requires`，不新建。
- 第一方判定：A 片只记录来源标签（第一方 / 非第一方）用于展示；**不据此改变任何授予**。

### 4.2 `ComponentRuntime` 令牌，只有 dev-bridge 一档（B 片）

- 新增 `Audience::ComponentRuntime`，claims 加 `component_id`。**桥的授权档在 daemon 里写死**：`wechat-bridge → { Operation::InboundIngest }`，来源固定 `wechat`；不读桥自报的 `component.json`（评审 M2）。不做通用的「按 requires 内部签发组件令牌」。
- 新 Operation `InboundIngest`（`inbound.ingest`，日常档）：不在 FirstPartyShell 集合里（同 `InboundWrite` 的写法，`first_party.rs:380-382`），不在 creative 白名单。
- **路由门**：组件令牌只允许显式列出的 method+path，目前只有 `POST /api/v1/inbound/events`（参照 `creative_route_permitted`，`route_policy.rs:217-224`）；`ModuleRoute` 默认排除；`host_only_route` 对它一律拒绝。变异测试：给另一条路由映射同一个 `InboundIngest`，它**不得**自动对桥令牌开放（评审 H5）。
- **handler 三重校验**：令牌 audience == ComponentRuntime、`component_id == "wechat-bridge"`、持有 `InboundIngest`，缺一拒绝；ProductHost、FirstPartyShell、legacy 令牌调该路由一律拒绝。请求体 `source` 写别的仍是 400（现有，`inbound/routes.rs:165`）。
- **开发闸与 legacy 名单种子分离**：现在 `dev_wechat_seed` 把「路由启用」和「legacy 名单种子导入」绑在 legacy 上（`serve_steps.rs:1858-1869`）。改为：`A24_DEV_TOOL=1` 即启用路由，capabilities 模式也可开；种子导入**仍只在 legacy + 开发闸**下生效（capabilities 模式下不读，名单只能由 ProductHost 经 2f 写入，`INBOUND-ADMISSION-PLAN.md:173`）。路由在 capabilities 模式下只对 ComponentRuntime 令牌开放。
- **签发**：路由 `POST /api/v1/capabilities/bridge`（按 ADR-045 命名，与 `/capabilities/creative` 同级，`route_policy.rs:80`），`Operation::CapabilityMint` 提升档。提升授权要有**精确目标与宿主生成的摘要**：扩展 `resolve_target` 的 `supported` 名单加入该路由（`first_party.rs:498-520`），并给它在 `target_summary` 里生成摘要，如「签发 8 小时微信桥开发令牌，只能写入入站事件」（`first_party.rs:574-576`，沿用「`{identifier} (pid {pid}) wants to: {details}`」格式）。
- **令牌期限**：8 小时，不续期，过期即 401，桥提示重新签发。令牌绑定当前 daemon 代次，daemon 重启即失效（沿用现有 `daemon_generation` 检查）。

### 4.3 审计（B 片）

- 复用现有类型化审计，不新造日志：`module_audit_event.rs:479-495` 的 `append_module_tool_audit_event` 与 `Store::append_audit`（`audit.rs:363`）。
- **只审计决策与变化**：签发、撤销、拒绝（原因为封闭码）。挂载时的解析结果只展示，不审计。字段只用封闭集与引用，不记令牌、不记请求体。
- **签发前审计须可写**：审计写不进，不签发，返回封闭结果码。**撤销写不进，不得让旧令牌照常可用**：撤销先在内存撤销表标记，再写审计，写审计失败时令牌仍保持失效并返回失败，由重试补记（Codex H4）。
- 已知问题：现有类型化审计是为「模块工具调用」设计的（要求 run、tool_call 关联字段），能否直接承载「令牌签发」这类事件，还是要加变体，未核实（第 8 节 2）；以「加一个变体、不放开自由 JSON」为原则。

### 4.4 桥纵向闭环（C 片）

- **CLI**：`agent24 dev bridge-token`：登记 → 请求提升 → 桌面显示摘要并确认 → 签发 → 令牌写入状态目录下 `dev/wechat-bridge.token`（0600，目录 0700，只写一次，不进日志、argv、env）。**不放** `~/.agent24/components/`（那是托管根，评审 L2）。
- **桥**：只读该文件。无文件、权限不是 0600、过期：拒绝启动或退出并提示重新签发；**不回退读 `daemon.json`、不读 `A24_TOKEN`**，源码里去掉 legacy bearer 的读取（`config.ts:44-67`、`agent24.ts:25-27`）。
- **iLink 游标**：桥在 `POST` 入站成功**之前**不推进 iLink 游标。令牌 8 小时到期或 daemon 重启使 POST 失败时，消息不丢，重签后重投（daemon 侧 `wechat:<message_id>` 去重，`INBOUND-ADMISSION-PLAN.md:187`）。
- **去 legacy（同一验收边界，同一 PR）**：客户端停止回退，同时服务端拒绝 legacy 令牌的入站（`inbound/routes.rs:175` 的 legacy 分支去掉）。legacy 单令牌**模式本身保留**（别的开发工具、contract-tests 在用）。
- README：流程改为 capabilities 模式 + 桌面放行 uid + `bridge-token`；写明「capabilities 模式下种子环境变量不生效」。

### 4.5 `/components` 展示（D 片）

- 展示**实际授予**（取自挂载后真正的 `Grants`，即 `MountReport.granted`），旁边附 `declared` 与 `denied[{item, code}]`；不是把解析结果直接当授予（Codex M3）。
- 没有组件详情路由，扩展集合响应 `GET /api/v1/components`（Codex L2）。非宿主视图沿用 audience 脱敏，不给 `denied` 细节（`component_routes.rs:852` 一带）。
- Hyphae 显示 `no_channel`。能力中心展示文案三语。

### 4.6 legacy 模式

legacy 不动。内核能力解析在两种模式下都跑。ComponentRuntime 令牌只在 capabilities 模式存在，legacy 下签发路由返回 `capability_mode_required`（同 `handlers.rs:96`）。

## 5. 设计预留（本期只写不做）

需要同意的模块（以后第三方、手动安装）：
- 权限指纹（规范化后的 `requires`，不含版本号）没变就沿用已有同意；**新增能力才重新问**，减少项直接收回。无法比较按变化处理（K1-03 §4）。
- 同意有效期复用现有 `GrantSettings.max_days`（`consent_routes.rs:434`，上限 `MAX_GRANT_DAYS_CAP = 90`，`:430`），不另立 180 天。
- 撤销即时生效、在途请求复验、同意随启用纪元作废，到那时再做，可复用 K1-6a 的 `module_enable_epochs` 机制（`module_consents.rs:866-915`）。
- 是否需要存储迁移，到那时再定。

本期边界：第三方模块在同意卡落地前，内核能力**维持现状**（请求 ∩ 宿主上限），不新增也不收紧，留 FU。

## 6. 分片计划

派给：Grok 写码、Opus 验收；评审走 PR-Daemon；每片按 PR 前流程跑 Codex 挑战 / `pre-pr-check`。无迁移片。

| 片 | 目标 | 涉及文件 | 规模 | 依赖 | 验收（可测，含正对照与变异） | 风险 | 派给 |
|---|---|---|---|---|---|---|---|
| A 声明与解析 | `RequiresSpec` 归一 + 语义校验；形态上限表（三常量搬家）；解析器；**三处** `granting`（`attach_grant.rs:49`、`domain.rs:1716`、`domain.rs:2174`）改走解析器，行为不变；扩展 REGISTRY 漂移测试覆盖 `requires`；记录第一方来源标签 | `agent24d/src/capabilities/requires.rs`（新）、`attach_grant.rs`、`domain.rs`、`component_routes/documents_catalog.rs`；测试 | M | 无 | 正对照：三处现有授予集合逐项不变（`a3_2b_attach_blackbox`、`daemon_modules`、`me4_*` 全绿）。反例：未知 operation、斜杠别名、`provides` 的 id 放进 kernel：`requires_invalid` 且零授予、组件仍运行。变异：把上限表改成「全放行」，attach 请求 memory 的用例变红；让解析器多授予一项，授予集合对比测试变红；删掉某个调用点的改造，「三处都经解析器」的断言（调用点计数或注入探针）变红 | 搬常量时改了值；三处语义略有差异（in-process 与 OOP 上限不同） | Grok |
| B ComponentRuntime + 路由门 + 签发 + 审计 | `Audience::ComponentRuntime`（只有写死的 dev-bridge 档）；`Operation::InboundIngest`；路由显式白名单门；`POST /api/v1/capabilities/bridge`（提升档，精确目标与宿主摘要）；handler 三重校验；开发闸与 legacy 种子分离；签发/撤销/拒绝审计，签发写不进不签、撤销写不进旧令牌仍失效 | `capabilities/{types,mint,validate,revoke,operations,route_policy,middleware,first_party}.rs`、`inbound/routes.rs`、`serve_steps.rs:1858-1869`、审计变体（`module_audit_event.rs`）；OpenAPI | M，偏上；若超 300 行拆出「审计」为 B2 | A | 正对照：持桥令牌 POST 微信事件，1 个 run、tag Unknown。越权：桥令牌访问其他路由 403；ProductHost、FirstPartyShell、legacy 令牌调入站路由拒绝；`component_id` 不是 wechat-bridge 拒绝；无开发闸 404；body `source:"hyphae"` 400；capabilities 模式下种子变量不生效。变异：新映射一条复用 `InboundIngest` 的路由，桥令牌不得通过（去掉显式白名单则变红）；把 handler 三项校验各删一项，对应用例变红；审计桩返回写失败，签发失败（变异：忽略写失败则变红）；撤销时审计写失败，旧令牌仍 401（变异：先写审计再撤销，变红） | 令牌 claims 被多处 match；类型化审计未必能直接承载（第 8 节 2） | Grok |
| C bridge-token CLI + 桥 + 去 legacy | `agent24 dev bridge-token`；令牌文件（状态目录 `dev/`，0600）；桥只读该文件；POST 成功前不推进 iLink 游标；**同 PR** 删桥的 legacy 回退（客户端）与服务端 legacy 入站；README | `agent24-cli/src/`、`packages/wechat-bridge/src/{config,agent24,main,bridge}.ts` 及测试、`inbound/routes.rs`、README | M | B | **真实链路验收**：登记 → 桌面确认 → 签发 → 入站，一条微信消息 1 个 run（用真 daemon + 真 enroll，不用手造令牌；沿用 `enroll_real_daemon` 一类夹具）。反例：无文件 / 权限 0644 / 过期，桥不启动或退出且不读 `daemon.json`；legacy 令牌直接 POST 入站被服务端拒绝；POST 失败（令牌过期），游标不前进，重签后同条消息被投递且不重复起 run。变异：桥回退读 `daemon.json`，测试变红；服务端放回 legacy 分支，变红；游标提前推进，丢消息用例变红 | 桥现有测试大改；需桌面在线确认；行为变化（微信里原本就不能审批，不变） | Sonnet + Opus 验收 |
| D 展示 + 订正 + 收口 | `/components` 展示实际授予 / declared / denied（`no_channel` 对 Hyphae 可见）；ADR-038 D7（待接线→已接线）与 ADR-K1-03 §4 加 §2.3 那句订正；§4 C9 行标完成；`INBOUND-ADMISSION-PLAN.md` 第 8 条订正；`MILESTONES-AND-TASKS.md` C9 行 | `component_routes.rs`、OpenAPI、`packages/api-client`、contract-tests、`docs/adr/ADR-038-…`、`ADR-K1-03-…`、`docs/tracking/…` | M + 生成物 | A（展示）；B、C（订正与标完成） | 展示的是 `MountReport.granted` 而非解析结果：构造「解析器说授予、挂载实际没给」（如模型回调缺失导致 `model_grant` 为空）的用例，展示以挂载实际为准（变异：改成读解析结果，变红）；非宿主视图无 `denied` 细节；文档每条行号重核；`pre-pr-check` 通过 | OpenAPI 生成物体量大；文档与实现漂移 | Sonnet |

依赖：A → B → C，D 的展示部分可在 A 之后并行，订正与收口等 C。桥线（B、C）先走，按「多片但以少为好」，A 可与 D 的展示合成一个 PR。

## 7. 已拍板（jason，2026-11-12）

1. 第一方签名、随桌面装的模块（Documents、AgentEar 等，以生产签名白名单判定）启用即同意，只在能力中心 / `/components` 展示所拿内核能力；第三方同意卡以后再做，本期不做。ADR-038 D7 与 ADR-K1-03 订正一句话（§2.3）。因此不做撤销即时生效、在途复验、同意随纪元作废；第三方在同意卡落地前内核能力维持现状，不新增不收紧，留 FU。
2. 需要同意的模块（以后第三方）：权限指纹没变就沿用，新增能力才重新问；有效期复用现有 `settings.max_days`，不另立 180 天。本期只写设计（§5），不实现。
3. 开发态微信桥：`agent24 dev bridge-token` 经桌面确认签发，只能调入站一个接口，8 小时不续期；同时删掉桥对 legacy 全权令牌的支持，客户端停止回退与服务端拒绝 legacy 入站在同一验收边界（C 片）完成。

本版没有新的真取舍。

## 8. 未核实项

1. **「第一方模块」的判定怎么落到 OS 包 / attach 模块**：`enroll_endpoint.rs:21-23、51-55` 的白名单验的是登记进程的签名，OS 包和 attach 模块是否带可对照的签名身份、以及现有 `ConsentSource::FirstParty`（`module_consents.rs:48`）的判定依据，没读。A 片只记录来源标签，不据此改授予；若判定不清，本期展示里标「未知」。
2. **类型化审计能否承载令牌签发 / 撤销 / 拒绝**：`module_audit_event.rs:479-495` 的事件围绕模块工具调用（run、tool_call 关联）；是否要新增变体、是否满足「不放开自由 JSON」的字段校验，没读完。可能让 B 片超 300 行，届时拆 B2。
3. **K1-7 内核审计的整体落地状态**：只确认了追加接口存在，没有核对导出、保留（180 天）等是否全部就绪。
4. **提升授权的接线细节**：`resolve_target` 的 `supported` 名单与 `target_summary` 的结构按 `first_party.rs:498-520、574-576` 读过，但没读完整的授权消费与单次使用路径，B 片开工前要读全。
5. **Hyphae `requires.kernel=["events"]` 想表达什么**：CLI 没有回调通道，按 `no_channel` 只展示；是否让 Hyphae 发新锁未向其确认。
6. **`component.json` 与 `domain-os.yml` 的 kernel 声明**是否有测试钉在一起，没查（REGISTRY 漂移测试只对账能力 id 与提供方）。
7. **桥令牌文件的状态目录路径**：「状态目录下 `dev/`」具体对应哪个目录常量没确认（`packages/data-dir` 与 Rust `data_dir.rs` 的对应），C 片开工前定。
8. **桥 iLink 游标当前的推进位置**：只知道要求，没读 `bridge.ts` 现有游标逻辑，改动量未知。
9. **capabilities 模式下开发桥要先在桌面放行 uid（种子不生效）**的体验，未向 jason 确认过，README 会写明。
10. **新路由 `POST /api/v1/capabilities/bridge`** 没按 ADR-045 逐条核对，开工前复核。
