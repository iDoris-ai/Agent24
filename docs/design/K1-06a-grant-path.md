# K1-6a-G —— 模块工具的内核授予路径

> 状态：设计，2026-10-21。负责人：David。任务：#705 剩余部分的 A 线（DOC-1-06b §4）。
> 依据：ADR-K1-01 §2.2–§2.4；ADR-K1-02 §2.5；ADR-K1-03 §1、§3、§4；ADR-K1-04 §2.1；K1-06a-resource-scope（下称「资源契约」）；APPROVAL L-APPR-7 与 #1123；DOC-1-06b §0、§4（`ab/documenting`）。
> 本文只定对外接口、守卫顺序和安全前提。表结构、函数拆分和界面细节交给实现片，用代码和测试验证。

## 0. 结论

- 补齐生产里缺的授予入口：宿主生成确认卡片 → 用户确认（带资源范围、期限）→ 存成授予记录 → 派发时按记录填 `authorized_resources`。
- 授予入口合入时**默认关闭**，由宿主配置项打开。打开之前要同时满足：K1-6a 与 K1-6b 都验收、B（Documenting 资源校验）合入、#1123（L-APPR-7 修订）合入。所以代码可以先合，产品行为不变。
- **两道守卫先于授予入口落地，最晚同一片**：派发时重查远端档位守卫（§2.4），撤销会停掉在途调用（§2.5）。不能先有授予，再补守卫。
- 风险等级「跟随工具声明」会让读类工具在授予后免去逐次审批，实际上就是读类常驻授权，所以这一步排在 #1123 合入之后（§2.3）。

## 1. 现状（`ab/kernel` @ 0210d972）

| 项 | 现状 | 出处 |
|---|---|---|
| 授予 | `grant_module_consent` / `deny_module_consent` 只有测试调用；HTTP 只有列出、导出、撤销 | `agent24-store/src/module_consents.rs:505,518`；`agent24d/src/server.rs:1081-1093`；`consent_routes.rs` |
| 权限摘要 | 生产只有一份写死的文本，所有工具共用 | `server.rs:1580-1592` |
| 风险 | `RiskClass::External`、`ManualInstall`、`High` 写死，manifest 的 `risk`、`reversible_draft` 未使用 | `server.rs:1587,1591,1598,1636` |
| 资源 | `authorized_resources` 恒为空；表和记录没有资源列；审计 `resource_ref` 恒为 `None` | `consent_gate.rs:244`；`migrations/0015_module_consents.sql`；`agent24-tools/src/lib.rs:685` |
| `authorization_ref` | `{scope_fingerprint}:{decided_at}`，进审计；审计引用 ≤128 字节、字符集受限 | `consent_gate.rs:245-248`；`module_audit_event.rs:40-67` |
| 远端档位守卫 | 只在通告时检查；`dispatch`、`execute_preapproved`、`ModuleTool::call_with_invocation` 都不查；档位在启动时固定 | `agent24-tools/src/lib.rs:772-775,1456-1575,898-1018`；`server.rs:1618` |
| 撤销 | 持久化并广播，但广播没有生产订阅者；授权只在调用开始时查一次；用户撤销、到期都不停在途调用（只有禁用/停止模块经 generation drain 能停）；不通知模块 | `module_consents.rs:396-431`；`agent24-tools/src/lib.rs:911-914`；`kernel_call.rs:360-367` |
| manifest | `RawToolDeclaration` 无 `resource_kinds`；`DAEMON_PROTOCOL_VERSION = 1` | `agent24-domain/src/tool.rs:149-162`；`agent24-domain/src/lib.rs:405,716-724` |
| 函数名 | `module.op` 原样作为 provider 的 `function.name` | `agent24-tools/src/lib.rs:733`；`agent24-models/src/lib.rs:607-611` |
| 界面 | Desktop 没有模块授权卡片和列表 | — |

## 2. 契约与安全前提

### 2.1 确认卡片和授予接口

- **摘要只由宿主生成**：用一个函数，从 manifest 的工具声明（`risk`、`output_privacy`、`resource_kinds`、`reversible_draft`）和宿主判定的来源、风险，生成 `ToolPermissionSummary`。启动装配和授予接口用同一个函数，所以授予时存的指纹和派发时算的指纹逐字节相同；不同即 `Stale`，拒绝。模块自报的描述文字不进摘要。
- `GET /api/v1/module-consents/modules/{module}/tools/{tool}/card`：返回卡片，内容是 ADR-K1-03 §1 的四项、可授予的资源种类和摘要指纹。
- `POST /api/v1/module-consents/modules/{module}/tools/{tool}`，正文 `{decision: "granted"|"denied", fingerprint, resources: [...], expires_at}`：
  - `fingerprint` 必须等于宿主当下重新生成的指纹，否则返回 409，不写记录（用户确认的必须是当下的权限）。
  - `resources` 每项符合资源契约 §2.1，种类在该工具声明的 `resource_kinds` 内，最多 256 项；否则返回 422，不写记录。
  - `expires_at` 必填且有界（例如不超过 90 天），不接受「永久」。
  - 收窄、扩大都写新记录（资源契约 §3.2）；扩大必须走这个接口重新确认。
- 这两个接口和现有的 consent 路由一样走内核认证（`server.rs:1081-1093`），只接受用户身份。Agent、模块和 creative 身份都不能调用（creative 已被拒，`server.rs:3641`）。模型能用的工具里，没有任何一个能到达这两个接口。
- **默认关闭**：宿主配置项（例如 `module_tool_grants.enabled`，默认 `false`）关闭时，`POST` 返回 403，`GET card` 照常（便于界面开发）。打开条件见 §0，打开本身是一次单独的发布决定，不随代码合入。

### 2.2 授予记录、资源集合与引用

- 资源集合存进授予记录（新增列或子表），随记录持久化；存储写失败就不产生授予。读到没有资源集合的旧记录，按空集合处理。
- 资源集合摘要：对去重、按字节序排序后的元素集合做规范编码，再取 SHA-256。
- **`authorization_ref` 改成单个摘要**：`ga1:` 加 SHA-256(权限指纹, 资源集合摘要, `decided_at`) 的十六进制，共 68 字节，满足审计的长度和字符集要求。原先直接拼接会超过 128 字节。范围一变，引用就变（资源契约 §5 第 8 条）。
- 审计 `resource_ref` 记 `rs:<个数>:<资源集合摘要>`，不记具体 ID（资源契约 §2.4）。
- ConsentGate 放行时带出记录里的资源集合；适配层按记录原样填进 `authorized_resources`。派发前再校验一遍格式和上限，不合规就拒绝这次调用（`permission_denied`），不截断。

### 2.3 风险等级

- `HostRiskLevel` 和 `ConsentSource` 不变：在宿主能核验来源（ADR-K1-03 §4，签名或来源校验）之前，一律按 `ManualInstall` + `High` 处理，不因自报第一方而降级。
- `RiskClass` 跟随声明的 `risk`（DOC-1-06b §4 第 2 条），条件是：该工具有当前有效的授予，且 #1123 已合入。之前一律 `External`，每次调用都审批。跟随之后，读类工具免去逐次审批；写和执行类仍按 L-APPR-5 逐次审批。
- 宿主可以收紧，不能放宽：声明 `read` 但宿主判定会外发的工具，仍按 `External` 处理。

### 2.4 派发时重查远端档位守卫

- 守卫放在 `ModuleTool::call_with_invocation` 里，先于授权查询，所以 `dispatch` 和 `execute_preapproved` 两条路径都会经过。按名字直接调用被守卫挡下的工具，返回 `permission_denied`，并写审计。
- 守卫每次读当前的档位配置，不用启动时的快照（`server.rs:1618`）。读不到时按「有远端档位」处理。
- 撤掉守卫的条件仍按 ADR-K1-02 §2.5，本文不改。

### 2.5 撤销、到期与在途调用

- 内核订阅撤销广播，并在 `expires_at` 设定时器。撤销或到期时，取消该（模块，工具）所有在途调用（复用调用自己的取消令牌），终态记为 `cancelled`，模型看到 `permission_denied`。广播溢出（`Lagged`）时，取消该模块的全部在途调用。
- 调用返回后、把结果交给模型之前，再查一次授权；已撤销就丢弃结果，返回 `permission_denied`。
- **通知模块**：撤销或到期时，内核向模块发 `POST /api/v1/<module>/_a24/authorization/revoked`，正文 `{operation, revoked_at}`，其中 `operation` 为 `"*"` 表示整个模块。模块收到后，停止由该操作发起、尚未结束的后台工作（资源契约 §3.5），到达安全点再返回 2xx。内核等这个应答，或者等到期限，才把撤销记为完成；失败或超时时，在审计和授权列表里标注「停止未确认」，不报告成功（ADR-K1-03 §3）。
- 本身只在本机运行、不经出口、结果只能经范围检查读取的后台工作（例如 Documenting 的文本层生成），可以跑完，不算违反撤销。经内核回调访问模型等出口的后台工作，由内核出口门按当前授权复验（K1-6b）。

### 2.6 manifest 与兼容

- `RawToolDeclaration` 加可选字段 `resource_kinds`（资源契约 §2.3）。
- `DAEMON_PROTOCOL_VERSION` 升到 2。带 `tools` 或 `resource_kinds` 的 manifest 写 `min_daemon_protocol: 2`，旧内核会在加载时明确拒绝，而不是报未知字段。DOC-1-06b §4 第 6 条写的「仍是 2」与代码不符，以代码的 1 为准。

### 2.7 模型可见的函数名

- 发给 provider 的函数名把 `.` 换成 `__`（`documents.list` → `documents__list`），回放和解析工具调用时按同一规则还原。内部全名、审计、授予记录仍用 `module.op`。生成的名字若和已注册的工具冲突，或超过 64 字符，注册时报错。

## 3. 切片（每片 ≤300 行，base `ab/kernel`）

| 片 | 内容 | 前置 |
|---|---|---|
| A1 | §2.4 派发时守卫与动态档位；§2.5 撤销/到期停在途调用（不含通知模块） | 无 |
| A2 | §2.6 `resource_kinds` 解析与协议版本；§2.1 摘要生成函数，启动装配改用它（摘要文本变化会让测试里的旧授予变成 `Stale`，属预期） | 无 |
| A3 | §2.2 资源集合存储、`authorization_ref`、`resource_ref`、填 `authorized_resources` | A2 |
| A4 | §2.1 卡片与授予接口（默认关闭），OpenAPI | A1、A2、A3 |
| A5 | §2.5 通知模块（内核侧）；Documenting 侧的接收与停止另开 `ab/documenting` 片 | A1 |
| A6 | §2.7 函数名 | 无 |
| A7 | §2.3 `RiskClass` 跟随声明 | #1123 合入、A4 |
| UI | Desktop 确认卡片与授权列表 | A4；负责人待定 |

打开授予开关不属于任何一片：要等 §0 的全部前置满足，并且 A1–A5、A7、UI 和 Documenting 的 B 都已合入。

## 4. 反例测试（实现片先写，并证明修复前失败）

1. 有授予、有远端档位：按名字直接 `dispatch` 文档工具 → 被拒，有审计（A1）。
2. 运行中加入远端档位：下一次调用被拒（A1）。
3. 在途调用中撤销 → 调用以 `cancelled` 结束，模型看到 `permission_denied`；新调用被拒（A1）。
4. 到期时刻在调用中间到达 → 同第 3 条（A1）。
5. 调用返回后、结果交出前被撤销 → 结果被丢弃（A1）。
6. 卡片指纹和当下不一致 → 409，无记录；资源越界、种类未声明、257 项、无期限 → 422，无记录（A4）。
7. 开关关闭 → `POST` 返回 403，无记录（A4）。
8. Agent、模块、creative 身份调用授予接口 → 被拒（A4）。
9. 授予不带资源 → 派发的列表为空；范围变化 → `authorization_ref` 变化；审计里没有资源 ID（A3）。
10. 存的资源集合被改坏（格式错误、超过 256 项）→ 派发拒绝，不截断（A3）。
11. 撤销后模块通知失败或超时 → 撤销状态标注「停止未确认」，不报成功（A5）。
12. 声明 `read` 的工具，在 #1123 前仍逐次审批（A7 之前的行为保持）。

## 5. 不在本文范围

撤掉远端档位守卫（ADR-K1-02 §2.5）；第一方来源核验（ADR-K1-03 §4）；UI 与 Agent 统一业务授权（ADR-K1-04 §2.4）；通配和修订级范围（资源契约 §6）；Documenting 侧的通知接收（另开片）。
