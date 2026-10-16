# OfficeAssistant 技术方案（草案）

> 状态：**设计草案，待评审**；写于 2026-10-16，基于当日 main（`92f9f0cf`）。不是 ADR，不批准任何实现；需要定稿的部分在文末列为「待 ADR」。
> 产品计划见 [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md)（下称「计划」）；事实源见 [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md)（下称「报告」）。
> 涉及代码的位置均为本次在 worktree 中实际打开或检索过的；行号对应当前树，随代码变化会漂移。

---

## 1. 技术栈

### 1.1 沿用（不新增框架）

| 层 | 现有基础 | 依据 |
|---|---|---|
| 内核 | Rust daemon `agent24d`：Tokio/Axum，REST/WS；store 用 SQLite（sqlx）+ 哈希链审计；policy/tools/MCP/model router/memory/scheduler 为既有内核服务 | 报告 §4「Existing reusable foundations」；`../ARCHITECTURE-LAYERS.md` §1 图与 §1.1 依赖表 |
| 桌面 | TypeScript / Electron / React / Vite；已有 i18n（检索到泰语条目，`apps/desktop/src/renderer/i18n/index.ts`） | 报告 §4 同段；本次检索 |
| 协议 | `protocol/openapi.yaml`（手写权威）、`protocol/events.schema.json`（由 Rust 导出） | `../ARCHITECTURE-LAYERS.md` §2 L0 |
| 授权/审计 | K1：`module_consents`、`module_consent_revocations`、`run_source_tags`；`StoreConsentGate`；模块工具 `PreDispatch`/`Terminal` 类型化审计 | `K1-RELEASE-NOTES-20261016.md` 第 15–31 行 |
| 模型 | `ModelRouter`（`TaskProfile` 路由，`LocalOnly` 只选 `Local`/`Lora`） | 同上第 33、149–158 行 |

### 1.2 明确不做

- **不引入第二套通用 Agent 框架**，不用其替换内核（报告 §4「Recommended trade-offs」最后一行）。
- **不因历史原型提过 Postgres/LangGraph 就加库**；Components 的 `assistant_status` 返回的字符串不证明这些依赖已部署（报告 §4 第 2 段与表后段）。LangGraph + 独立状态库只在证明 Agent24 无法满足后作为候选（同表第 3 行）。
- **不锁定具体模型版本**；用真实的会议抽取与多步工具评测决定是否需要更高档（报告 §4「For models…」）。

---

## 2. 分层与职责

```
┌───────────── UI 投影层（Desktop）─────────────┐
│ 开始任务 · 任务详情 · 连接与权限（+ 技术页）    │  只渲染内核快照与服务端给的选项
└──────────────────────┬────────────────────────┘
                       │ REST / WS（读快照 + 事件；断连后重读快照）
┌──────────────────────▼────────────────────────┐
│ 业务编排层（薄）：workflow / step 状态机        │  计划版本、步骤依赖、投影状态
│   不持有审批/授权结论，只引用内核记录           │
└──────────────────────┬────────────────────────┘
                       │ 走内核 run / 工具调用 / 审批门
┌──────────────────────▼────────────────────────┐
│ 控制面（内核 agent24d）：run · 审批 · consent ·  │  唯一权威：状态、批准、授权、审计
│ 来源标签/LocalOnly · 审计 · workspace          │
└──────────────────────┬────────────────────────┘
                       │ 注册为工具（内置或模块），受同一门禁
┌──────────────────────▼────────────────────────┐
│ 连接器层：本地材料 · mock · 邮件 · Notion · LINE │  有界动作；不自带授权
└───────────────────────────────────────────────┘
```

| 层 | 拥有 | **不拥有** |
|---|---|---|
| 控制面（内核） | run 生命周期、审批决定、consent 记录与撤销、来源标签与有效模式、审计链、workspace 权限 | 业务语义（纪要是什么、行动项字段） |
| 业务编排层 | workflow 定义、计划版本、步骤依赖、用户可见投影状态（如「部分完成」「结果待核验」） | 任何批准/授权结论（只引用内核的 approval/consent ID）；任何出站执行通道 |
| 连接器层 | 与外部服务的协议细节、幂等键的落地方式、对账查询 | 授权（凭据只证明技术身份，不代替业务授权；报告 §5「Four capabilities」首段）；风险自报不降低宿主风险级（ADR-K1-03 决策 1 风险行） |
| UI 投影层 | 呈现、编辑输入、发起请求 | 权威状态；不得自建一套审批结果（报告 §4 表后段末句）；断连后不得凭本地动画推断成功（报告 §3 Status 行） |

**放在哪里（已定：见 [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md) §0.4 D-A）**：采用 **(b) 受监管的进程外第一方领域 OS 模块 `office`**，经 os-proto 回调接内核（报告 §4 表第 2 行）。原先「倾向 (a) 编译进内核的薄 crate」的两条理由经复核不成立：M1 不向 agent loop 通告工具，用不到 consent 前置；而 (a) 会把领域名词与不可回退的内核迁移带进内核。无论哪种，都不得开辟绕过模型/工具控制的旁路。

**命名**：后端 ID 分开且显式关联：`workflow_id`、`run_id`、`workspace_id`、`artifact_id`（报告 §1「Distinct meanings of Workspace」第 2 段）；UI 用「任务」，文件位置设置用「文件工作区」。

---

## 3. 最小数据契约

报告 §4「Proposed minimum data contract」给出四类记录；以下为字段细化与复用关系。全部是**待定的 OfficeAssistant 契约**，不是 Agent24 已有 API（报告同节末句）。

### 3.1 记录与关系

```
workflow 1 ──< step >── 0..1 action 1 ──< attempt
   │                         │              │
   │                         ├── approval_ref（→ 内核 Approval.id / consent 记录）
   │                         └── receipt 0..1
   └──< artifact（版本链；action 的输入/输出引用 artifact 版本）
run：每个 step 关联一个或多个内核 run（新 attempt = 新 run，不复活终态 run）
```

| 记录 | 字段（建议） | 新增 / 复用 |
|---|---|---|
| **workflow** | `workflow_id`、`goal`（一句话）、`source_refs[]`（规范来源 ID + 修订/哈希）、`plan_version`、`steps[]`（依赖）、`run_ids[]`、`projection_status`、`created_by`（宿主注入的主体）、`created_at` | **新增**。`source_refs` 复用 ADR-K1-02 §2.1 的来源标记语义与 `run_source_tags` |
| **action** | `action_id`、`step_id`、`tool`/`connector`、`account_ref`（不含凭据）、`exact_destination`、`effective_input_digest`（最终 payload 哈希）、`payload_version`、`approval_ref`、`idempotency_key`、`attempt_count`、`risk_class`（宿主计算）、`simulated: bool` | **新增**；`approval_ref` 指向内核 `Approval`（`agent24-protocol/src/types.rs:514-540`）或模块审批/consent 记录 |
| **attempt** | `attempt_id`、`action_id`、`run_id`、`tool_call_id`、`started_at`、`outcome_class`（闭集）、`reconciled: bool` | **新增**；`run_id + tool_call_id` 与 ADR-K1-04 §2.1 审计关联一致 |
| **artifact** | `artifact_id`、`type`、`location`（受控存储引用）、`version`、`content_hash`、`parent_version`、`edited_by_human: bool`、`evidence[]`（source_quote 位置）、`read_scope`/`write_scope` | **新增**；存储位置复用内核受控存储（具体选型待 ADR，见 §7） |
| **receipt** | `receipt_id`、`action_id`、`business_outcome`（闭集，见 §4.3）、`external_id`/`link`、`provider_accepted_at`、`verified_at`、`verification_method`、`compensable: bool` | **新增** |

### 3.2 敏感正文与元数据分离

- 正文（转录、纪要、邮件正文、页面内容）只放在受控 artifact 存储；workflow/action/receipt、事件与审计只放必要元数据（报告 §4 数据契约末段）。
- 审计沿用 ADR-K1-04 §2.1：不得记录正文、凭据、完整提示、完整工具参数、自由文本原因；结果只用闭集结果码。
- 「按哈希绑定 payload」不等于把正文写入日志（报告 §4 同段）。
- `account_ref` 是不透明引用；凭据只在宿主侧安全存储（具体机制待 ADR，**未核实**现有凭据存储能否复用——COMM 的 PasswordStore 是 Hyphae 专用，见 `tasks.md` COMM-1b，未评估其通用性）。

### 3.3 存储与迁移

- 业务状态落在 **`office` 模块自己的库与 `data_dir`**（**不进内核 `agent24-store`**）。模块可以像 `rust/apps/agent24-documents/` 那样自带模块级 `migrations/`：其 schema、升级与重启恢复由模块自己负责（报告 §4 表后段；模块自带迁移的细节在本议题范围外，须在该模块自己的 ADR 里定）。
- 因此本议题**不引入内核 store 迁移**，也不触发「存储迁移」类必审。**若将来改主意把业务表放进内核**：`Store::open` 的前向迁移**不可按版本回退**（`K1-RELEASE-NOTES-20261016.md` 第 89–121 行），届时属「存储迁移」类，必须 PR-Daemon APPROVE（`AGENTS.md`）。

---

## 4. 关键机制设计

### 4.1 授权与批准绑定

- 每个有外部效果的 action 走内核审批门；批准绑定 **action_id + payload_version + effective_input_digest + exact_destination**。执行前复核四者，任一不一致 → 旧批准失效，重新询问（报告 §3 Approval 行；§5 第 5 条）。
- 现有 `Approval` 有 `payload`（自由 Map）、`available_decisions`、`standing_target`、`expires_at`，但**没有 payload 版本/摘要绑定字段**（`types.rs:514-540`，本次读过）。需要扩展：方案是在内核侧计算摘要并存入审批记录、在派发前比对，**不得**让业务层或 UI 自报摘要。属对外协议与审批语义变更 → PR-Daemon APPROVE。
- fail-closed：拒绝、过期、断连、daemon 重启、审批通道不可用一律等同 deny/abort（`types.rs` `Decision` 注释「Fail-closed: the implementation default (broken channel, cancelled run, daemon restart) is equivalent to abort/deny」）。
- 外发动作有效风险为 `External` 时，只出现 `approve` / `approve_for_target`（资格成立时）/ `deny` / `abort`，不出现 `approve_for_session`（L-APPR-4；`agent24-policy/src/lib.rs:56-68` `decisions_for`）。OA-M1/M2 **建议不提供** `approve_for_target`：报告要求首个 MVP 逐项确认（报告 §5 第 6 条）。
- 计划批准不是无限授权：plan mode 解锁工具集后，外部动作仍按风险、目的地、实际 payload 各自过门（报告 §3「Three points」第 1 条；plan mode 见 `types.rs` 第 390–394 行注释）。
- 「gate」与「advise」不混用：审批事件不等于已执行，不能据此显示「已发送」（报告 §3 第 2 条）。

### 4.2 幂等与对账

- **业务幂等键**由业务层为每个 action 生成（如 `workflow_id + action_id + payload_version` 的派生值），随请求传给支持幂等的服务；不支持的服务改为「写前查询 + 写后核验」。
- **不得**用 `tool_call_id` 做业务幂等键（ADR-K1-04 §3、§4 第 2 条明确禁止）。仓库中检索 `idempoten` 只命中内核自身操作的幂等注释，**没有连接器级业务幂等机制**（本次检索）。
- 结果未知（超时、断连、进程崩溃）→ 状态「结果待核验」→ 先对账查询（按幂等键或外部 ID）→ 再决定「已完成 / 需重试 / 需人工」。禁止整链盲重放（报告 §3 Retry 行；§6 负例第 3 行）。
- 重试只针对未完成步骤；成功步骤的回执保留（报告 §6 负例第 4 行）。

### 4.3 回执分层与「事件不等于送达」

`business_outcome` 闭集（建议）：`prepared`（本地产物就绪）→ `approved` → `dispatched`（工具已调用）→ `provider_accepted`（服务方受理，如 HTTP 2xx）→ `verified`（独立查询确认远端状态）；旁支：`unknown_needs_verification`、`failed`、`cancelled_before_dispatch`、`simulated_accepted`（mock 专用，永远不进入 `verified`）。

- 批准、工具返回、服务方受理、实际送达分开记录（报告 §3 Receipt 行）。邮件受理不代表收件人收到；LINE 的 HTTP 200 不代表最终送达（报告 §2 表；§5 LINE 段）。
- `verified` 必须来自对外部状态的独立查询，不能来自本地日志或审计（L-CTX-4）。
- WS 事件只是通知：`seq` 是**每连接单调**，不是跨重连的全局游标（`protocol/events.schema.json` 描述「per-connection monotonic seq」；报告 §5 第 8 条）。

### 4.4 取消与补偿的区别

- **取消**：停止未来工作；在可取消边界停止在途调用；列出已发生的外部效果。不声称撤回已发邮件或已写页面（报告 §3 Cancel 行）。
- **补偿**：针对已发生效果的新动作（如删除已建页面、发更正邮件），本身是一个新的 action，**需要独立授权**，不是取消的一部分（报告 §5 第 7 条）。
- run 终态（`Completed/Failed/Cancelled`）无出边；「部分完成/结果未知/停止中」是 workflow 层投影；重试创建关联的新 attempt/新 run，不复活终态 run（报告 §3 第 3 条；`RunStatus` 见 `types.rs:381-388`）。

### 4.5 断连与恢复

- 客户端重连后**先读权威快照**（workflow + action + receipt + 内核 run/approval 状态），再订阅事件；不得依据旧确认卡再次提交（报告 §6 负例第 6 行）。
- daemon 重启：内核恢复只允许从 `awaiting_approval` 且有未答工具调用的状态恢复（ADR-K1-02 §1 表「会话与重启恢复」行）；业务层对 `dispatched` 但无终态的 attempt 一律置 `unknown_needs_verification` 并走对账。
- 恢复时重验来源政策、授权期限与撤销状态；无法重验 → 不执行副作用（ADR-K1-02 §2.2 第 3 条）。

### 4.6 LocalOnly 与出站路径分别治理

| 出口 | 现状（K1 合入后） | OfficeAssistant 要求 |
|---|---|---|
| 模型（抽取/起草） | 主路径已收窄为 LocalOnly，无本地模型即 unavailable（K1 发布说明第 125–158 行） | OA-M1 只用本地模型；不可用即显式失败，不切云（报告 §4 第 3 段） |
| 连接器出站（邮件/Notion/LINE/HTTP/MCP） | ADR-K1-02 §2.4 第 2 条列为出口门；但统一出站判定（6b.3）未做，`http_fetch`/`shell_exec` 无 LocalOnly 判定（K1 发布说明第 33 行） | 连接器**不得**经 `shell_exec` 或通用 `http_fetch` 实现（报告 §5 第 3 条）；必须是专用工具并接入出站判定；在判定接线前，真实出站不可启用（计划 §4.2 依赖、§7 D7） |
| 模块模型回调 | 按 manifest `model_access`，`remote_allowed` → `Privacy::Any` | 若编排层做成模块，manifest 必须保持缺省 LocalOnly |
| 存储外流（日志/trace/诊断） | ADR-K1-02 §2.4 第 4 条 | artifact 正文不进日志/审计/遥测 |

- 用户输入来源标签固定为 `LocalOnly`（K1 发布说明第 31 行），由此派生的纪要继承该标签（ADR-K1-02 §2.1）。**把纪要发到外部服务，在现行契约下是 LocalOnly 资料的出口**，需要「资料 × 用途 × 目的地」的显式授权（ADR-K1-02 §2.3），而该授权的生产入口今天不存在。
- 本地推理不保证材料不出本机（报告 §5 第 4 条）；iDoris 未接、非 LLM 出口守卫未完成，**不得承诺端到端脱敏**。
- 资料中的指令只是数据，不扩大读取或发送范围（报告 §6 负例第 7 行；会话记忆召回已标注「记忆数据·非指令」，ADR-K1-02 §1 表记忆行）。

---

## 5. 与 K1 / 权限体系的关系

### 5.1 走既有门

| 需求 | 既有机制 | 位置 |
|---|---|---|
| 工具调用审批 | `ApprovalBroker` / `ApprovalGate`，fail-closed；决策集数据驱动 | `agent24-policy/src/lib.rs`；L-APPR-2/4 |
| 模块工具启用同意 | `StoreConsentGate::authorize`：查不到/失败/拒绝/过期/撤销/版本或指纹不符一律否决 | `agent24-policy/src/consent_gate.rs`（发布说明第 21 行） |
| 来源与模式 | `run_source_tags`；有效模式缺省/冲突/读失败均 LocalOnly | `agent24-store/src/source_policy.rs`（发布说明第 31 行） |
| 审计 | 模块工具 `PreDispatch`/`Terminal`；ADR-K1-04 字段约束 | `agent24-tools/src/lib.rs`（发布说明第 29 行） |
| UI 与 Agent 共用业务权威 | ADR-K1-04 §2.4：同一身份、资源、动作授权入口 | ADR-K1-04 |

### 5.2 需要扩展的契约（点名，不给实现细节）

1. **审批记录的 payload 版本/摘要绑定**：`agent24-protocol/src/types.rs`（`Approval`）、`agent24-policy`（派发前复核）、`protocol/openapi.yaml` 与 `events.schema.json`（对外协议）。
2. **模块 consent 的宿主授予入口**（若连接器以模块工具形式提供）：发布说明第 27、59 行——生产代码无授予路由；第一个加授予路径的 PR 必须同时处理 F1/F3/F6。这属 K1 线范围，OfficeAssistant 只消费，不在本议题内顺手实现。
3. **连接器出站判定接线**：ADR-K1-02 §6 的 6b.3（统一出站判定 API）；本议题的连接器是其消费者之一。
4. **「资料 × 用途 × 目的地」授权的产品入口**：ADR-K1-02 §2.3 / ADR-K1-03 决策 2；当前无 `CloudAuthorized` 生产写入点。需注意：外发到 Notion/邮件 ≠ 云模型处理，但两者在 ADR-K1-02 中走同一出口门；是否复用同一授权记录形状待 ADR。
5. **业务层新记录**：落在 `office` 模块自己的 `data_dir`（模块自带迁移，见 §0.4 D-A）；**不进 `agent24-store`**。
6. **法律**：有界委托（报告 §5）若需要比 `tool → 确切目标` 更宽或维度不同的常驻授权，或需要存储读授权，须先单独 PR 修改 `docs/laws/APPROVAL.md`（L-APPR-5/7），再实现（`../laws/README.md`）。

### 5.3 不得绕过

- 不得以「用户点了 UI 按钮」绕过资源/来源/组织策略（ADR-K1-04 §2.4）。
- `os.json enabled=true`、Creative capability、模块签名均不能替代模块授权或 LocalOnly（ADR-K1-03 反例第 8 条）。
- Skill 文本、MCP 描述、外部文档不能自我扩权（报告 §5 第 1 条）。
- 默认 legacy bearer 为全权，路由级资源授权未完成，Desktop 通用代理隔离未做（`COMPONENT-ROADMAP.md` C5 现状）——因此首版只验证可信本地材料与 mock，不开放给不可信插件或组织共享（报告 §5 第 2 条）。

---

## 6. 可观测性与测试

### 6.1 七项语义的前后端断言

| 语义 | 后端断言（自动化） | 前端断言（组件/E2E） |
|---|---|---|
| 计划 | 计划版本固定；新增范围/目的地/材料变化 → 计划版本递增且相关批准失效 | 显示将读材料、将产出物、哪些步骤写/发 |
| 动作 | 每步唯一 `action_id`；attempt 关联 run；自然语言不是执行凭据 | 确认卡展示账号、目的地、差异、影响范围 |
| 批准 | 批准绑定 payload 摘要；改 payload 后派发被拒；过期/拒绝/断连 → 无派发 | 只渲染服务端给的 `available_decisions`；无 `approve_for_session` |
| 取消 | 取消后无新派发；已发生效果在回执中列出 | 显示「停止中 → 已停止」与已完成动作清单 |
| 重试 | 结果未知 → 先对账；成功步不重做；同幂等键不产生第二次外部效果 | 显示可安全重试的步骤 |
| 状态 | 投影状态由内核快照计算；重连读快照 | 断连后不显示乐观成功 |
| 回执 | `provider_accepted` 与 `verified` 分离；mock 永不 `verified` | 「模拟」「结果待核验」标注可见 |

### 6.2 负例矩阵自动化

- 报告 §6 负例矩阵 14 行逐行映射为测试用例 ID（`OA-NEG-01..14`）；每行先在「旧行为/未实现」上证明失败，再实现，再证明通过（沿用 ADR-K1-03/04 反例方法）。
- 可自动化层次：Rust 单元/集成测试（审批绑定、幂等、恢复）；mock connector 注入故障（超时-远端成功、部分失败、token 过期）；桌面组件测试（标注、按钮集合）。
- 真实连接器负例（OA-M2）：用录制回放覆盖大部分；真实账号上只跑「一次写入 + 一次对账 + 一次撤销」，由 jason 执行或在场。
- 抽取质量：金标准逐项比对脚本，输出漏项、无依据承诺、待确认标注错误、出处错误四类计数与清单；安全/正确性反例单列，不进平均分（报告 §6 负例后段）。

### 6.3 演示与真实的区分

- action 记录 `simulated` 字段由宿主根据连接器类型设定，不可由业务层或模型改写；UI 与回执始终显示。
- 演示包明确标注样本为合成，不含真实客户材料、真实凭据或未授权账号（报告 §6 R0）。
- 本地演示不得描述为完成了 Notion 或邮件交付闭环（报告「How the original page plan…」第 4 段末句）。

---

## 7. 技术风险与替代方案

| 风险 / 选择 | 不这么做会怎样 | 降级 / 替代路径 |
|---|---|---|
| 业务编排层做薄、权威留内核 | 前端或业务层各自维护审批结论，出现两个权威，断连/重启后不一致，审计无法对应 | 若薄层不够，再评估进程外模块（经 os-proto）；LangGraph 仅在证明缺口后作为候选（报告 §4 表） |
| 批准绑定 payload 摘要 | 用户批准 A，实际发出 B（改收件人/附件）而无人察觉 | 内核扩展前，OA-M1 在业务层做「派发前比对 + 不一致即拒」，但标记为过渡，不对外宣称机制保障 |
| 业务幂等键 + 先查后做 | 超时重试产生重复邮件/重复页面 | 服务不支持幂等时：写前查询 + 写后核验；仍无法确定 → 停在「结果待核验」交人工 |
| 连接器不走 `shell_exec`/通用 `http_fetch` | 绕过模型与权限边界；cwd 固定不是 OS 沙箱（报告 §5 第 3 条） | 只用专用工具；未接出站判定前不启用真实出站 |
| 只用本地模型 | 敏感会议内容外流；违反 K1 LocalOnly | 抽取质量不足时：缩小任务（只抽行动项）、更多「待确认」、提高本地模型档；不回退云 |
| 正文与元数据分离 | 正文进入不可删除的哈希链审计（ADR-K1-04 §1 表「任意审计详情」行） | 审计只用闭集字段；正文仅在受控 artifact 存储 |
| 读授权不持久化（L-APPR-7） | 若持久化，授权面从「可解释的动作」扩散到「探索」 | 每次任务披露读取范围；若产品确需持久读授权，先单独修法并补机制 |
| 逐项确认（首版不做有界委托） | 若过早上委托，一次同意变成开放代理 | M3 先出设计 + 反例 + 法律提案，验收后再实现 |
| 迁移不可回退 | 发版后无法降级旧二进制 | 迁移 PR 小而独立；发布说明写清备份步骤（参照 K1 发布说明第 261 行） |
| 内核前置未就绪（授予入口、出站判定） | OA-M2 无法合规完成真实写入 | OA-M2 停在「只读 + 远端草稿评审 + mock 写入」；真实写入等前置（计划 §7 D7） |
| Workspace 同名混淆 | 权限、生命周期、状态机被误认为共享 | ID 与 UI 文案严格分开（报告 §1） |

---

## 8. 待 ADR 的决定

1. 业务编排层的位置（内核 crate vs 进程外模块）。
2. 四类记录的 schema、迁移、与主 store 的边界、重启恢复语义。
3. 审批记录的 payload 摘要绑定（协议变更）。
4. 连接器集成方式与凭据保管（OA-M2，依作者选定的连接器）。
5. 外发授权记录是否复用 ADR-K1-02 的「资料 × 用途 × 目的地」形状。
6. 有界委托的授权模型（OA-M3；须先过法律）。

## 9. 本文未核实

- 现有凭据存储机制能否通用于邮件/Notion 凭据。
- 桌面 i18n 的泰语覆盖度（只检索到泰语相关条目，未逐页核对）。
- 报告中关于 Components 仓库的事实（不在本仓库）。
- 外部平台（Notion、LINE、邮件服务）的 API 行为，以报告所引官方文档为准，本文未重新核对。
