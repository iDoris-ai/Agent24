# ADR-045：HTTP 路由命名约定（只约束新路由）

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（2026-11-04，jason 拍板架构优化方案 §5 Q4 按推荐执行：约定选 (a)，sin90 选 (iii)，`_a24/*` 定为模块内部通道）
- 上位：[SPEC-002 协议规范](../specs/SPEC-002-protocol.md) 第 2、3 条（`/api/v1/` 前缀、JSON 字段 `snake_case`）；[ADR-039](ADR-039-concept-model.md)（模块类东西统一叫「组件」）
- 相关：ADR-037 G-API-1（每条路由有 Operation）、G-API-3（代码路由与 OpenAPI 一致）；[ADR-DOC-02](../documenting/adr/ADR-DOC-02-operation-contract.md)（Documents 路由）
- 依据：[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) §3.2 B3–B5、§4.1 S0-7、§5 Q4；R1 报告 §7.1–§7.3（分支 `docs/arch-review-r1-module-map`，`docs/architecture/review-2026-11-03/R1-MODULE-MAP.md`）
- 核对基线：`origin/main` @ `2d5e8c02`（Merge #1582）

本文的路径都相对于 `/api/v1`，与 `protocol/openapi.yaml` 的 `paths` 写法一致。

## 1. 背景

- `protocol/openapi.yaml` 有 104 个 path、124 个 operation。另有 7 个内核路由有意不进 OpenAPI（`rust/apps/agent24d/src/server.rs:4120-4131` 的 `UNDOCUMENTED_KERNEL_ROUTES`），以及 Documents 的 2 个内部通道（`rust/apps/agent24-documents/src/lib.rs:204-205`）。
- 命名没有成文约定，已经发散（R1 §7.2）：
  - 「模块」类资源分在 5 个顶级段下：`/os`、`/attached`、`/module-consents`、`/module-approvals`、`/components`。
  - 单复数混用：comm 用单数（`/comm/identity`、`/comm/contact`），内核用复数（`/runs`、`/schedules`）。
  - 只有 `/schedules/{id}/run_now` 用下划线（`protocol/openapi.yaml:1538`）。
  - 路径参数 `{id}`、`{name}`、`{module}`、`{tool}`、`{pattern}`、`{command}` 混用。
  - 叠词：`/documents/documents/...`。
- 改旧路由会让 wire 不兼容。原型阶段收益小、代价大（方案 §6.2「不改旧路由」）。
- Sin90 的 10 个 operation 写在本仓 OpenAPI（`protocol/openapi.yaml:2037-2252`），本仓没有实现。它们靠 `MODULE_SURFACE_PATHS` 豁免（`server.rs:4135-4142`），实现在外部仓库 `iDoris-ai/Sin90`（`rust/apps/agent24d/src/domain.rs:11-14`）。
- Documents 的 `POST /documents/_a24/tools/{op}`、`POST /documents/_a24/authorization/revoked` 既不在 OpenAPI，也不在任何豁免表里。现有门禁不扫 Documents 源码（R1 §7.1）。

## 2. 决策

### D1 范围：只约束新路由，旧路由冻结

- 本约定只约束 2026-11-04 之后**新增**的路由，包括新路径，也包括在旧路径上新增的方法。
- 旧路由一律不改名、不改参数名、不改方法。§3 列出不符合本约定的旧路由，作为冻结例外。
- 例外表只减不增。旧路由删除或下线时，从例外表里删掉对应行。不允许为了新路由往例外表里加行；确实要破例，必须先修订本 ADR。

### D2 路径段

- 字面段一律小写 kebab-case：`^[a-z0-9]+(-[a-z0-9]+)*$`。不用下划线，不用大写，不用点号。
- 唯一例外：保留段 `_a24`，见 D7。
- 相邻两个字面段不得相同（不再出现 `/documents/documents` 这种叠词）。模块命名空间里，第一个集合名不要重复命名空间本身。

### D3 集合与单例

- 集合用复数名词：`/runs`、`/runs/{id}`。
- 单例资源用单数名词，即每个 daemon 只有一个、没有 id 的资源，例如 `/health`、`/usage`、`/decide/profile`。
- 新增 comm 路由按本条写：多个就用复数（`/comm/contacts`），只有一个才用单数。已有的 comm 单数集合列入冻结例外（§3.2）。
- 这一条要人判断是不是单例，门禁检查不了，靠评审把关。

### D4 动作子资源

- 不能自然表达为增删改查的操作，写成 `POST /{集合}/{id}/{动词}`。动词只占一个 kebab-case 段，例如 `/runs/{id}/cancel`、`/components/{id}/report`。
- 动作段只用于 `POST`。`GET` 不得以动词结尾。
- 能用资源方法表达的就不用动作段：改状态用 `PATCH /{集合}/{id}`，删除用 `DELETE`。
- 现有的 `install`、`start`、`stop`、`unlock`、`retry` 等动作段（R1 §7.2）都是 `POST`，已经符合本条，不是例外。

### D5 路径参数

- 参数名用 `snake_case`，并且必须在白名单里：
  - `{id}`：系统签发的不透明 id，例如 ULID。一条路径只有一个参数时，优先用它。
  - `{<单数资源名>_id}`：一条路径有多个 id 参数时用，例如 `{document_id}`、`{job_id}`。在已经统一用 `*_id` 的命名空间里（Documents 遵循 ADR-DOC-02），单个参数也可以这样写。
  - `{name}`：清单或用户给的稳定名字，不是系统签发的 id。例如领域 OS 名、attached 模块名。
- 同一个东西在不同路由里用同一个参数名。新的组件类路由用 `{id}`，与 `/components/{id}` 一致，不再新增 `{module}`。
- 门禁按正则 `^(id|name|[a-z][a-z0-9]*(_[a-z0-9]+)*_id)$` 检查。

### D6 模块类资源的前缀

- 按 ADR-039，模块、领域 OS、attached 模块、MCP 都是「组件」。新增的组件类路由一律放在 `/components` 或 `/components/{id}/...` 下。
- `/os`、`/attached`、`/module-consents`、`/module-approvals` 四个前缀冻结：保留现有路径，不在下面新增路径或方法。
- 进程外模块自己的业务路由，仍然放在自己的命名空间 `/{ns}/...` 下，例如 `/documents/...`、`/sin90/...`。命名空间就是模块名，命名空间里面也按 D2–D5 写。

### D7 `_a24/*`：模块内部通道

- `/{ns}/_a24/...` 是内核与进程外模块之间的**模块内部通道**，只有内核能调用。
- 内核代理拒绝转发 canonical 后第一段是 `_a24` 的客户端请求，返回 404（`rust/crates/agent24-os-proto/src/proxy.rs:934-948`，`RESERVED_SEGMENT = "_a24"`）。所以模块收到的 `_a24` 请求只可能来自内核的回调通道。
- 内部通道不进 OpenAPI，不是客户端契约。它们列入豁免表，并注明「模块内部通道」（§3.3）。门禁要扫进程外模块的路由源码，保证每条 `_a24` 路由都在豁免表里，豁免表里的每一行也都真实存在。
- `_a24` 只能出现在命名空间后面的第一段，不能出现在内核自己的路由里。
- 待核实：`route_policy` 对模块路由统一给 Everyday 级（`rust/apps/agent24d/src/capabilities/route_policy.rs:231-242`）。由于代理已经拦住客户端，`_a24` 实际上碰不到这一级判断，但这一点没有专门的测试。S1-1 补豁免表时一并核实。

### D8 Sin90：外部契约，实现以 Sin90 仓为准

- `/sin90/*` 的 10 个 operation 留在本仓 OpenAPI，标为**外部契约，实现以 `iDoris-ai/Sin90` 为准**（方案 Q4 的 (iii)）。
- 本仓不为它们写实现，也不加跨仓契约测试。等 Sin90 进入活跃开发，再升级为 (ii)：留在本仓，并加跨仓契约测试。
- 两仓不一致时以 Sin90 仓为准，本仓 OpenAPI 跟着改。新增 `/sin90/*` 路径也要过本约定的门禁。
- 在 OpenAPI 的 `sin90` tag 描述里写明「外部契约」，以及把 `MODULE_SURFACE_PATHS` 的注释「M2-07 records whether they stay here」（`server.rs:4133-4134`）改成指向本 ADR，都由 S1-1 完成。本 ADR 不改代码。

### D9 每条新路由都要登记

新路由在同一个 PR 里必须同时满足下面三条：

1. 进 `protocol/openapi.yaml`，按「方法 + 路径」登记；或者进豁免表，并写明理由与负责人。豁免表就是 `UNDOCUMENTED_KERNEL_ROUTES`、`MODULE_SURFACE_PATHS`，以及 §3.3 的模块内部通道表。
2. 内核路由在 `route_policy.rs` 有对应行（ADR-037 G-API-1；现有门禁 `registered_routes_match_the_operation_catalog`，`server.rs:7484`）。
3. 过本 ADR 的命名门禁（§4）。

## 3. 冻结例外与豁免表

### 3.1 不符合 D2 / D5 的旧路由（冻结，只减不增）

| 路径 | 不符合 | 说明 |
|---|---|---|
| `/schedules/{id}/run_now` | D2 下划线 | 唯一用下划线的路径 |
| `/documents/documents` | D2 叠词 | ADR-DOC-02 §4 定的路径 |
| `/documents/documents/{document_id}` | D2 叠词 | 同上 |
| `/documents/documents/{document_id}/extractions` | D2 叠词 | 同上 |
| `/documents/documents/{document_id}/find` | D2 叠词 | 同上 |
| `/documents/documents/{document_id}/revisions/{revision}/text` | D2 叠词；D5 `{revision}` | 同上；`revision` 是序号，不是 id |
| `/documents/documents/{document_id}/revisions/{revision}/pages/{page}` | D2 叠词；D5 `{revision}`、`{page}` | 同上 |
| `/module-consents/modules/{module}` | D5 `{module}` | D6 冻结前缀 |
| `/module-consents/modules/{module}/tools/{tool}` | D5 `{module}`、`{tool}` | 同上 |
| `/module-consents/modules/{module}/tools/{tool}/card` | D5 `{module}`、`{tool}` | 同上 |
| `/os/{name}/commands/{command}` | D5 `{command}` | 作用对象是 attached 模块，路径却挂在 `/os` 下（原因见 `server.rs:1639-1643` 的注释，R1 §7.2） |
| `/tool-overrides/{pattern}` | D5 `{pattern}` | 参数是工具名模式，不是 id |

这张表是用 D2、D5 的正则逐条扫出来的。扫描范围是 `protocol/openapi.yaml` 的 104 个 path，加上 `UNDOCUMENTED_KERNEL_ROUTES` 的 7 条和 Documents 的 2 条 `_a24`。共 113 个不同路径，除本表 12 条和 §3.3 的 2 条内部通道外，其余 99 条都符合 D2、D5。

### 3.2 不符合 D3 / D6 的旧路由（冻结，门禁不查，评审不追溯）

- comm 的单数集合段：`/comm/identity`、`/comm/contact`、`/comm/relay`、`/comm/groupchat` 及其子路径。
- D6 冻结的前缀：`/os/*`（5 个 path）、`/attached/*`（2 个）、`/module-consents/*`（5 个）、`/module-approvals/*`（2 个）。门禁在这四个前缀下只认基线已有的路径。
- 两套审批面 `/approvals` 与 `/module-approvals` 并存是有意设计，不算例外（`server.rs:1574-1575` 的注释）。

### 3.3 模块内部通道（D7，豁免出 OpenAPI）

| 路径 | 方法 | 所在模块 | 源码 |
|---|---|---|---|
| `/documents/_a24/tools/{op}` | POST | agent24-documents | `rust/apps/agent24-documents/src/lib.rs:204` |
| `/documents/_a24/authorization/revoked` | POST | agent24-documents | `rust/apps/agent24-documents/src/lib.rs:205` |

`{op}` 是内核回调用的工具操作名，不受 D5 白名单约束。内部通道不对客户端开放，参数名只对内核和模块有意义。

## 4. 门禁测试应检查什么（本 ADR 不写测试）

建议放在 `rust/apps/agent24d/src/server.rs` 的测试模块里，挨着 `kernel_routes_and_openapi_paths_have_no_unlisted_drift`（`server.rs:4275`），复用同一个 `route_literals` 扫描（`server.rs:4176`）。建议命名 `route_paths_follow_adr_045_naming`。

**输入**（全部取路径模板，去掉 `/api/v1` 前缀）：

- `protocol/openapi.yaml` 的全部 `paths`；
- `build_router_with_modules` 与 `docked_router` 的路由字面量；
- `COMM_ROUTE_SOURCES` 扫出的 comm 路由（加 `/comm` 前缀）；
- 进程外模块的路由源码。目前只有 `rust/apps/agent24-documents/src/lib.rs` 的 router，加 `/documents` 前缀。现有门禁不扫这一份。

**检查**：

1. **字面段**：每个非参数段匹配 `^[a-z0-9]+(-[a-z0-9]+)*$`。只有命名空间后的第一段可以是 `_a24`，而且该路径必须在 §3.3 表里。
2. **参数名**：`{...}` 里的名字匹配 `^(id|name|[a-z][a-z0-9]*(_[a-z0-9]+)*_id)$`。`_a24` 路径不查。
3. **叠词**：相邻两个字面段不得相同。
4. **冻结前缀**：`/os`、`/attached`、`/module-consents`、`/module-approvals` 下的路径必须属于基线集合（§3.2 列出的 14 个 path）。
5. **例外表要真实**：§3.1 的每一行必须仍然存在，而且仍然违反至少一条规则。路径已删除或已符合规则，测试就要求删掉这一行，防止例外表变成白名单。
6. **内部通道要真实**：§3.3 每一行在模块源码里存在，而且不在 OpenAPI 里；模块源码里每条 `_a24` 路由都在 §3.3 里。
7. **方法级**（与 S1-1 的「方法 + 路径」门禁合并做）：以已知动词结尾的路径（至少包括 `cancel`、`retry`、`start`、`stop`、`install`、`connect`、`unlock`、`send`、`import`、`resume`、`suspend`、`report`、`accept`、`revoke`、`clear`、`pull`、`probe`）只能有 `POST`。

**正对照**：PR 里要演示两件事。一是新增一条 `/foo_bar` 或 `/x/{module}` 时测试变红；二是从例外表删掉一行、但不删路由时测试也变红。

**不查**：单复数（D3）、动作能不能改写成资源方法（D4）、`{id}` 与 `{x_id}` 的取舍。这些靠评审。

## 5. 备选与否决

- **趁 v1 统一改名**（Q4 的 (b)）：wire 不兼容，要改桌面、CLI、两个 bridge、api-client 生成物和 Sin90 仓。原型阶段收益不抵代价。
- **把 sin90 迁到 Sin90 仓**（(i)）或**马上加跨仓契约测试**（(ii)）：Sin90 现在不在活跃开发，先标外部契约，等它活跃了再升 (ii)。
- **把 `_a24/*` 写进 OpenAPI**：它不是客户端契约，写进去会让 api-client 生成客户端调不通的方法。

## 6. 后果

- 新路由有了可以机器检查的规则。旧路由不动，没有兼容成本。
- 例外表只减不增，旧的不一致不会继续扩散。
- 门禁测试和 `_a24` 的 route_policy 核实留给 S1-1，本 ADR 合入后才有约束力。

## 7. 开放问题

- `/comm/install` 实现在 agent24d、前缀属于 comm，`unconfigured_router` 下没有这条路由（`rust/apps/agent24d/src/comm_routes.rs:790` 只在已配置的 router 里注册，`unconfigured_router` 在 `:748`；R1 §7.3）。这是不是预期，由 S1-1 核实，与命名无关。
- 要不要给组件类路由出一张从旧前缀到 `/components/{id}/...` 的迁移表：等哪天真要下线旧路由再议。方案 Q14 已定旧路由不设下线期。
