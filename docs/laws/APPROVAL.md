# 审批与授权（Approval & Authorization）

> 谁能放宽一条风险、什么能把一次批准变成常驻。规则与形状见 [`README.md`](README.md)。
>
> 立于 2026-09-20。来源：[`../specs/TASKS.md`](../specs/TASKS.md) 的 **H2**（用户本地风险 override）
> 与 **H4**（external 定向常驻授权），两者均在 M-H 线合并（#61 / #62）；
> 另参考 [`../agent/architecture.md`](../agent/architecture.md) 与 [`../reference-notes/openworker.md`](../reference-notes/openworker.md)。

---

## 0. 核实记录（2026-09-20）

| 查了什么 | 结果 | 位置 |
|---|---|---|
| override store 是否只有一个用户写入入口 | **只有一个入口，但不是机械门。** 只有一个 `set_risk_override`，注释写明安装路径不得调用；**没有机械门** | `agent24-store/src/repo.rs:746-775`；`../specs/TASKS.md` §「H2」 |
| override 是否只改风险级、不放行具体调用 | **是。** override 参与算 `effective_risk`，放行仍走同一条门禁 | `agent24-tools/src/lib.rs:279+` |
| 内置工具能否被 override 放宽 | **不能。** 内置只可收紧，放宽沿 `RiskClass::escape_rank` 被拒并告警 | 同上 |
| external 是否还提供 `approve_for_session` | **不提供。** 有定向目标时只给 `approve_for_target` | `agent24-policy/src/lib.rs:40-66` |
| 常驻授权是否绑确切目标、挂 schedule | **是。** 匹配 `(scope_kind, scope_id, tool, target)`，schedule 优先 | `agent24-store/src/repo.rs:639-670`；`agent24-policy/src/lib.rs:129-137` |
| 常驻授权的资格是否收紧 | **是。** 仅 `External`，且要求工具声明 target、调用真的填了 target | `agent24-protocol/src/types.rs:1064`；`agent24-policy/src/lib.rs:165-168` |
| 「读权限只披露、不上存储」，以及模块工具授予这一唯一例外，是否有机械门 | **未核到独立门。** 今天由「只有 `External` 能铸常驻授权」和「授予入口默认关闭」间接约束 | 见 L-APPR-7 |

---

## 法律

### L-APPR-1 · override store 是 user-local

**风险 override 存储 MUST 只由用户的显式操作写入；
模块清单、persona、MCP server 与任何安装路径 MUST NOT 写入或以任何方式影响它。**

**挡的是什么**：如果安装器能写这里，**模块市场就等于让第三方自带豁免** —— 第三方代码的保守默认值归零。
包可以「声明」它想要什么工具，但只有机器的主人决定信任到什么程度。

**今天靠什么保证**：**机制 + 结构性，但不是机械门**。`RiskOverrides` trait 的文档把这条写成
inviolable（`agent24-tools/src/lib.rs:108-115`）；store 侧与 migration 注释同款
（`agent24-store/src/repo.rs:746-750`、`agent24-store/migrations/0002_risk_overrides.sql`）。
今天没有代码路径从安装入口调 `set_risk_override`，但**也没有测试或类型阻止未来加一条** ——
那将是一个 bug 而不是 feature，靠复审与这条法律发现。

---

### L-APPR-2 · 用户 override 优先于模型推断，且不等于放行

**用户 override 与模型推断（Guardian）冲突时 MUST 以用户 override 为准；
override MUST 只调整风险等级，MUST NOT 直接放行任何一次具体调用 ——
放行仍 MUST 走 `risk_class → 门禁` 同一条路径。**

**挡的是什么**：两种退化。① 让模型推断压过用户的显式声明 —— 用户失去对自己机器的控制；
② 把「调整风险级」误当成「批准这次调用」—— 一次 override 就绕过审批，而不是改变它被问的形式。

**今天靠什么保证**：**机制**。override 只参与 `effective_risk`（`agent24-tools/src/lib.rs:279+`），
门禁在它之后照常运行；算完有效风险后仍走同一个 `ApprovalGate`。

---

### L-APPR-3 · override 只能收紧内置，可纠正第三方

**用户 override MUST 总能收紧任一工具的风险级；
对内置工具 MUST NOT 放宽（只可沿 `escape_rank` 收紧）；
对第三方工具 MUST 允许放宽到用户选定的级。**

**挡的是什么**：`shell_exec → read`（「别再问我 shell 了」）与 `shell_exec → external`
（会悄悄让它有资格拿常驻授权）—— 这两条是 override 变成「没人记得开过的永久洞」的路径。
内置的等级不是猜测，是我们写下的代码；第三方的 `external` 是信息不足时的猜测，
机器的主人有资格纠正。

**今天靠什么保证**：**机制 + 告警**。`effective_risk` 对内置工具的放宽会 `tracing::warn!`
并保留声明级（`agent24-tools/src/lib.rs:279+`）；测试用 `FixedOverride` 覆盖放宽与收紧两侧（`:647` 起）。

---

### L-APPR-4 · external 工具不得提供宽泛的会话授权

**当一次调用的有效风险为 `external` 时，审批 MUST NOT 提供 `approve_for_session`；
MUST 只提供 `approve` / 定向的 `approve_for_target`（当且仅当资格成立）/ `deny` / `abort`。**

**挡的是什么**：在窄选项旁边摆一个宽选项，用户会点宽的那个 —— 因为它停止追问。
而宽授权随后覆盖那个工具够得着的**每一个**目标。让安全选项成为唯一选项，才是机制本身。

**今天靠什么保证**：**机制**。`decisions_for`（`agent24-policy/src/lib.rs:54-66`）：
`standing_grant_eligible()`（仅 `External`）为真时**不出** `approve_for_session`，
只在有定向目标时加 `approve_for_target`；非 external 工具保持原样。

---

### L-APPR-5 · 常驻授权必须窄到确切目标

**由一次批准铸出的常驻授权 MUST 形如 `tool → 确切目标`；其资格 MUST 同时满足：① 有效风险为 `external`；② 工具声明了 target 参数；③ 该次调用真的填了 target。`exec` MUST 永远逐次询问；`write_local` MUST 逐次询问，但第一方工具的可逆草稿写入仅在启用时用户已明确同意该工具且当前权限指纹匹配、宿主确认操作只写模块私有存储并有修订历史且可撤回、不覆盖权威内容、不提交正式修订、不删除且不外发时，MAY 免逐次询问。**

**挡的是什么**：常驻授权是「以后都不问」的承诺。把承诺从「这个工具」收窄到「这个工具对这个目标」，
是唯一能让 external 授权可撤销、可解释的形态；否则一次点击等于授出一个开放代理。草稿例外只覆盖可审阅、可撤回且不改变权威内容的私有提议；正式 commit、覆盖权威内容、删除、Exec、External，以及来源、同意、指纹、修订历史、撤回性或数据路径无法核验的 WriteLocal 操作仍 MUST 逐次审批。

**今天靠什么保证**：**部分靠机制**。external 匹配按 `(scope_kind, scope_id, tool, target)` 精确相等
（`agent24-store/src/repo.rs:639-670`）；external 资格三重收窄在 `agent24-policy/src/lib.rs:238-242`
（`standing_grant_eligible().then_some(standing_target).flatten()`）。K1-6a.5 的 `ConsentGate` 会把具备第一方来源、有效匹配同意和宿主核验语义的 `WriteLocal` 草稿标记为免逐次审批；manifest 的 `reversible_draft` 只是声明。当前工具分发路径尚未接入该核验结构，实际操作路径与可逆语义的宿主核验仍须由调用方提供，缺失时按普通 `WriteLocal` 逐次审批。

---

### L-APPR-6 · 常驻授权挂在 schedule 上，不是会话上

**由自动化（schedule）触发的 run 铸出的常驻授权 MUST 挂在发起它的 schedule 记录上；
MUST 在该 schedule 删除时一并失效。**

**挡的是什么**：用户批准的是「**这个自动化**可以发到那里」，不是「这个对话可以」。
挂在会话上会让撤销变得不可解释：删掉任务，授权还在。

**今天靠什么保证**：**机制**。`grant_scope` 在两者都有时**优先 schedule**
（`agent24-policy/src/lib.rs:129-137`）；store 有按 schedule 清除的删除路径
（`agent24-store/src/repo.rs:605`）。

---

### L-APPR-7 · 授权 fail-closed，读权限默认不上存储，模块工具授予是唯一例外

**读权限 MUST 只在同意卡片上披露，MUST NOT 被存储。唯一的例外是模块工具的授予记录：它 MAY 存储读类工具的许可和资源范围，当且仅当下列四项同时成立；任何一项不成立，该授予 MUST NOT 被存储，也 MUST NOT 被派发使用。存储的授予只在当前有效时才能放行调用。**

① **来源**：由宿主在用户于启用时的明确确认之后签发（ADR-K1-03 §1），逐个工具授予，绑定模块版本和制品身份；模型、模块、工具返回值与模块自报的风险都不能创建、扩大或续期它。宿主也 MUST NOT 沿用之前的确认延长期限，或在到期后重新签发：新的期限要有覆盖它的新的明确同意。只收窄范围、不延长期限的换发不受此限。

② **范围**：资源范围只含精确的资源目标（K1-06a 资源契约 §2.1：`<kind>:<id>`），种类须在该工具声明的资源种类之内（同上 §2.3），MUST NOT 含通配、前缀或范围；没有选资源就是没有任何资源权限，MUST NOT 被当成「全部」。

③ **期限与撤销**：有明确的有限到期时间，MUST NOT 永久；可撤销。撤销或到期之后，新调用 MUST NOT 通过；撤销、到期、收窄、扩大或改为拒绝，MUST 使排队与在途调用失效，并在调用返回后、结果交给模型之前复验（ADR-K1-03 §3）；无法可靠取消或复验时 MUST fail closed。

④ **变化要重新同意**：范围扩大，或权限指纹、模块版本、制品身份、来源变化，或模块被禁用、停止后再启用，旧授予 MUST 失效，重新同意之前不得派发（ADR-K1-03 §3、§4）。

这一例外只回答「能不能存」，不回答「要不要逐次审批」：存了授予并不免除逐次审批；免除只在宿主已核验来源为第一方、并按工具实现核验了实际动作之后才可能（ADR-K1-03 §1、§4）；第三方与手动安装的模块，以及仅凭模块自报为读、宿主未按实现核验其动作的工具，仍然逐次审批。这类授予由启用确认签发，不是 L-APPR-5 所说「由一次批准铸出的常驻授权」，不适用其 external 资格；写、执行与外发不在本例外之内，仍按本文件其余条款。

**挡的是什么**：把「读」也变成常驻授权，会让授权面从「可解释的动作」扩散到「探索」——
而探索的范围今天无法枚举。例外把这个面收窄到「用户在启用时点名的、逐工具逐资源的、会过期的」授权：范围可枚举、可到期、可撤销，
否则一次启用确认会变成对整个资料库的长期读权限。

**今天靠什么保证**：**部分靠机制，部分是设计约束；例外部分的机制还没有全部落地。**
常驻授权只对 `External` 敞开（`agent24-protocol/src/types.rs:1064`），生产里模块工具一律装配成 `External`
（`rust/apps/agent24d/src/server.rs:1655`），来源一律记为 `ManualInstall`（`agent24-policy/src/tool_summary.rs:47`）。
授予入口（`POST …/module-consents/modules/{module}/tools/{tool}`，`rust/apps/agent24d/src/consent_routes.rs:404`）已合入，
但由宿主开关 `A24_MODULE_TOOL_GRANTS` 控制，默认关闭（`consent_routes.rs:296`），所以默认配置下没有任何读类模块工具授予被存储。
授予记录已能携带精确资源集合与摘要、到期时间（`agent24-store/src/module_consents.rs:276-337`、`agent24-policy/src/consent_gate.rs:296-299`）。
**四项条件目前没有一道把它们合在一起的机械门**；落地方式写在 `docs/design/K1-06a-grant-path.md` §2.1–§2.5：
授予写入处按 ①②③④ 校验并拒绝写入，派发处按授予记录复验（含制品身份、代次、到期），撤销与到期取消在途调用并通知模块。
授予路径的全部切片（同一设计 §3）合入并有反例测试之前，**本条例外只是法律，不是机制**，授予开关 MUST 保持关闭。
---

## 与既有文档的关系

- [`../specs/TASKS.md`](../specs/TASKS.md) 的 H2 / H4 条目是本文件的来源，含「比原文更进一步」的收窄理由与实现备注。
- [`../reference-notes/openworker.md`](../reference-notes/openworker.md) 记录了 H2 与 Guardian 并存的取舍（用户声明优先于模型推断）。
- [`../ADR-026-rust-core-polyglot.md`](../ADR-026-rust-core-polyglot.md) 决策 3「可用决策集数据驱动」是 L-APPR-4 的协议面。
- [`MEMORY.md`](MEMORY.md) L-MEM-2「作用域由内核注入」与本文件同族：都不接受调用方传入的作用域/身份参数。
