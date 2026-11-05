# ADR-045：路由命名约定

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（jason 2026-11-05 拍板接受；正文经 #1600 评审合入）。约定的方向由 jason 在 2026-11-04 拍板（架构优化方案 Q4：约定选 (a) 只约束新路由、旧路由冻结；sin90 选 (iii)；`_a24/*` 定为模块内部通道），本文是落笔稿。§8 开放问题里列出的细则（如参数名 `_name`、`_no` 后缀）随首个门禁测试 PR 一起定。
- 上位：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §4.1 S0-7、§5 Q4、B4、B5；[ADR-039](ADR-039-concept-model.md)（模块类资源都是「组件」）；[ADR-038](ADR-038-component-contract-v1.md)（`GET /api/v1/components`）。
- 相关：S1-1（OpenAPI 漂移收口，同一个门禁文件）；`docs/specs/SPEC-002-protocol.md`。
- 范围：只写约定和门禁该查什么，**不写测试，不改任何现有路由**。

## 1. 背景

`protocol/openapi.yaml` 现有 104 个 path、124 个 operation（`/api/v1` 之下）。风格不统一，几处是事实：

- 「模块」类资源分在 5 个顶级前缀下：`/os`、`/attached`、`/module-consents`、`/module-approvals`、`/components`。按 ADR-039，它们都是组件。
- 一处下划线：`/schedules/{id}/run_now`（`protocol/openapi.yaml:1551`），其余都是连字符。
- 叠词：`/documents/documents` 及其下 5 条（`protocol/openapi.yaml:2310` 起）。
- 路径参数混用：`{id}` 共 18 处，另有 `{name}`、`{module}`、`{tool}`、`{command}`、`{pattern}`、`{revision}`、`{page}` 和一批 `{xxx_id}`。
- 单复数混用：comm 用单数（`/comm/identity`），内核用复数（`/runs`、`/sessions`、`/schedules`）。
- 动作路径写法不一：`/runs/{id}/cancel`、`/schedules/{id}/suspend` 是子资源动作；`/idoris/install`、`/idoris/start` 是顶级动词。
- 另有一组不在 OpenAPI 里的内部路径 `_a24/*`，门禁没覆盖，也没登记在豁免表里。

不立约定，新接口会继续发散。改旧路由是 wire 不兼容（桌面、api-client、微信桥、外部 Sin90 都在用），代价大于收益，所以**只约束新路由，旧路由冻结**。

以上现状取自 main 上的 `protocol/openapi.yaml`、`rust/apps/agent24d/src/server.rs`、`rust/apps/agent24-documents/src/lib.rs`，并与 R1 报告 §7.2、§7.3（分支 `docs/arch-review-r1-module-map`）核对过。

## 2. 决策

### D1 适用范围：只约束新增路由

- 「新增」指本 ADR 合入之后新加的 `(方法, 路径)`。已有的全部冻结：路径、参数名、单复数一律不改，除非另立 ADR 并承担 wire 不兼容的后果。
- 冻结不等于合规。凡是不符合 D2 的旧路由，都列在 §4 的例外表里；例外表只能删，不能加（D9）。
- 适用面是所有对客户端可见的 HTTP 路由：OpenAPI 里的，以及 `UNDOCUMENTED_KERNEL_ROUTES`（`server.rs:4120`）里的、`MODULE_SURFACE_PATHS`（`server.rs:4135`）里的。

### D2 命名规则（新路由）

| 编号 | 规则 | 例 |
|---|---|---|
| N1 | 路径段小写字母、数字、连字符：`^[a-z][a-z0-9]*(-[a-z0-9]+)*$`。不用下划线，不用大写 | `/standing-grants`，不要 `/standing_grants` |
| N2 | 相邻两段不得同名，集合名不得与所在命名空间同名（叠词） | 不要 `/documents/documents` |
| N3 | 路径参数名只能是 `{id}`，或 `{<单数名词>_id}`、`{<单数名词>_name}`、`{<单数名词>_no}`（snake_case）。`_name` 用于以名字为键的资源，`_no` 用于序号（版本号、页码） | `/runs/{id}`、`/documents/{document_id}/revisions/{revision_no}` |
| N4 | 一条路径只有一个参数时可以写 `{id}`；有两个以上参数时，每个都必须带名词前缀，不得出现裸 `{id}` | `/groupchat/{group_id}/messages/{logical_id}` |
| N5 | 集合用复数名词；整个守护进程或整个用户只有一个实例的资源（单例）用单数名词，并且不带 `{id}` | `/schedules`、`/usage`、`/health` |
| N6 | 集合名、资源名是名词，不是动词。创建和删除用 `POST`、`DELETE` 加集合，不写 `/create`、`/delete` | 不要 `/runs/start` |
| N7 | 对已有资源的非 CRUD 动作写成 `POST /<集合>/{id}/<动作>`，动作是单个小写动词（多词用连字符），放在路径末尾，不再往下嵌套 | `POST /runs/{id}/cancel` |
| N8 | 长耗时、要查进度或可取消的动作，不要做顶级动词，改成「操作资源」：`POST /<域>/operations` 返回 `{id}`，之后 `GET`、`POST .../{id}/cancel` | 先例：`/idoris/operations/{id}`(`/documents/jobs/{job_id}` 也是这个写法,但属于历史写法,不当作先例) |
| N9 | 新增「模块类」资源（原来会放进 `/os`、`/attached`、`/module-*` 的东西）一律放在 `/components/{id}/...` 下；**不再新增顶级前缀**，也不再往这 5 个旧前缀里加子路由 | `/components/{id}/report` |
| N10 | 顶级前缀只能是 §4.3 列出的 30 个。要新增顶级前缀，先改本 ADR | — |
| N11 | 每条新路由同时要有：OpenAPI 条目、`route_policy.rs` 的行和 `Operation`（`rust/apps/agent24d/src/capabilities/route_policy.rs:62`）、一个 tag。这三项已有各自的门禁，本 ADR 不重复 | — |

N5 的「单数还是复数」无法由机器判断，归评审；其余规则都可以机器检查（见 §5）。

### D3 动词路径的边界

- 旧的顶级动词路径（`/idoris/install`、`/idoris/connect`、`/idoris/start`、`/idoris/stop`、`/comm/unlock`、`/comm/send`、`/comm/inbox/pull`、`/comm/daemon/start` 等）冻结。
- 新增同类需求时，按 N7（有资源）或 N8（无现成资源）写，不要照旧路径的样子加。

### D4 `{name}` 与 `{id}`

- 旧路由里 `{name}`（`/os/{name}`、`/attached/{name}`）和 `{module}`（`/module-consents/modules/{module}`）指同一类东西：模块名。冻结，不统一。
- 新路由里，凡是用稳定字符串名而不是生成的 id 当键的，写 `{<名词>_name}`；组件按 ADR-038 用组件 id，写 `/components/{id}`。

### D5 sin90 的 10 个端点：按 (iii) 处理

- `/sin90/*` 的 10 个 operation（`protocol/openapi.yaml:2050` 至 `:2265` 一带）留在本仓 OpenAPI，**标为「外部契约」，实现以 `iDoris-ai/Sin90` 仓为准**。本仓没有字面路由（`agent24-domain/src/lib.rs:79`：硬编码路由已删，由 `domain.rs` 动态挂载）。
- 它们仍列在 `MODULE_SURFACE_PATHS`（`server.rs:4135`），不受 N1 至 N10 约束，也不进 §4 的例外表：命名以 Sin90 仓为权威。
- 本仓不为它们新增任何路由。要改动 sin90 的端点，先在 Sin90 仓改，再回来同步 OpenAPI。
- 等 Sin90 进入活跃开发，再升级为 (ii)：留在本仓并加跨仓契约测试。升级的触发由 jason 决定，不由本 ADR 触发。
- 落地项（不在本片）：在 OpenAPI 里给这 10 个 operation 加 `x-external-contract: iDoris-ai/Sin90` 标记，并把 `MODULE_SURFACE_PATHS` 注释里的「M2-07 records whether they stay here」改成指向本 ADR。这两项随 S1-1 做。

### D6 `_a24/*`：模块内部通道

`_a24` 是内核与模块之间的内部通道，不是对客户端的 API。

- 保留段由内核代理强制：命名空间之后的第一段若规范化为 `_a24`（大小写不敏感），代理直接回 404，不转发（`rust/crates/agent24-os-proto/src/proxy.rs:948` `RESERVED_SEGMENT`，`:957`、`:1010` 判定）。
- 现有的 `_a24` 面（穷举，取自 main）：

| 类型 | 路径或方法 | 位置 |
|---|---|---|
| HTTP | `POST /documents/_a24/tools/{op}` | `rust/apps/agent24-documents/src/lib.rs:204` |
| HTTP | `POST /documents/_a24/authorization/revoked` | `rust/apps/agent24-documents/src/lib.rs:205` |
| HTTP | `POST /api/v1/<命名空间>/_a24/scheduler/fired` | `rust/apps/agent24d/src/scheduler_deliver.rs:129` |
| 协议方法 | `_a24/events/emit`、`_a24/model/complete`、`_a24/command/invoke`、`_a24/approval/gate`、`_a24/approval/advise`、`_a24/memory/private/*` | `agent24-os-proto/src/attach_mux.rs:48`、`drain.rs`、`agent24d/src/attach_registry.rs:206-212` |

- 规则：
  - 它们**不进 OpenAPI**，**不受 N1 至 N10 约束**（例如 `_a24/tools/{op}` 里的操作名 `job_get`）。这是有意豁免，不算例外表里的欠账。
  - 任何对客户端可见的路由，不得以 `_a24` 为第一段；OpenAPI 的 path 里不得出现 `_a24`。
  - `_a24` 只允许出现在「命名空间之后的第一段」。新的内部通道一律写 `_a24/<领域>/<动作>`。
  - 内部通道仍要被扫描：模块源码里每个 `_a24` 字面路由都必须列在门禁的内部通道表里（§5 C6），避免 R1 §7.1 指出的「既不在 OpenAPI 也不在豁免表」的盲区再现。
- 路由策略：模块路由统一按 `MODULE_LEVEL = Everyday` 处理（`route_policy.rs:45`）。`_a24` 路径因为客户端到不了，不额外分级。

### D7 新模块的命名空间

- 进程外模块挂在 `/api/v1/<namespace>/`，命名空间用模块名，规则同 N1。
- 模块自己的集合名按 N2 不得与命名空间同名；`documents` 模块的 `/documents/documents` 是已有的例外，冻结。

### D8 动态挂载的模块命名空间 `/api/v1/<ns>/**`

这一类不在 OpenAPI 的 path 列表里，也不是内置路由的字面量，单独说明：

- 来源：进程外或包形式的模块，在内核启动时按各自的 manifest 注册一条覆盖整个命名空间的任意方法代理（`rust/apps/agent24d/src/server.rs:3172` 起的 Package modules 代理；命名空间必须恰好等于由模块名派生的值，`agent24-domain/src/lib.rs:877-879`）。
- **N10 不适用**：顶级前缀白名单只管内置路由。模块命名空间不用改本 ADR，由组件 manifest 的命名空间登记，并受上面「命名空间必须由模块名派生」的检查约束。命名空间本身按 N1 取名（小写连字符）。
- **N1 至 N4 对子路径建议适用，属于建议级**：模块作者在自己的 `<ns>/**` 下应遵守，但门禁**只查内置路由**（§5），不扫模块源码里的路由；`documents` 的 `_a24` 路由是 D6 的单独扫描，不算在内。
- **N9 的边界**：N9 管的是内核自己新增的路由，不让它再长出新的顶级「模块类」前缀。动态挂载的模块命名空间本来就是每个模块一个顶级前缀，不违反 N9；但内核不得借「命名空间」之名新增内置的 `/<ns>/...` 路由。
- **D7 的边界**：D7（集合名不与命名空间同名）同样是建议级，只对新写的模块有意义，门禁不查。
- 已有的 `/documents/*`、`/sin90/*` 是这一类的实例：documents 的叠词记在 §4.2，sin90 按 D5。

### D9 例外表只减不增

- §4 的例外表是冻结范围的完整清单。路由被删除或改名时，从表里删掉对应行。
- 往表里加行，等于承认新的违规，必须另立 ADR。门禁据此运行（§5 C7）。

## 3. 备选方案及否决理由

| 方案 | 否决理由 |
|---|---|
| 趁 v1 一次性统一改名 | wire 不兼容：桌面、api-client、微信桥、外部 Sin90 同时受影响；收益只是一致性（方案 Q4 的 (b)） |
| 不立约定，靠评审把关 | 评审记不住 124 个 operation 的现有写法；新接口会继续沿用最近看到的那一种 |
| 把 sin90 迁到 Sin90 仓，本仓只留豁免 | 现在 Sin90 不活跃，迁移没有人接；先标「外部契约」，等 Sin90 活跃再升级（Q4 (iii)） |
| 把 `_a24/*` 补进 OpenAPI | 它不是对客户端的契约，写进去会被 api-client 生成出去，变成可被调用的假象 |

## 4. 冻结的例外表（旧路由里不符合 D2 的全部）

下表由 `protocol/openapi.yaml` 的 104 个 path 按 N1 至 N4、N10 逐条扫描得到，加上 `server.rs` 的 `UNDOCUMENTED_KERNEL_ROUTES` 做的核对（其中的参数名都已合规）。N5、N6、N7 属于评审项，不进表。

### 4.1 N1：下划线（1 条）

| 路径 | 位置 |
|---|---|
| `/schedules/{id}/run_now` | `protocol/openapi.yaml:1551` |

### 4.2 N2：叠词（6 条，均属 documents 模块）

`/documents/documents`、`/documents/documents/{document_id}`、`/documents/documents/{document_id}/find`、`/documents/documents/{document_id}/extractions`、`/documents/documents/{document_id}/revisions/{revision}/text`、`/documents/documents/{document_id}/revisions/{revision}/pages/{page}`。起点 `protocol/openapi.yaml:2310`。

### 4.3 N3：不合规的参数名（11 条）

| 路径 | 不合规参数 |
|---|---|
| `/module-consents/modules/{module}` | `{module}` |
| `/module-consents/modules/{module}/tools/{tool}` | `{module}`、`{tool}` |
| `/module-consents/modules/{module}/tools/{tool}/card` | `{module}`、`{tool}` |
| `/os/{name}`、`/os/{name}/delegation`、`/os/{name}/stop` | `{name}` |
| `/os/{name}/commands/{command}` | `{name}`、`{command}` |
| `/attached/{name}` | `{name}` |
| `/tool-overrides/{pattern}` | `{pattern}` |
| `/documents/documents/{document_id}/revisions/{revision}/text` | `{revision}` |
| `/documents/documents/{document_id}/revisions/{revision}/pages/{page}` | `{revision}`、`{page}` |

### 4.4 N10：顶级前缀（冻结的 30 个）

`approvals`、`attached`、`capabilities`、`chat`、`comm`、`components`、`decide`、`diagnostics`、`documents`、`events`、`health`、`host`、`idoris`、`memory`、`models`、`module-approvals`、`module-consents`、`os`、`peer`、`presence`、`runs`、`schedules`、`sessions`、`shutdown`、`sin90`、`standing-grants`、`timings`、`tool-overrides`、`tools`、`usage`。

其中 `capabilities`、`events`、`host`、`presence` 只在代码里、不在 OpenAPI（`server.rs:4120` 起的 `UNDOCUMENTED_KERNEL_ROUTES`）。

### 4.5 不进门禁、仅登记的旧写法

- 单复数混用（N5）：`/comm/identity`、`/comm/contact`、`/comm/relay`、`/comm/groupchat`、`/os`、`/attached` 用单数。
- 顶级动词（D3）：`/idoris/*`、`/comm/*` 里的 install、connect、start、stop、unlock、send 等。
- 顶级动词的归属：`/comm/outbox/clear`、`/comm/inbox/pull`、`/comm/daemon/start`、`/comm/daemon/stop`（`agent24-comm/src/router.rs:179-186`）与 `/idoris/*` 的动词一样，归 D3 冻结，不进 §4.1 至 §4.4 的机器检查。
- 两套审批面 `/approvals` 与 `/module-approvals` 并存，是有意的（`server.rs` 在注册处有注释）。
- `/comm/identity/default` 的 operationId 是 `useCommIdentity`，路径与动作不符。

## 5. 门禁测试应检查什么（本 ADR 不写测试）

放在 `rust/apps/agent24d/src/server.rs` 测试段、`kernel_routes_and_openapi_paths_have_no_unlisted_drift`（`:4275`）旁边，复用同一套路径来源：

- OpenAPI 的 `paths` 键；
- `route_literals` 扫出的内核路由、docked 路由器、comm 的三处字面路由（`COMM_ROUTE_SOURCES`，`:4162`）；
- 另加：`agent24-documents/src/lib.rs` 的路由字面量（S1-1 也要这个来源，两片共用同一个扫描函数）。

对每个路径（去掉 `/api/v1` 前缀；排除 `MODULE_SURFACE_PATHS` 里的 `/sin90/*`，它们按 D5 豁免）逐条检查：

| 检查 | 内容 | 通过条件 |
|---|---|---|
| C1 段名 | 每个非参数段匹配 N1 的正则 | 不匹配的路径必须在 §4.1 表里 |
| C2 叠词 | 相邻段同名；集合段与命名空间同名 | 命中的必须在 §4.2 表里 |
| C3 参数名 | 每个 `{...}` 匹配 `^(id|[a-z][a-z0-9]*(_[a-z0-9]+)*_(id|name|no))$` | 不匹配的必须在 §4.3 表里 |
| C4 裸 `{id}` | 同一路径有 2 个以上参数时，不得出现裸 `{id}` | 目前零命中，不设例外 |
| C5 顶级前缀 | 第一段属于 §4.4 的 30 个 | 新前缀一律失败 |
| C6 内部通道 | 任何路径的 `_a24` 只能出现在命名空间之后的第一段；OpenAPI 的 path 不得含 `_a24`；模块源码里每个 `_a24` 字面路由都在门禁的内部通道表里（目前是 documents 的两条） | 否则失败 |
| C7 例外表只减不增 | 例外表里的每一项都必须仍然违规 | 某项已不再违规（路由被删或已改名）时，测试失败，提示把它从表里删掉 |

测试自身的验收（AGENTS.md「测试先行、反面对照」）：

- 对每条检查各造一个合成的坏路径（例：`/foo_bar`、`/widgets/widgets`、`/widgets/{Name}`、`/a/{id}/b/{id2}`、`/brand-new-prefix`、`/widgets/_a24/x`），断言门禁对它失败。
- 把 `/schedules/{id}/run_now` 从例外表里临时删掉，断言门禁失败，证明例外表不是摆设。
- 往例外表里放一条不存在的路径，断言 C7 失败。

C1 至 C7 之外，N5、N6、N7、N8、D3、D7、D8 的子路径部分由评审把关，PR 模板可以加一句提醒，不进门禁。

## 6. 后果

- 新接口有了一页可查的规则，评审时可以直接引用 N 编号。
- 旧路由一个字不改，wire 兼容；但客户端会长期看到两种写法并存，这是有意的代价。
- 例外表是一份欠账清单，随路由删除自然缩短，不会悄悄变长。
- 门禁测试是一片独立的 S 级工作，随 S1-1 一起做较省事（同一个扫描函数、同一个测试文件）。

## 7. 与其他 ADR 的关系

- ADR-039：模块类资源统一归「组件」，N9 把新增的此类路由收到 `/components` 下。
- ADR-038：`GET /api/v1/components` 与 `/components/{id}/report` 是 N9 的先例。
- ADR-041：入站事件准入若新增路由，按本 ADR 命名。
- ADR-044（通讯）：comm 的单数写法冻结；通讯 ADR 若需新增路由，按 N5、N7、N8。
- ADR-K1-01：`_a24/*` 里的工具调用通道（`_a24/tools/{op}`）属于 D6，不属于对外路由。

## 8. 开放问题

1. N3 里 `_name` 和 `_no` 两个后缀是本文的补充（方案只要求「参数名在白名单内」）。如果评审认为只留 `_id` 更干净，就把 `{revision}`、`{page}` 这类序号也改成 `_id` 语义，或者把 `_no` 去掉。
2. D5 的 `x-external-contract` 标记是 OpenAPI 扩展字段；生成 api-client 时是否会带出，S1-1 实现时核实。**已核实（S1-1）**：openapi-typescript 7.13 忽略 `x-*`，加标记后 `openapi.d.ts` 无差异。
3. `/comm/install` 在 `unconfigured_router` 里缺失是否预期（方案 S1-1 已列），与命名无关，不在本 ADR 决定。**已核实（S1-1）**：预期行为，见 `comm_routes.rs` 的 `unconfigured_router` 注释与测试。
