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
| 常驻授权的资格是否收紧 | **是。** 仅 `External`，且要求工具声明 target、调用真的填了 target | `agent24-protocol/src/types.rs:784`；`agent24-policy/src/lib.rs:238-242` |
| 「读权限只披露、不上存储」是否有机械门 | **未核到独立门。** 今天由「只有 `External` 能铸常驻授权」间接约束 | 见 L-APPR-7 |

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

**由一次批准铸出的常驻授权 MUST 形如 `tool → 确切目标`；其资格 MUST 同时满足：① 有效风险为 `external`；② 工具声明了 target 参数；③ 该次调用真的填了 target。`exec` MUST 永远逐次询问，MUST NOT 提供或承认会话级或常驻授权。`write_local` MUST 逐次询问，但有且仅有两例外：① **第一方 Documenting 模块**的可逆草稿写入，仅在启用时用户已明确同意该工具且当前权限指纹匹配、宿主确认操作只写模块私有存储并有修订历史且可撤回、不覆盖权威内容、不提交正式修订、不删除且不外发时，MAY 免逐次询问；② 内置写入的规范化目标落在用户已确认的工作区根内时，MAY 依 L-APPR-8 免逐文件询问。**

**挡的是什么**：常驻授权是「以后都不问」的承诺。把承诺从「这个工具」收窄到「这个工具对这个目标」，
是唯一能让 external 授权可撤销、可解释的形态；否则一次点击等于授出一个开放代理。草稿例外只覆盖可审阅、可撤回且不改变权威内容的私有提议，**且仅限第一方 Documenting 模块**。工作区写入例外只覆盖用户已确认的那一个工作区根，不覆盖执行，也不覆盖根外路径。正式 commit、覆盖权威内容、删除、Exec、External，以及来源、同意、指纹、修订历史、撤回性或数据路径无法核验的 WriteLocal 操作仍 MUST 逐次审批。「本会话都批准这个工具」不是确切目标：它会让一次点击覆盖该工具以后的每一次写和每一次执行。

**今天靠什么保证**：**部分靠机制**。external 匹配按 `(scope_kind, scope_id, tool, target)` 精确相等
（`agent24-store/src/repo.rs:639-670`）；external 资格三重收窄在 `agent24-policy/src/lib.rs:238-242`
（`standing_grant_eligible().then_some(standing_target).flatten()`）。K1-6a.5 的 `ConsentGate` 会把具备第一方来源、有效匹配同意和宿主核验语义的 `WriteLocal` 草稿标记为免逐次审批；manifest 的 `reversible_draft` 只是声明。工具分发路径仍未接入该核验结构：生产装配传入 `risk: External`、`reversible_draft_declared: false` 与 `draft_verification: None`（`rust/apps/agent24d/src/server.rs:1603-1604`），缺失宿主核验时按普通 `WriteLocal` 逐次审批。**门本身已经按本条收窄**（#1034 改 MUST 表述；#1108 合入 `53032dac`）：`ConsentGate` 免逐次人审要求 `WriteLocal` ∧ 请求声明草稿 ∧ 同意记录 `reversible_draft` ∧ 来源 `FirstParty` ∧ `record.module == "documents"` ∧ 宿主核验合格（`rust/crates/agent24-policy/src/consent_gate.rs:391-398`，常量 `DRAFT_EXEMPTION_MODULE`）。同意记录的草稿位由迁移 `0018_module_consent_reversible_draft.sql` 携带，存量行默认 0（非草稿同意）。**例外在生产装配下仍不可达**：上述门条件已在代码里，但没有调用方把宿主核验交给它。把例外接进工具分发路径的 PR 必须先解决下文「未决 / 接线闸门」。

**本条 2026-10-08 的修订溯源**（`docs/laws/APPROVAL.md` blob `16db4489` → `244429b0`；jason 于该日授权，依据见 [`ADR-K1-03`](../adr/ADR-K1-03-enable-authorization-trust-revocation.md) 的「K1-6a.5 补遗」）**随实现提交 `62218c31`（`ab/kernel`）一起提交，经 #1022（`56264ef9`）进入 main，未走独立法律 PR**。`62218c31` 本身不是 main 的祖先。这与 [`README.md`](README.md)「改变可观察行为的 PR…**不得**在同一个 PR 里既改法律又改行为」相冲突。2026-10-16 按该条要求补做了**独立的追溯性法律评审**（PR #1031），仅复核上述这段边界变更本身，**未改动本条的 MUST 表述**；本段是该评审留下的记录。

**该评审意见的处置**（2026-10-20，#1108 已合入）。评审结论是**有条件成立**，不是「意见都已处置、可以接线」。

- **L1（MUST 表述需收窄到第一方 Documenting）** —— 已由 PR #1034 在 main 上处置（`d40a34a0`）：
  本条 MUST 现为「**第一方 Documenting 模块**的可逆草稿写入」，不再泛指「第一方工具」。
- **L2（草稿例外只信请求自报，未核验同意记录里的属性）** —— 已由 PR #1108 合入 main（`53032dac`）：
  迁移 `0018_module_consent_reversible_draft.sql` 让同意记录本身携带该位（存量行回填为
  「非草稿同意」），`ConsentGate` 的豁免判定要求「请求声明 ∧ 同意记录属性 ∧ 第一方
  ∧ 模块为 `documents` ∧ 宿主核验合格」同时成立，并有两条回归
  （非草稿同意、以及第一方但非 Documenting 模块，均仍逐次人审）。
- **未决 / 接线闸门（L3–L5）**。把本条例外接进工具分发路径的 PR，必须先逐项解决下列三条，并在该 PR 内提供证据：
  - **L3**：「第一方」今天由宿主填入 `ConsentSource`，来源/签名核验未实现（`rust/crates/agent24-store/src/module_consents.rs:34-42`）。
  - **L4**：`!self.external` 没有负例，不能用「测试绿」证明 External 不会走草稿例外。
  - **L5**：适配器按 `(module, op)` 固定请求，拿不到本次调用输入，条件 ③–⑥ 无法逐次核验。

**2026-10-21 工作区写入例外**（jason 对 issue #794 的决定；本 PR 只改法律，不改行为）：每次写文件都弹窗不可接受；已确认工作区根内的内置写入可常驻放行；根外写入单独询问，只能「仅本次」或「一直允许这个目录」；执行仍永远逐次询问。细则见 L-APPR-8。

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

### L-APPR-7 · 授权 fail-closed，读权限不上存储

**只有写操作 MUST 能成为常驻授权；读权限 MUST 只在同意卡片上披露，MUST NOT 被存储。**

**挡的是什么**：把「读」也变成常驻授权，会让授权面从「可解释的动作」扩散到「探索」——
而探索的范围今天无法枚举。

**今天靠什么保证**：**部分靠机制，部分是设计约束。** 常驻授权只对 `External` 敞开
（`agent24-protocol/src/types.rs:784`），这是今天唯一的结构性约束；但
「读权限只在卡片披露、不存储」这一条**没有独立的机械门** —— 本次核实未在代码里找到它。
若将来给读类风险引入常驻授权，**必须先补机制**，否则这条法律就只剩措辞。

---

### L-APPR-8 · 工作区写入可常驻，根外写入必须指向确切目录

**内置写入 MAY 在用户已明确确认的工作区根内免逐文件询问；该确认只授权 `fs_write → 这个工作区根`，MUST NOT 授权执行，也 MUST NOT 授权根外路径。工作区根外的内置写入 MUST 单独询问，且只能提供「仅本次」与「一直允许写入这个规范化目录」；后者 MUST 可撤销，撤销或目录授权删除后 MUST 立即失效。判定 MUST 使用写入当下解析出的规范化路径，按路径分量比较，MUST NOT 信模型声明的字符串。家目录、`~/.ssh` 与系统目录 MUST NOT 作为工作区根。**

**挡的是什么**：每次写都问会让用户放弃审查；「本会话都批准写」又会让一次点击覆盖以后的每一个路径，包括工作区外。字符串前缀比较会把 `/proj` 误当 `/proj-evil`，符号链接和 `..` 会把写入送出工作区。把家目录当工作区根，等于一次确认就授出整机写权限。

**今天靠什么保证**：**今天没有这条机制。** `fs_write` 仍被工作区白名单直接拒绝根外写入，不会弹出「仅本次 / 一直允许这个目录」。`approve_for_session` 仍会放行同一会话内后续的 `fs_write` 与 `shell_exec`（`agent24-policy/src/lib.rs` 的会话授权快路径）。这与本条以及 L-APPR-5 的执行逐次询问不一致，是已知缺口，不是符合。行为修复必须另开 PR，不得与本法律变更同交。

---

## 与既有文档的关系

- [`../specs/TASKS.md`](../specs/TASKS.md) 的 H2 / H4 条目是本文件的来源，含「比原文更进一步」的收窄理由与实现备注。
- [`../reference-notes/openworker.md`](../reference-notes/openworker.md) 记录了 H2 与 Guardian 并存的取舍（用户声明优先于模型推断）。
- [`../ADR-026-rust-core-polyglot.md`](../ADR-026-rust-core-polyglot.md) 决策 3「可用决策集数据驱动」是 L-APPR-4 的协议面。
- [`MEMORY.md`](MEMORY.md) L-MEM-2「作用域由内核注入」与本文件同族：都不接受调用方传入的作用域/身份参数。
